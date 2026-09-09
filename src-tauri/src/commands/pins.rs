//! 首选渠道钉住的命令面：列、存（顺手写进内核 + 刷代理）、删（顺手从内核清掉）。
//!
//! 机制见 `services::pins`。这里管的是三处状态的一致：`pins.json`、内核渠道上的
//! 私有别名条目、代理内存里的规则表。顺序是**先内核、后落盘、再刷代理**：内核写
//! 不进去就不落盘 —— 否则代理会拿着一个内核不认的名字先发一次，开着退让是白挨一
//! 个 503 再重发，关着退让是每次都失败。

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::channel_writer::{patch_channel, remove_models};
use crate::services::pins::{pin_rules, pinned_alias, resolve_upstream, validate_pin, Pin, PinStore};
use crate::state::AppState;

pub(crate) fn store_path(state: &AppState) -> std::path::PathBuf {
    state.config_dir().join("pins.json")
}

/// 一次保存 / 删除的结果：新的全表 + 人话日志（写了哪个渠道、窗口怎么变了）。
#[derive(Debug, serde::Serialize)]
pub struct PinOutcome {
    pub pins: Vec<Pin>,
    pub log: Vec<String>,
}

/// 把磁盘上的钉住表装进代理。代理没起来就什么都不做（起来时 ensure_cli_proxy 会再调）。
pub(crate) async fn refresh_proxy_pins(state: &AppState) {
    let rules = match PinStore::load(&store_path(state)) {
        Ok(store) => pin_rules(&store),
        Err(e) => {
            tracing::warn!("pins: store unreadable, proxy runs without pins: {e}");
            return;
        }
    };
    if let Some(proxy) = state.cli_proxy.read().await.as_ref() {
        proxy.set_pins(rules).await;
    }
}

#[tauri::command]
pub async fn pin_list(state: State<'_, AppState>) -> AppResult<Vec<Pin>> {
    Ok(PinStore::load(&store_path(&state))?.pins)
}

/// 保存一条钉住：在每个首选渠道上写私有别名，旧规则里不再钉的渠道把条目清掉。
#[tauri::command]
pub async fn pin_save(state: State<'_, AppState>, pin: Pin) -> AppResult<PinOutcome> {
    validate_pin(&pin)?;
    let path = store_path(&state);
    let mut store = PinStore::load(&path)?;
    let previous = store.find(&pin.alias).cloned();

    let mut log = Vec::new();
    // 落点以渠道**现在**的基名条目为准，钉住表里的只是快照（见 resolve_upstream）。
    // 内核读不到时退回快照 —— 那种情况下面的 patch_channel 也会失败，不会写错东西。
    let mut pin = pin;
    if let Some(routes) = crate::commands::cli::fetch_kernel_routes(&state).await {
        let hits = routes.hits(&pin.alias);
        for t in pin.targets.iter_mut() {
            let (up, note) = resolve_upstream(&hits, t.channel_id, &t.upstream);
            if let Some(n) = note {
                log.push(format!("渠道 {}（{}）：{n}", t.channel_id, t.channel_name));
            }
            t.upstream = up;
        }
    }
    // 只切了退让开关、落点没变：不碰内核 —— 整体更新会让内核把该渠道的 key 全删再
    // 重建一遍，能省就省。落点和开关都没变的「原样再存一次」照常写：那是用户在
    // 内核后台误删了私有条目之后的修复路径。
    let toggle_only = previous.as_ref().is_some_and(|old| {
        old.fallback != pin.fallback
            && old.targets.len() == pin.targets.len()
            && old.targets.iter().zip(&pin.targets).all(|(a, b)| {
                a.channel_id == b.channel_id && a.upstream.trim() == b.upstream.trim()
            })
    });
    // 先写内核。任何一个渠道写不进去就整体失败、不落盘。
    for t in pin.targets.iter().filter(|_| !toggle_only) {
        let alias = pinned_alias(&pin.alias, t.channel_id);
        let patch = patch_channel(
            &state,
            t.channel_id,
            None,
            &[(alias.clone(), t.upstream.clone())],
        )
        .await?;
        log.push(format!(
            "渠道 {}（{}）：{alias} → {}",
            t.channel_id, patch.channel_name, t.upstream
        ));
    }
    // 旧规则里钉过、这次不钉了的渠道：清掉私有别名。清不掉只记日志 —— 那条名字没人会
    // 再发，留着无害；而钉住本身已经生效了。
    if let Some(old) = &previous {
        for t in old
            .targets
            .iter()
            .filter(|o| !pin.targets.iter().any(|n| n.channel_id == o.channel_id))
        {
            log.push(cleanup(&state, &old.alias, t.channel_id).await);
        }
    }

    store.upsert(pin);
    store.save(&path)?;
    refresh_proxy_pins(&state).await;
    // 落点变了：已接管 CLI 的窗口按新的口径重算（不退让时链上的备胎不再压窄它）。
    log.extend(crate::commands::cli::resync_windows(&state).await);
    Ok(PinOutcome {
        pins: store.pins,
        log,
    })
}

