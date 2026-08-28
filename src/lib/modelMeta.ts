/// Heuristics for kernel model aliases: context-window presets and multimodal
/// capability. The kernel knows neither (a channel's upstream model string is
/// opaque to it), so the client carries best-effort defaults the user can
/// always override row by row in the import table.
///
/// 窗口值必须和 `src-tauri/src/services/context_window.rs` 的 `family_window`
/// 对齐 —— 调度图、导入表、CLI compact 挑模型，三处看见的是同一个数。改一处
/// 必须改另一处。

const CONTEXT_PRESETS: [RegExp, number][] = [
  // 更具体的规则必须排在更宽的前面，不然 grok-4.6 会先被 /grok/ 吃成 256k。
  [/grok-4[.-]?[56]/i, 500_000],
  [/grok/i, 256_000],
  [/deepseek-v[34]/i, 1_000_000],
  [/deepseek/i, 128_000],
  [/glm-5\.[23]/i, 1_000_000],
  [/glm/i, 200_000],
  [/kimi/i, 262_144],
  [/gemini/i, 1_000_000],
  [/gpt-4\.1/i, 1_000_000],
  [/gpt-5/i, 1_000_000],
  [/gpt-4o/i, 128_000],
  [/o[34]/, 200_000],
  [/qwen/i, 131_072],
  // Claude 4.6 起（含 opus-5 / fable-5 / sonnet-5）是 1M；haiku 和 4.5 仍是 200k。
  // 一刀切 200k 会让调度图把 opus-5 标成 200k，fallback 到 glm-5.3 时看不出两边都是 1M。
  [/haiku/i, 200_000],
  [/(claude|opus|sonnet|fable).*4[.-]5/i, 200_000],
  [/claude|opus|sonnet|fable/i, 1_000_000],
];

/// `[1m]` / `[500k]` 是上游挂在名字上的声明，比家族猜测准。
function suffixWindow(alias: string): number | null {
  const m = /\[(\d+(?:\.\d+)?)([mk]?)\]$/i.exec(alias.trim());
  if (!m) return null;
  const n = Number(m[1]);
  if (!(n > 0)) return null;
  const mul = m[2].toLowerCase() === "m" ? 1_000_000 : m[2].toLowerCase() === "k" ? 1_000 : 1;
  return Math.round(n * mul);
}

export function defaultContextWindow(alias: string): number {
  if (!alias.trim()) return 0;
  const fromSuffix = suffixWindow(alias);
  if (fromSuffix) return fromSuffix;
  for (const [re, n] of CONTEXT_PRESETS) {
    if (re.test(alias)) return n;
  }
  return 128_000;
}

/// 给人看的短标签：1M / 500k / 200k。格子里放不下「1000000」。
export function formatWindow(n: number): string {
  if (n <= 0) return "";
  if (n >= 1_000_000 && n % 1_000_000 === 0) return `${n / 1_000_000}M`;
  if (n >= 1_000 && n % 1_000 === 0) return `${n / 1_000}k`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}k`;
  return String(n);
}

/// Aliases we treat as able to see images. Anything not matching is assumed
/// text-only — which is exactly the case the vision MCP exists for. Match on
/// family names that only ship multimodal variants; vague families (kimi,
/// glm text line) are deliberately left out so we over-install rather than
/// silently assume vision support.
const VISION_PATTERNS: RegExp[] = [
  /claude/i,
  /gemini/i,
  /gpt-4o/i,
  /gpt-4\.1/i,
  /gpt-5/i,
  /o[34]/,
  /vl/i,
  /vision/i,
  /glm-4v/i,
  /grok-4/i,
  /pixtral/i,
];

export function isVisionCapable(alias: string): boolean {
  return VISION_PATTERNS.some((re) => re.test(alias));
}
