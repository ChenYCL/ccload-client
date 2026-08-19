//! 调度图命令。存本地、校验、应用到内核渠道与 CLI 配置。

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::channel_writer::patch_channel;
use crate::services::graph::{
    preview as preview_tier, validate as validate_doc, GraphDoc, GraphStore, GraphValidation,
    PreviewStep,
};
use crate::state::AppState;

fn store_path(state: &AppState) -> std::path::PathBuf {
    state.config_dir().join("graphs.json")
}

#[tauri::command]
pub async fn graph_list(state: State<'_, AppState>) -> AppResult<Vec<GraphDoc>> {
    Ok(GraphStore::load(&store_path(&state))?.graphs)
}

#[tauri::command]
pub async fn graph_save(state: State<'_, AppState>, doc: GraphDoc) -> AppResult<Vec<GraphDoc>> {
    let path = store_path(&state);
    let mut store = GraphStore::load(&path)?;
    store.upsert(doc);
    store.save(&path)?;
    Ok(store.graphs)
}

#[tauri::command]
pub fn graph_validate(doc: GraphDoc) -> AppResult<GraphValidation> {
    Ok(validate_doc(&doc))
}

#[tauri::command]
pub fn graph_preview(doc: GraphDoc, tier: String) -> AppResult<Vec<PreviewStep>> {
    Ok(preview_tier(&doc, &tier))
}

/// 把一张图落到内核里。
///
/// 对每个参与的 provider 做一次渠道更新：
///   * `models[]` 里 upsert 它在各档的 `别名 → 真实模型`
///   * 渠道 `priority` 设成校验算出的全局顺序
///
/// 校验不通过就**不写任何东西**（PRD §11.2 的 fail-fast）。写一半再报错会留下
/// 一个半配好的状态，比不写更难收拾。
#[tauri::command]
pub async fn graph_apply(state: State<'_, AppState>, id: String) -> AppResult<Vec<String>> {
    let store = GraphStore::load(&store_path(&state))?;
    let doc = store
        .graphs
        .iter()
        .find(|g| g.id == id)
        .ok_or_else(|| AppError::Config(format!("没有名为 {id} 的调度图")))?
        .clone();

    let v = validate_doc(&doc);
    if !v.ok {
        return Err(AppError::Config(format!(
            "校验未通过，未做任何写入：\n{}",
            v.problems.join("\n")
        ))
        .into());
    }

    // provider → 它要承担的 (别名, 上游模型) 列表。一个 provider 可能同时出现在
    // 好几档里（例如 claude 同时在 daily/deep/flagship），一次写完，别为同一个
    // 渠道发多次 PUT。
    let mut per_provider: Vec<(&str, Vec<(String, String)>)> = Vec::new();
    for tier in &doc.tiers {
        for pid in &tier.providers {
            let Some(p) = doc.providers.iter().find(|p| &p.id == pid) else {
                continue;
            };
            let Some(model) = p.models.get(&tier.id) else {
                continue;
            };
            let entry = (tier.alias.clone(), model.clone());
            match per_provider.iter_mut().find(|(id, _)| *id == pid.as_str()) {
                Some((_, list)) => list.push(entry),
                None => per_provider.push((pid.as_str(), vec![entry])),
            }
        }
    }

    let mut log = Vec::new();
    for (pid, entries) in per_provider {
        let p = doc
            .providers
            .iter()
            .find(|p| p.id == pid)
            .expect("validated above");
        let channel_id = p.channel_id.expect("validated above");
        let priority = v.priorities.get(pid).copied();

        let out = patch_channel(&state, channel_id, priority, &entries).await?;
        let mapped = entries
            .iter()
            .map(|(a, m)| format!("{a}→{m}"))
            .collect::<Vec<_>>()
            .join("、");
        log.push(format!(
            "{}（渠道 {} #{channel_id}）优先级 {}→{}：{mapped}",
            p.label,
            out.channel_name,
            out.old_priority
                .map(|x| x.to_string())
                .unwrap_or_else(|| "?".into()),
            priority
                .map(|x| x.to_string())
                .unwrap_or_else(|| "不变".into()),
        ));
    }
    log.push(
        "提示：优先级是渠道级属性，会影响这些渠道服务的所有模型；\
         另外调度图与「模型链」都会改优先级，同时用会互相覆盖。"
            .into(),
    );
    Ok(log)
}