/// 删一条钉住：从内核清掉私有别名，落盘，刷代理。
#[tauri::command]
pub async fn pin_delete(state: State<'_, AppState>, alias: String) -> AppResult<PinOutcome> {
    let path = store_path(&state);
    let mut store = PinStore::load(&path)?;
    let Some(removed) = store.remove(&alias) else {
        return Err(AppError::Config(format!("没有「{alias}」的钉住")).into());
    };
    let mut log = Vec::new();
    for t in &removed.targets {
        log.push(cleanup(&state, &removed.alias, t.channel_id).await);
    }
    store.save(&path)?;
    refresh_proxy_pins(&state).await;
    log.extend(crate::commands::cli::resync_windows(&state).await);
    Ok(PinOutcome {
        pins: store.pins,
        log,
    })
}

/// 把 `pins.json` 里每一条的私有别名重新写回内核。
///
/// # 为什么需要它
///
/// 私有别名（`claude-opus-5@ch15`）活在渠道的 `models[]` 里，而那张表**会被别人
/// 整体重写**：「同步渠道模型清单」的覆盖档（`POST /admin/channels/models/refresh-batch`
/// 的 `replace`）按上游返回的清单收敛，上游当然不会返回我们编的 `@ch15`；
/// 有人在内核后台改渠道、导入 CSV 也一样。
///
/// 没了之后钉住不会报错，只会**每条请求先挨一个 503**：代理先发私有别名（内核：
/// 没有渠道服务它 → 503），再用原名重发才成功。实测日志里就是一对一对的
/// 「503 首选 / 200」，0ms、0 token、$0，纯浪费一个往返，而且把日志刷满红色。
///
/// 所以刷完模型清单要顺手补一次。补不回来的（渠道没了 / 权限不够）只记日志，
/// 钉住本身照旧退让，不该让整次操作失败。
#[tauri::command]
pub async fn pin_resync(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    let store = PinStore::load(&store_path(&state))?;
    if store.pins.is_empty() {
        return Ok(vec!["没有钉住的别名，无需修复。".into()]);
    }
    let mut log = Vec::new();
    let mut fixed = 0usize;
    for pin in &store.pins {
        for t in &pin.targets {
            let alias = pinned_alias(&pin.alias, t.channel_id);
            match patch_channel(
                &state,
                t.channel_id,
                None,
                &[(alias.clone(), t.upstream.clone())],
            )
            .await
            {
                Ok(p) => {
                    fixed += 1;
                    log.push(format!(
                        "渠道 {}（{}）：{alias} → {}",
                        t.channel_id, p.channel_name, t.upstream
                    ));
                }
                Err(e) => log.push(format!(
                    "渠道 {}（{}）：{alias} 没能写回（{e}）—— 这条钉住会继续走退让",
                    t.channel_id, t.channel_name
                )),
            }
        }
    }
    log.insert(0, format!("已把 {fixed} 条私有别名写回内核。"));
    refresh_proxy_pins(&state).await;
    Ok(log)
}

async fn cleanup(state: &AppState, alias: &str, channel_id: i64) -> String {
    let private = pinned_alias(alias, channel_id);
    match remove_models(state, channel_id, std::slice::from_ref(&private)).await {
        Ok(p) => format!("渠道 {channel_id}（{}）：已移除 {private}", p.channel_name),
        Err(e) => format!("渠道 {channel_id}：{private} 没能移除（{e}），留着无害"),
    }
}
