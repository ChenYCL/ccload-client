//! 出口别名的命令面：列、存（顺手刷代理改写表）、写进各 CLI。
//!
//! 机制见 `services::bridge`。这里管的是三处状态的一致：`bridge.json`、代理内存
//! 里的改写表、各 CLI 配置文件里的模型目录。
//!
//! 顺序是**先校验、再落盘、再刷代理**，最后才由用户显式点「写进 CLI」。分成两步
//! 而不是一次做完：落盘 + 刷代理是纯内存/本地的，随手可以撤；写 CLI 会改用户
//! home 下的真实配置（要快照、要原子写），那必须是一次明确的动作。
//!
//! Claude Code 多一层：每次读表都先和它磁盘上的 6 个槽位对齐（`load_synced`）。
//! 桥接表独占那 6 个槽位、写入会清掉表里空着的，所以磁盘上已有而表里没认领的
//! 必须先收进来 —— 否则用户在别处配好的槽位会在第一次写入时被抹掉。

use std::collections::BTreeSet;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::bridge::{
    guess_slots, seed, sync_entries, validate, BridgeEntry, BridgeStore, ClaudeSuffix, CLAUDE_TIERS,
};
use crate::services::claude_bridge::{self, PICKER_MIN_VERSION};
use crate::services::cli_backup::unique_stamp;
use crate::services::cli_types::{CliTarget, ConfigRoot};
use crate::services::context_window::ContextPolicy;
use crate::services::model_import::apply_import;
use crate::state::AppState;

pub(crate) fn store_path(state: &AppState) -> std::path::PathBuf {
    state.config_dir().join("bridge.json")
}

/// 保存的结果：新的全表 + 该让用户看见但不阻止保存的话。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeOutcome {
    pub entries: Vec<BridgeEntry>,
    pub claude_suffix: ClaudeSuffix,
    pub warnings: Vec<String>,
    pub log: Vec<String>,
}

/// 读表的结果。后缀跟行一起回来，否则界面上的 m/M 勾选每次刷新都会跳回默认。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeList {
    pub entries: Vec<BridgeEntry>,
    pub claude_suffix: ClaudeSuffix,
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

/// 读表，并和 Claude Code 磁盘上的槽位对齐；有改动就顺手存回去。
///
/// 对齐是幂等的：磁盘上的槽位只收表里没人认领、也没被用户清空过的。之所以存回去
/// 而不是只返回：`bridge_apply` 也走这里，不存的话「写入」看到的表和「列表」看到
/// 的会不一样。
async fn load_synced(state: &AppState) -> Result<BridgeStore, AppError> {
    let path = store_path(state);
    let mut store = BridgeStore::load(&path)?;
    let root = state.config_root().await?;
    if sync_entries(&mut store, &root) {
        store.save(&path)?;
        refresh_proxy_rewrites(state).await;
    }
    Ok(store)
}

#[tauri::command]
pub async fn bridge_list(state: State<'_, AppState>) -> AppResult<BridgeList> {
    let store = load_synced(&state).await?;
    Ok(BridgeList {
        entries: store.entries,
        claude_suffix: store.claude_suffix,
    })
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
    claude_suffix: Option<ClaudeSuffix>,
) -> AppResult<BridgeOutcome> {
    // 改名依赖代理，所以校验要知道代理开没开。取的是「写进 CLI 配置的地址是不是
    // 代理」这个开关，不是代理进程活着没有 —— 代理一直在跑，但 CLI 直连时它不在
    // 路径上。
    let proxy_on = state.settings.read().await.route_cli_through_proxy;

    // 对齐要在校验**之前**、对用户刚提交的这张表做，顺序不能反：
    //   1. 先记墓碑 —— 表里空着、磁盘上还有值的槽位是用户刚清掉的，不记下来，
    //      下一步对齐（以及以后每次读表）会把磁盘旧值收回来，「清空槽位」永远
    //      无法生效；
    //   2. 再收磁盘上真正没人认领的槽位（用户在别处配好的 fable / haiku）。
    let prev = BridgeStore::load(&store_path(&state)).unwrap_or_else(|_| BridgeStore::default());
    let mut store = BridgeStore {
        entries,
        cleared_slots: Default::default(),
        claude_suffix: claude_suffix.unwrap_or(prev.claude_suffix),
    };
    let root = state.config_root().await?;
    let disk = claude_bridge::slots_on_disk(&root);
    store.cleared_slots.extend(
        CLAUDE_TIERS
            .iter()
            .filter(|s| {
                disk.get(**s).is_some_and(|v| !v.trim().is_empty())
                    && !store.entries.iter().any(|e| e.has_slot(s))
            })
            .map(|s| s.to_string()),
    );
    sync_entries(&mut store, &root);
    let mut warnings = validate(&store.entries, proxy_on)?;

    // 落点在内核里不存在的行，写进 CLI 配置就是一个选中即 404 的死名字。拦不住
    // ——「先配表、后建渠道」是合法顺序，内核离线时也查不了 —— 但保存时必须说
    // 一声，别让人到日志里找原因。桥接页那批 `claude-fa` / `claude-fabl` 前缀垃圾
    // 就是因为没人说，静默攒了 13 行。
    if let Some(routes) = crate::commands::cli::fetch_kernel_routes(&state).await {
        let missing: Vec<&str> = store
            .entries
            .iter()
            .filter(|e| {
                let upstream = e.upstream_alias();
                !upstream.is_empty() && routes.hits(upstream).is_empty()
            })
            .map(|e| e.upstream_alias())
            .collect();
        if !missing.is_empty() {
            let mut uniq = missing;
            uniq.sort();
            uniq.dedup();
            let shown: Vec<&str> = uniq.iter().take(5).copied().collect();
            warnings.push(format!(
                "有 {} 个落点在内核渠道里不存在（{}{}），写进 CLI 后选中它会 404 —— 先去内核后台建渠道，或把落点改成真实存在的别名。",
                uniq.len(),
                shown.join("、"),
                if uniq.len() > 5 { "…" } else { "" },
            ));
        }
    }

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
        claude_suffix: store.claude_suffix,
        warnings,
        log,
    })
}

