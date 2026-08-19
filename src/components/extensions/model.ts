/// 扩展管理的纯数据层：常量表 + 把「5 个 CLI 各自的清单」折叠成「一个扩展装
/// 在哪几个 CLI 上」的分组。没有 JSX，方便单独推理。

import type {
  CliTarget,
  ExtensionItem,
  ExtensionKind,
  ExtensionSupport,
  HookEvent,
} from "../../types";

// 目标名/标签是全应用共享的（模型导入、视觉 MCP 也要用），常量表已经搬到
// lib/targets.ts；这里转出去，扩展管理内部的 import 路径不变。
// `export … from` 只做转发、不进本模块作用域，所以下面还要各自 import 一次。
import { ALL_TARGETS, TARGET_LABELS } from "../../lib/targets";
export { ALL_TARGETS, TARGET_LABELS, TARGET_SHORT } from "../../lib/targets";

export const KIND_TABS: { id: ExtensionKind; label: string; hint: string }[] = [
  { id: "mcp", label: "MCP", hint: "外部工具服务器" },
  { id: "skill", label: "Skill", hint: "技能目录（SKILL.md）" },
  { id: "agent", label: "Agent", hint: "子代理定义（.md）" },
  { id: "hook", label: "Hook", hint: "生命周期钩子" },
];

export const KIND_LABELS: Record<ExtensionKind, string> = {
  mcp: "MCP 服务器",
  skill: "Skill",
  agent: "Agent",
  hook: "Hook",
};

/// 规范事件名。Rust 侧的 `HookEvent` 没有 rename_all，序列化就是变体名原样。
/// 某个 CLI 没有对应事件时后端会在写入阶段报错 —— 那一行 sync 失败，其余照常。
export const HOOK_EVENTS: HookEvent[] = [
  "PreToolUse",
  "PostToolUse",
  "UserPromptSubmit",
  "SessionStart",
  "SessionEnd",
  "Stop",
  "SubagentStop",
  "PreCompact",
  "Notification",
];

/// 一个扩展在全部 CLI 上的合并视图。`items` 按 ALL_TARGETS 顺序，`targets`
/// 是它的 target 投影，徽章直接用。
export type ExtensionGroup = {
  kind: ExtensionKind;
  id: string;
  label: string;
  description: string | null;
  items: ExtensionItem[];
  targets: CliTarget[];
};

/// 某一类扩展下，5 个 CLI 各自的支持情况（含不支持时该显示的原因）。
export type TargetSupport = {
  target: CliTarget;
  label: string;
  supported: boolean;
  /** 支持时写到哪（相对 home），用来告诉用户这次会动哪个文件。 */
  path: string | null;
};

/// 把支持矩阵切成「这一类扩展的 5 行」，顺序固定为 ALL_TARGETS。
/// 矩阵还没加载回来时给一份保守的占位：全部按不支持处理，宁可先置灰也不要让
/// 用户点到一个注定失败的目标。
export function targetSupportFor(
  rows: ExtensionSupport[] | undefined,
  kind: ExtensionKind,
): TargetSupport[] {
  return ALL_TARGETS.map((target) => {
    const row = rows?.find((r) => r.target === target && r.kind === kind);
    return {
      target,
      label: row?.label ?? TARGET_LABELS[target],
      supported: row?.supported ?? false,
      path: row?.path ?? null,
    };
  });
}

/// 按 id 折叠成分组。跨 CLI 的 id 是可比的：MCP 用服务器名、skill/agent 用
/// 目录或文件名、hook 用「规范事件 + 命令哈希」，都不含 CLI 自身的信息。
export function groupByExtension(
  items: ExtensionItem[],
  kind: ExtensionKind,
): ExtensionGroup[] {
  const byId = new Map<string, ExtensionGroup>();
  for (const item of items) {
    if (item.kind !== kind) continue;
    const existing = byId.get(item.id);
    if (existing) {
      existing.items.push(item);
      existing.targets.push(item.target);
      // 描述以第一个非空的为准：同一个扩展在各家的 frontmatter 可能有缺。
      existing.description ??= item.description;
      continue;
    }
    byId.set(item.id, {
      kind,
      id: item.id,
      label: item.label,
      description: item.description,
      items: [item],
      targets: [item.target],
    });
  }
  const groups = [...byId.values()];
  for (const g of groups) {
    g.items.sort((a, b) => ALL_TARGETS.indexOf(a.target) - ALL_TARGETS.indexOf(b.target));
    g.targets.sort((a, b) => ALL_TARGETS.indexOf(a) - ALL_TARGETS.indexOf(b));
  }
  // 字母序而不是「装的家数」降序：家数会随每次同步跳动，按名字排才找得到东西。
  return groups.sort((a, b) => a.id.localeCompare(b.id));
}

/// 搜索框的匹配：id / 显示名 / 描述任一命中即可，描述里常放的是 hook 的命令行。
export function matchesQuery(g: ExtensionGroup, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return (
    g.id.toLowerCase().includes(q) ||
    g.label.toLowerCase().includes(q) ||
    (g.description ?? "").toLowerCase().includes(q)
  );
}
