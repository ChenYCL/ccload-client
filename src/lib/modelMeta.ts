/// Heuristics for kernel model aliases: context-window presets and multimodal
/// capability. The kernel knows neither (a channel's upstream model string is
/// opaque to it), so the client carries best-effort defaults the user can
/// always override row by row in the import table.

const CONTEXT_PRESETS: [RegExp, number][] = [
  [/kimi/i, 262_144],
  [/claude/i, 200_000],
  [/gpt-5/i, 400_000],
  [/gpt-4\.1/i, 1_000_000],
  [/gpt-4o/i, 128_000],
  [/o[34]/, 200_000],
  [/gemini/i, 1_000_000],
  [/glm/i, 200_000],
  [/deepseek-v[34]/i, 1_000_000],
  [/deepseek/i, 128_000],
  [/qwen/i, 131_072],
  [/grok-4\.[56]/i, 500_000],
  [/grok/i, 256_000],
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
  const fromSuffix = suffixWindow(alias);
  if (fromSuffix) return fromSuffix;
  for (const [re, n] of CONTEXT_PRESETS) {
    if (re.test(alias)) return n;
  }
  return 128_000;
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