/// Claude Code 那一家：槽位 + modelPicker 一起写，文案把三个数都说出来 ——
/// 「写了 4 个槽位」和「清掉了 haiku」是两件事，用户得分得清。
async fn write_claude(
    root: &ConfigRoot,
    store: &BridgeStore,
    policy: &ContextPolicy,
    state: &AppState,
) -> BridgeWrite {
    let target = CliTarget::ClaudeCode;
    let picked = store
        .entries
        .iter()
        .any(|e| e.targets.contains(&target) && !e.alias.trim().is_empty());
    if !picked {
        return BridgeWrite {
            target,
            status: "skipped".into(),
            text: "这一家一条都没勾，配置未改动".into(),
        };
    }
    match claude_bridge::write(
        root,
        &store.entries,
        policy,
        store.claude_suffix,
        &unique_stamp(),
        &state.backups,
    ) {
        Ok(r) => {
            // 写入已把磁盘上的旧值清掉，对应的墓碑随之作废。顺手存回去，别等下次
            // 读表时才被 retain 掉。
            if !r.cleared.is_empty() {
                let mut persisted = store.clone();
                persisted.cleared_slots.retain(|s| !r.cleared.contains(s));
                if let Err(e) = persisted.save(&store_path(state)) {
                    tracing::warn!("bridge: cleared tombstones not persisted: {e}");
                }
            }
            let mut text = format!("{}（槽位 {} 个", r.written.join("、"), r.slots);
            if !r.cleared.is_empty() {
                text.push_str(&format!("，清掉 {}", r.cleared.join(" / ")));
            }
            if r.picker > 0 {
                text.push_str(&format!(
                    "，/model 列表 {} 行 —— 需要 Claude Code ≥ {PICKER_MIN_VERSION}",
                    r.picker
                ));
            }
            text.push('）');
            BridgeWrite {
                target,
                status: "ok".into(),
                text,
            }
        }
        Err(e) => BridgeWrite {
            target,
            status: "failed".into(),
            text: e.to_string(),
        },
    }
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
    let store = load_synced(&state).await?;
    let root = state.config_root().await?;
    let policy = state.settings.read().await.context_policy.clone();
    let prune = prune.unwrap_or(false);

    let mut out = Vec::new();
    for target in targets {
        if target == CliTarget::ClaudeCode {
            out.push(write_claude(&root, &store, &policy, &state).await);
            continue;
        }
        let entries = store.import_entries(target, &policy);
        if entries.is_empty() {
            out.push(BridgeWrite {
                target,
                status: "skipped".into(),
                text: "这一家一条都没勾，配置未改动".into(),
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
    let mut store = load_synced(&state).await?;
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

#[cfg(test)]
mod tests {
    /// 落点在内核渠道里不存在的行要出警告：写进 CLI 配置的就是一个选中即 404 的
    /// 死名字。桥接页那批 `claude-fa` / `claude-fabl` 前缀垃圾就是因为没人说，
    /// 静默攒了十几行才被用户在 Grok 的模型列表里看见。
    #[test]
    fn a_target_the_kernel_doesnt_serve_is_warned_about() {
        let json = serde_json::json!([
            {"id": 1, "enabled": true, "models": [{"model": "grok-4.6"}]},
            {"id": 2, "enabled": false, "models": [{"model": "retired-alias"}]},
        ]);
        let routes = crate::services::context_floor::KernelRoutes::parse(&json);
        let served = |a: &str| !routes.hits(a).is_empty();
        assert!(served("grok-4.6"));
        assert!(!served("claude-fabl"), "内核没这个前缀名");
        assert!(!served("retired-alias"), "停用渠道不算在服务");
    }
}
