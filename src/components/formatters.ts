/// 总览页和实时日志页共用的格式化口径。放一处是为了两边不会漂移 —— 同一个
/// 数字在趋势图 tooltip 和日志表里必须长得一样，否则用户会以为看到了两份数据。

/** 整数千分位。缺失值一律显示 em dash，不显示 0，避免把「没数据」读成「零」。 */
export function fmtInt(n?: number | null): string {
  if (n == null || !Number.isFinite(n)) return "—";
  return Math.round(n).toLocaleString("en-US");
}

/** token 列很窄，用紧凑单位：12.3k / 4.5M。0 就是 0，不是缺失。 */
export function fmtCompact(n?: number | null): string {
  if (n == null || !Number.isFinite(n)) return "—";
  const abs = Math.abs(n);
  if (abs >= 1e9) return `${(n / 1e9).toFixed(1)}B`;
  if (abs >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (abs >= 1e4) return `${(n / 1e3).toFixed(1)}k`;
  return Math.round(n).toLocaleString("en-US");
}

/** 费用按量级换精度：小额要看得见，大额不需要 4 位小数的噪声。 */
export function fmtCost(n?: number | null): string {
  if (n == null || !Number.isFinite(n)) return "—";
  if (n === 0) return "$0";
  if (Math.abs(n) < 0.01) return `$${n.toFixed(4)}`;
  if (Math.abs(n) < 1) return `$${n.toFixed(3)}`;
  return `$${n.toFixed(2)}`;
}

/** duration / first_byte_time 是秒（浮点）。 */
export function fmtDuration(sec?: number | null): string {
  if (sec == null || !Number.isFinite(sec)) return "—";
  if (sec < 1) return `${Math.round(sec * 1000)}ms`;
  if (sec < 60) return `${sec.toFixed(1)}s`;
  // 先把总秒数取整再拆分。先拆后取整的话 119.7s 会算出 1m + round(59.7)=60
  // → "1m60s"，一个不存在的时间。
  const total = Math.round(sec);
  return `${Math.floor(total / 60)}m${String(total % 60).padStart(2, "0")}s`;
}

/** 入参是 0–1 的比率。 */
export function fmtPct(rate?: number | null, digits = 1): string {
  if (rate == null || !Number.isFinite(rate)) return "—";
  return `${(rate * 100).toFixed(digits)}%`;
}

/** unix 秒 → 本地 HH:MM:SS。 */
export function fmtClock(unixSec: number): string {
  return new Date(unixSec * 1000).toLocaleTimeString("zh-CN", { hour12: false });
}

/** RFC3339 → 本地 HH:MM。趋势图的 x 轴刻度。 */
export function fmtHm(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

export type Tone = "ok" | "warn" | "bad" | "idle";

/** 状态码分档：2xx 绿、4xx 琥珀、5xx 红，其余（含 0/未知）灰。 */
export function statusTone(code?: number | null): Tone {
  if (code == null || !Number.isFinite(code) || code === 0) return "idle";
  if (code < 400) return "ok";
  if (code < 500) return "warn";
  return "bad";
}

/** 成功率分档。99% 以上才算健康 —— 代理层的失败是要被看见的。 */
export function rateTone(rate: number, sample = 1): Tone {
  if (sample <= 0 || !Number.isFinite(rate) || rate < 0) return "idle";
  if (rate >= 0.99) return "ok";
  if (rate >= 0.9) return "warn";
  return "bad";
}

/** 文本色。分档到颜色只在这里做一次，页面里不写死颜色。 */
export const TONE_TEXT: Record<Tone, string> = {
  ok: "text-emerald-600",
  warn: "text-amber-600",
  bad: "text-red-600",
  idle: "text-muted",
};

/** 填充色（进度条、健康格）。 */
export const TONE_FILL: Record<Tone, string> = {
  ok: "bg-emerald-500",
  warn: "bg-amber-500",
  bad: "bg-red-500",
  idle: "bg-border",
};

/** 徽标：底色 + 文字，用于状态码这种要一眼扫到的标记。 */
export const TONE_BADGE: Record<Tone, string> = {
  ok: "bg-emerald-500/12 text-emerald-700",
  warn: "bg-amber-500/14 text-amber-700",
  bad: "bg-red-500/12 text-red-700",
  idle: "bg-surface-2 text-muted",
};

/// 一条日志真正要付的钱。
///
/// `logs.cost` 是**标准成本**（内核 model/log.go:98 的注释就是这么写的），倍率
/// 单独存在 `cost_multiplier`；总览卡片用的 `effective_cost` 则是内核算好的
/// `cost * cost_multiplier`。两边都叫「费用」却是两个数，同一条请求在日志里
/// 显示 $0.10、在总览里显示 $0.30，用户会以为看到了两份数据。
/// 统一以「乘过倍率」为准，倍率本身在详情里单列留痕。
export function effectiveCost(log: {
  cost?: number;
  cost_multiplier?: number;
}): number | undefined {
  if (log.cost == null) return undefined;
  const m = log.cost_multiplier;
  // 倍率缺失当 1；负数是脏数据，内核自己也钳到 1（proxy_error.go:372）。
  return log.cost * (m == null || m < 0 ? 1 : m);
}
