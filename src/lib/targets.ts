/// 五个 CLI 目标的公共常量表。
///
/// 放 lib 而不是 components/extensions 下：模型导入、视觉 MCP、CLI 接管、扩展管理
/// 都要用同一份名字，之前每个页面各写一串三元表达式，加第四个目标时必然漏。
///
/// 字面量必须与后端 `CliTarget` 的 serde 形式逐字一致（见 cli_types.rs）。对不上
/// 时 Tauri 会报 `unknown variant`，而且只在点到那个按钮时才暴露 —— OpenCode 就
/// 因为后端 kebab-case 出 `open-code` 而一直是坏的。

import type { CliTarget } from "../types";

/// 顺序与后端 `cli_extensions::ALL_TARGETS` 一致 —— sync 省略 source 时正是按
/// 这个顺序找第一个装了该扩展的 CLI 当来源，UI 里的排序跟着它才不会误导。
export const ALL_TARGETS: CliTarget[] = [
  "claude-code",
  "codex",
  "gemini-cli",
  "grok-build",
  "opencode",
];

export const TARGET_LABELS: Record<CliTarget, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
  "gemini-cli": "Gemini CLI",
  "grok-build": "Grok Build",
  opencode: "OpenCode",
};

/// 五个徽章并排时全名放不下，列表里用短名，详情里才用全名。
export const TARGET_SHORT: Record<CliTarget, string> = {
  "claude-code": "Claude",
  codex: "Codex",
  "gemini-cli": "Gemini",
  "grok-build": "Grok",
  opencode: "OpenCode",
};
