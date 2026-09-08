//! 出口别名的命令面：列、存（顺手刷代理改写表）、写进各 CLI。
//!
//! 机制见 `services::bridge`。这里管的是三处状态的一致：`bridge.json`、代理内存
//! 里的改写表、各 CLI 配置文件里的模型目录。
//!
//! 顺序是**先校验、再落盘、再刷代理**，最后才由用户显式点「写进 CLI」。分成两步
//! 而不是一次做完：落盘 + 刷代理是纯内存/本地的，随手可以撤；写 CLI 会改用户
//! home 下的真实配置（要快照、要原子写），那必须是一次明确的动作。

use std::collections::BTreeSet;

use tauri::State;

use crate::error::AppResult;
use crate::services::bridge::{guess_slots, seed, validate, BridgeEntry, BridgeStore};
use crate::services::cli_backup::unique_stamp;
use crate::services::cli_types::CliTarget;
use crate::services::model_import::apply_import;
use crate::state::AppState;

pub(crate) fn store_path(state: &AppState) -> std::path::PathBuf {
    state.config_dir().join("bridge.json")
}

/// 保存的结果：新的全表 + 该让用户看见但不阻止保存的话。
#[derive(Debug, serde::Serialize)]
pub struct BridgeOutcome {
    pub entries: Vec<BridgeEntry>,
    pub warnings: Vec<String>,
    pub log: Vec<String>,
}

/// 写进一家 CLI 的结果。逐家独立成败 —— 一家没接管不该拖垮其余四家。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeWrite {
    pub target: CliTarget,
    /// `ok` / `skipped` / `failed`。skipped 必须和 ok 分开：跳过等于**没写**，
    /// 混进 ok 再靠文案解释，用户没法判断到底写没写。
    pub status: String,
    pub text: String,
}

/// 把磁盘上的出口别名表装进代理的改写表。
///
/// 代理没起来就什么都不做 —— `ensure_cli_proxy` 起完会再调一次。表坏了只记
/// warn：代理照常转发，只是没有改写（同名记录本来就不需要改写，改名的那些会
/// 落到内核的 404，用户在日志里看得见）。
pub(crate) async fn refresh_proxy_rewrites(state: &AppState) {
    let rewrites = match BridgeStore::load(&store_path(state)) {
        Ok(store) => store.rewrites(),
        Err(e) => {
            tracing::warn!("bridge: store unreadable, proxy runs without rewrites: {e}");
            return;
        }
    };
    if let Some(proxy) = state.cli_proxy.read().await.as_ref() {
        proxy.set_rewrites(rewrites).await;
    }
}

#[tauri::command]
pub async fn bridge_list(state: State<'_, AppState>) -> AppResult<Vec<BridgeEntry>> {
    Ok(BridgeStore::load(&store_path(&state))?.entries)
}

/// 存整张表。
///
/// 整表替换而不是逐条 upsert：这张表就是一个界面上的表格，用户的心智模型是
/// 「我编辑完这一屏，保存」。逐条接口会让「删掉一行」变成一个额外的命令，而且
/// 两端的顺序要另外同步。
#[tauri::command]
pub async fn bridge_save(
    state: State<'_, AppState>,
    entries: Vec<BridgeEntry>,
) -> AppResult<BridgeOutcome> {
    // 改名依赖代理，所以校验要知道代理开没开。取的是「写进 CLI 配置的地址是不是
    // 代理」这个开关，不是代理进程活着没有 —— 代理一直在跑，但 CLI 直连时它不在
    // 路径上。
    let proxy_on = state.settings.read().await.route_cli_through_proxy;
    let warnings = validate(&entries, proxy_on)?;

    let store = BridgeStore { entries };
    store.save(&store_path(&state))?;
    refresh_proxy_rewrites(&state).await;

    let renamed = store.rewrites().len();
    let mut log = vec![format!(
        "已保存 {} 条出口别名，其中 {renamed} 条改名（代理转发前替换成落点名）。",
        store.entries.len()
    )];
    if renamed == 0 {
        log.push("没有改名记录，所以这张表不依赖本地代理。".into());
    }
    Ok(BridgeOutcome {
        entries: store.entries,
        warnings,
        log,
    })
}

/// 按当前这张表，把每家 CLI 该有的那些别名写进它自己的配置。
///
/// 逐家串行。并行会同时改 `backups/manifest.json`，短写入叠在旧文件尾巴上 ——
/// 那个坏法已经出现过一次（`trailing characters at line 46`，装和卸全卡死）。
#[tauri::command]
pub async fn bridge_apply(
    state: State<'_, AppState>,
    targets: Vec<CliTarget>,
    prune: Option<bool>,
) -> AppResult<Vec<BridgeWrite>> {
    let store = BridgeStore::load(&store_path(&state))?;
    let root = state.config_root().await?;
    let policy = state.settings.read().await.context_policy.clone();
    let prune = prune.unwrap_or(false);

    let mut out = Vec::new();
    for target in targets {
        let entries = store.import_entries(target, &policy);
        if entries.is_empty() {
            out.push(BridgeWrite {
                target,
                status: "skipped".into(),
                text: "这一家一条都没勾，配置未改动".into(),
            });
            continue;
        }
        // Claude Code 没有目录文件，只认槽位。一条槽位都没绑时后端会整次失败，
        // 在这里先说清楚，免得混选时一家的报错看起来像全都没写。
        if target == CliTarget::ClaudeCode && entries.iter().all(|e| e.tier.is_none()) {
            out.push(BridgeWrite {
                target,
                status: "skipped".into(),
                text: "勾了 Claude Code 的行都没选槽位（它没有模型目录，只有 5 个槽位），配置未改动".into(),
            });
            continue;
        }
        match apply_import(
            &root,
            target,
            &entries,
            &unique_stamp(),
            &state.backups,
            prune,
            Some(policy.percent()),
        ) {
            Ok(r) => {
                let mut text = r.written.join("、");
                if !r.skipped.is_empty() {
                    text.push_str(&format!("（{} 个没选槽位，未写入）", r.skipped.len()));
                }
                if !r.removed.is_empty() {
                    text.push_str(&format!("（清掉 {} 个旧别名）", r.removed.len()));
                }
                out.push(BridgeWrite {
                    target,
                    status: "ok".into(),
                    text,
                });
            }
            Err(e) => out.push(BridgeWrite {
                target,
                status: "failed".into(),
                text: e.to_string(),
            }),
        }
    }
    Ok(out)
}

/// 「填充默认值」：给内核现有的别名铺一份同名的表。
///
/// 已经存在的记录**原样保留** —— 用户手调过窗口 / 改过名 / 绑过槽位的行不能被
/// 一次「填充」抹掉。只补内核里有、表里还没有的那些。
#[tauri::command]
pub async fn bridge_seed(
    state: State<'_, AppState>,
    aliases: Vec<String>,
    targets: Vec<CliTarget>,
    guess_claude_slots: Option<bool>,
) -> AppResult<Vec<BridgeEntry>> {
    let mut store = BridgeStore::load(&store_path(&state))?;
    let want: BTreeSet<CliTarget> = targets.into_iter().collect();
    let have: BTreeSet<String> = store
        .entries
        .iter()
        .map(|e| crate::services::context_floor::alias_key(&e.alias))
        .collect();
    let fresh: Vec<String> = aliases
        .into_iter()
        .filter(|a| !have.contains(&crate::services::context_floor::alias_key(a)))
        .collect();
    store.entries.extend(seed(&fresh, &want));
    if guess_claude_slots.unwrap_or(false) {
        guess_slots(&mut store.entries);
    }
    Ok(store.entries)
}
