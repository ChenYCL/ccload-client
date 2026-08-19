import { fmtCompact, fmtCost, fmtDuration, fmtInt, fmtPct, rateTone, TONE_TEXT } from "../formatters";
import { RankBars, type RankItem } from "../charts/RankBars";
import type { ModelRow } from "./derive";

/// 「Top 模型消耗」排行 + 完整明细表。
///
/// 排行的度量默认是 effective_cost（实付费用）。但如果这个时间段所有模型的
/// 费用都是 0（本地/免费渠道、或定价表没覆盖到的模型），画一排全零的条纯属
/// 骗人 —— 这时自动改用请求数排序，并把标题一起改掉，让用户知道看的是什么。

const TOP_N = 6;

export function ModelBreakdown({ rows }: { rows: ModelRow[] }) {
  const costTotal = rows.reduce((s, r) => s + r.effectiveCost, 0);
  const byCost = costTotal > 0;

  const ranked = [...rows]
    .sort((a, b) => (byCost ? b.effectiveCost - a.effectiveCost : b.requests - a.requests))
    .slice(0, TOP_N);

  const items: RankItem[] = ranked.map((r) => ({
    key: r.model,
    label: r.model,
    value: byCost ? r.effectiveCost : r.requests,
    valueText: byCost ? fmtCost(r.effectiveCost) : `${fmtInt(r.requests)} 次`,
    sub: byCost
      ? `${fmtInt(r.requests)} 次 · ${fmtCompact(r.outputTokens)} tok`
      : `${fmtCompact(r.outputTokens)} tok`,
    tone: rateTone(r.rate, r.requests),
  }));

  return (
    <div className="space-y-4">
      <div>
        <p className="mb-2.5 text-[11px] text-muted">
          {byCost
            ? "条长 = effective_cost（乘过渠道倍率的实付费用），颜色 = 该模型成功率"
            : "该时间段费用全部为 0，改按请求数排序"}
        </p>
        <RankBars items={items} />
      </div>
    </div>
  );
}

/** 完整明细。列的选取标准是「能支撑一个决策」：要不要换渠道、要不要换模型。
 *  单独导出：它需要整行宽度，塞进两列栅格里模型名会被截断。 */
export function ModelTable({ rows }: { rows: ModelRow[] }) {
  return (
    <div className="-mx-1 overflow-x-auto px-1">
      <table className="w-full min-w-[38rem] text-sm">
        <thead>
          <tr className="border-b border-border text-left text-[11px] text-muted">
            <th className="py-1.5 font-normal">模型</th>
            <th className="w-16 py-1.5 text-right font-normal">渠道</th>
            <th className="w-20 py-1.5 text-right font-normal">请求</th>
            <th className="w-20 py-1.5 text-right font-normal">成功率</th>
            <th className="w-20 py-1.5 text-right font-normal">首字节</th>
            <th className="w-24 py-1.5 text-right font-normal">输出 tok</th>
            <th className="w-24 py-1.5 text-right font-normal">费用</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((m) => (
            <tr key={m.model} className="border-b border-border/50 last:border-0">
              <td className="max-w-0 truncate py-1.5 pr-2 font-mono text-xs" title={m.model}>
                {m.model}
              </td>
              <td className="py-1.5 text-right text-xs tabular-nums text-muted">{m.channels}</td>
              <td className="py-1.5 text-right tabular-nums">{fmtInt(m.requests)}</td>
              <td
                className={`py-1.5 text-right font-medium tabular-nums ${
                  TONE_TEXT[rateTone(m.rate, m.requests)]
                }`}
              >
                {m.requests === 0 ? "—" : fmtPct(m.rate)}
              </td>
              <td className="py-1.5 text-right text-xs tabular-nums text-muted">
                {fmtDuration(m.firstByte)}
              </td>
              <td className="py-1.5 text-right text-xs tabular-nums text-muted">
                {fmtCompact(m.outputTokens)}
              </td>
              <td className="py-1.5 text-right tabular-nums">{fmtCost(m.effectiveCost)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
