import { useT } from "../../i18n";
import { fmtCompact, fmtCost, fmtDuration, fmtInt, fmtPct, rateTone, TONE_TEXT } from "../formatters";
import { RankBars, type RankItem } from "../charts/RankBars";
import { totalTokens, type ChannelRow } from "./derive";

/// 「渠道消耗」排行 + 完整明细表。切法（按渠道）和 ModelBreakdown（按模型）互补：
/// 模型回答「钱花在什么能力上」，渠道回答「钱付给了哪一家」。两边的费用合计
/// 必然相等 —— 对不上就是某一边的聚合写错了。
///
/// 排行的度量是 effective_cost：渠道之间真正要比的是实付价，而不同渠道的
/// cost_multiplier 往往差着几倍，比标准价等于假装倍率不存在。费用全为 0
/// （本地/免费渠道）时 byChannel 已经退回到按请求数排序，这里把标题一起改掉。

const TOP_N = 6;

export function ChannelBreakdown({ rows }: { rows: ChannelRow[] }) {
  const t = useT();
  const costTotal = rows.reduce((s, r) => s + r.effectiveCost, 0);
  const byCost = costTotal > 0;

  const items: RankItem[] = rows.slice(0, TOP_N).map((r) => ({
    key: r.key,
    label: r.channel,
    value: byCost ? r.effectiveCost : r.requests,
    valueText: byCost ? fmtCost(r.effectiveCost) : `${fmtInt(r.requests)} ${t("次")}`,
    sub: byCost
      ? `${fmtPct(r.effectiveCost / costTotal, 0)} · ${fmtInt(r.requests)} ${t("次")}`
      : `${fmtCompact(totalTokens(r))} tok`,
    tone: rateTone(r.rate, r.requests),
  }));

  return (
    <div>
      <p className="mb-2.5 text-[11px] text-muted">
        {byCost
          ? t("条长 = effective_cost（实付费用），右侧是它占总消耗的比例，颜色 = 该渠道成功率")
          : t("该时间段费用全部为 0，改按请求数排序")}
      </p>
      <RankBars items={items} />
    </div>
  );
}

/** 渠道明细表。列的选取标准和模型明细一致：每一列都要能支撑一个决策 ——
 *  这家是不是又贵又慢、要不要把流量挪到别家去。 */
export function ChannelTable({ rows }: { rows: ChannelRow[] }) {
  const t = useT();
  const costTotal = rows.reduce((s, r) => s + r.effectiveCost, 0);

  return (
    <div className="-mx-1 overflow-x-auto px-1">
      <table className="w-full min-w-[42rem] text-sm">
        <thead>
          <tr className="border-b border-border text-left text-[11px] text-muted">
            <th className="py-1.5 font-normal">{t("渠道")}</th>
            <th className="w-14 py-1.5 text-right font-normal">{t("模型")}</th>
            <th className="w-20 py-1.5 text-right font-normal">{t("请求")}</th>
            <th className="w-20 py-1.5 text-right font-normal">{t("成功率")}</th>
            <th className="w-20 py-1.5 text-right font-normal">{t("耗时")}</th>
            <th className="w-20 py-1.5 text-right font-normal">{t("首字节")}</th>
            <th className="w-24 py-1.5 text-right font-normal">{t("Token 合计")}</th>
            <th className="w-24 py-1.5 text-right font-normal">{t("标准价")}</th>
            <th className="w-24 py-1.5 text-right font-normal">{t("实付")}</th>
            <th className="w-16 py-1.5 text-right font-normal">{t("占比")}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((c) => (
            <tr key={c.key} className="border-b border-border/50 last:border-0">
              <td className="max-w-0 truncate py-1.5 pr-2 font-medium" title={c.channel}>
                {c.channel}
              </td>
              <td className="py-1.5 text-right text-xs tabular-nums text-muted">{c.models}</td>
              <td className="py-1.5 text-right tabular-nums">{fmtInt(c.requests)}</td>
              <td
                className={`py-1.5 text-right font-medium tabular-nums ${
                  TONE_TEXT[rateTone(c.rate, c.requests)]
                }`}
              >
                {c.requests === 0 ? "—" : fmtPct(c.rate)}
              </td>
              <td className="py-1.5 text-right text-xs tabular-nums text-muted">
                {fmtDuration(c.duration)}
              </td>
              <td className="py-1.5 text-right text-xs tabular-nums text-muted">
                {fmtDuration(c.firstByte)}
              </td>
              <td className="py-1.5 text-right text-xs tabular-nums text-muted">
                {fmtCompact(totalTokens(c))}
              </td>
              <td className="py-1.5 text-right text-xs tabular-nums text-muted">
                {fmtCost(c.standardCost)}
              </td>
              <td className="py-1.5 text-right tabular-nums">{fmtCost(c.effectiveCost)}</td>
              <td className="py-1.5 text-right text-xs tabular-nums text-muted">
                {costTotal > 0 ? fmtPct(c.effectiveCost / costTotal, 0) : "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
