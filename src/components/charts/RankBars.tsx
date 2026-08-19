import { cn } from "../../lib/cn";
import { TONE_FILL, type Tone } from "../formatters";

/// 排行条：一行一个条目，条长 = value / max。用纯 div 宽度百分比画，不用 SVG ——
/// 横向条形图没有曲线也没有坐标轴，SVG 只会让文字排版变难。
///
/// 刻意不做「其他」聚合桶：把尾部塞进一个灰条会让用户以为那是一个真实模型。
/// 需要看全部就翻下面的明细表。

export type RankItem = {
  key: string;
  label: string;
  /** 决定条长的数值，必须非负 */
  value: number;
  /** 条右侧显示的主数值文本，由调用方格式化，避免这里猜单位 */
  valueText: string;
  /** 标签下方的一行补充信息 */
  sub?: string;
  tone?: Tone;
};

export function RankBars({ items, max }: { items: RankItem[]; max?: number }) {
  // 上限用传入值或当前最大值。传入 max 是为了让两组条能共用同一把尺子。
  const top = Math.max(max ?? 0, ...items.map((i) => i.value), Number.MIN_VALUE);

  return (
    <ul className="space-y-2.5">
      {items.map((item) => (
        <li key={item.key}>
          <div className="flex items-baseline justify-between gap-3">
            <span className="truncate font-mono text-xs" title={item.label}>
              {item.label}
            </span>
            <span className="shrink-0 text-xs font-medium tabular-nums">{item.valueText}</span>
          </div>
          <div className="mt-1 flex items-center gap-2">
            <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-surface-2">
              <div
                className={cn("h-full rounded-full", TONE_FILL[item.tone ?? "ok"])}
                // 0 值也留 2% 的可见宽度，否则「有这个模型但没花钱」会看起来像没这一行。
                style={{ width: `${Math.max(2, (item.value / top) * 100)}%` }}
              />
            </div>
            {item.sub && (
              <span className="shrink-0 text-[11px] tabular-nums text-muted">{item.sub}</span>
            )}
          </div>
        </li>
      ))}
    </ul>
  );
}
