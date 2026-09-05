import type { Envelope } from "../types";
import { splitPinned } from "./pins";

/// 「填模型名」的候选来源。
///
/// # 两种模型名，别搞混
///
/// 界面上有两类填模型的位置，看起来一样，要的东西**正好相反**：
///
/// | 位置 | 填的是 | 候选应该是 |
/// | --- | --- | --- |
/// | 模型链的每一层、调度图的 provider × 档位 | **上游真实模型名** | 该渠道 `redirect_model \|\| model` |
/// | CLI 接管的 tier 槽位、fallbackModel、Codex `model` | **CLI 请求用的别名** | 全部启用渠道的 `models[].model` |
///
/// 内核里 `ModelEntry` 是 `{model, redirect_model}`：`model` 是别名（CLI 写什么、
/// 内核按什么选渠道），`redirect_model` 是真正发给上游的名字。
///
/// 取反了的后果不会当场报错 —— 写进 CLI 的别名上游不认识，或者写进渠道的上游名
/// 内核匹配不到，都要等真正发请求那一刻才炸。所以这两个函数分开命名、各自带注释，
/// 不合并成一个「取模型名」。

/// `GET /admin/channels` 里和模型有关的那几个字段。
export type ChannelModels = {
  id?: number;
  enabled?: boolean;
  models?: { model?: string; redirect_model?: string; disabled?: boolean }[];
};

/// **上游真实模型名**：发给这个渠道的上游时用的名字。
///
/// `redirect_model` 非空时它才是真名，否则别名自己就是。`disabled` 的条目排除
/// —— 内核当它不存在，列出来只会是个点了就失败的选项。
export function upstreamModelsOf(channel: ChannelModels | undefined): string[] {
  return dedupe(
    (channel?.models ?? [])
      .filter((m) => !m.disabled)
      .map((m) => m.redirect_model?.trim() || m.model?.trim() || ""),
  );
}

/// **CLI 侧别名**：所有启用渠道对外提供的模型名的并集。
///
/// 只看启用的渠道：停用的渠道内核根本不会选，把它的别名建议给用户等于给一堆
/// 填了就 404 的名字（和模型导入页那条一样的理由）。首选渠道钉住写进内核的私有
/// 别名（`grok-4.6@ch21`）也不列 —— 那是代理内部用的名字，CLI 配置里该写原名。
export function kernelAliases(channels: ChannelModels[] | undefined): string[] {
  return dedupe(
    (channels ?? [])
      .filter((c) => c.enabled !== false)
      .flatMap((c) => (c.models ?? []).filter((m) => !m.disabled).map((m) => m.model?.trim() || ""))
      .filter((name) => !splitPinned(name)),
  ).sort();
}

function dedupe(xs: string[]): string[] {
  return [...new Set(xs.filter(Boolean))];
}

/// `GET /admin/channels` 的响应解包。两个页面都要用，写一次。
export function channelsOf(env: Envelope<ChannelModels[]> | undefined): ChannelModels[] {
  return env?.data ?? [];
}
