import { useT, type Translate } from "../../i18n";
import { fmtCompact, fmtCost, fmtInt, fmtPct } from "../formatters";
import { totalTokens, type Totals } from "./derive";

/// 「用量合计」：这段时间一共处理了多少 token，以及它们分成了哪四类。
///
/// 为什么单开一块，而不是往 KPI 行里再塞一张卡：账单是按 token 算的，而四类
/// token 的单价差着一个量级（缓存读通常是输入价的十分之一，缓存写反而更贵）。
/// 只报一个「输出 1.8M」既解释不了费用，也看不出缓存到底有没有生效 —— 而
/// 「缓存读占输入侧多少」正是 Claude Code 这类每轮重发 system 提示的客户端
/// 最该盯的一个数。
///
/// 四个字段都是内核逐条日志累加出来的真实用量（total_input_tokens 等），这里
/// 只做加法和除法。占比之外不推算任何「等效」「预计」的数。

type Part = {
  key: keyof Pick<
    Totals,
    "inputTokens" | "outputTokens" | "cacheReadTokens" | "cacheCreationTokens"
  >;
  label: string;
  fill: string;
};

/// 颜色只区分类别，不表达好坏 —— 这里没有「健康」这一维，套用 TONE_FILL 的
/// 红绿会被读成告警。输入和输出必须是两个色相：同色深浅在 2.5px 高的条上分不开。
const PARTS: Part[] = [
  { key: "inputTokens", label: "输入", fill: "bg-sky-500" },
  { key: "outputTokens", label: "输出", fill: "bg-accent" },
  { key: "cacheReadTokens", label: "缓存读取", fill: "bg-emerald-500" },
  { key: "cacheCreationTokens", label: "缓存写入", fill: "bg-amber-500" },
];

export function UsageTotals({ totals }: { totals: Totals }) {
  const t = useT();
  const total = totalTokens(totals);
  // 输入侧 = 三类喂进去的 token。缓存命中率的分母只能是它，把输出算进来会
  // 得到一个永远偏低、且随回答长度晃动的假比例。
  const inputSide =
    totals.inputTokens + totals.cacheReadTokens + totals.cacheCreationTokens;

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-baseline justify-between gap-x-6 gap-y-1">
        <div className="flex items-baseline gap-2">
          <span className="text-[1.75rem] font-semibold leading-tight tabular-nums">
            {fmtCompact(total)}
          </span>
          <span className="text-xs text-muted">tok</span>
        </div>
        <div className="text-xs text-muted">
          {t("实付")} <span className="font-medium text-content">{fmtCost(totals.effectiveCost)}</span>{" "}
          {t("· 标准价")} {fmtCost(totals.standardCost)}
          {totals.standardCost > 0 && (
            <> {t("· 倍率")} {(totals.effectiveCost / totals.standardCost).toFixed(2)}×</>
          )}
        </div>
      </div>

      {/* 组成条。0 的那一类整段不画 —— 留一条 1px 的缝会被当成「有一点点」。 */}
      <div className="flex h-2.5 w-full overflow-hidden rounded-full bg-surface-2">
        {total > 0 &&
          PARTS.map((p) => {
            const v = totals[p.key];
            if (v <= 0) return null;
            return (
              <div
                key={p.key}
                className={p.fill}
                style={{ width: `${(v / total) * 100}%` }}
                title={`${t(p.label)} ${fmtInt(v)}`}
              />
            );
          })}
      </div>

      <ul className="grid grid-cols-2 gap-x-6 gap-y-1.5 sm:grid-cols-4">
        {PARTS.map((p) => {
          const v = totals[p.key];
          return (
            <li key={p.key} className="min-w-0">
              <div className="flex items-center gap-1.5 text-[11px] text-muted">
                <span className={`h-2 w-2 shrink-0 rounded-sm ${p.fill}`} />
                <span className="truncate">{t(p.label)}</span>
              </div>
              <div className="mt-0.5 flex items-baseline gap-1.5">
                <span className="text-sm font-medium tabular-nums">{fmtCompact(v)}</span>
                <span className="text-[11px] tabular-nums text-muted">
                  {total > 0 ? fmtPct(v / total, 1) : "—"}
                </span>
              </div>
            </li>
          );
        })}
      </ul>

      <p className="text-[11px] text-muted">{footnote(totals, total, inputSide, t)}</p>
    </div>
  );
}

/// 模块级纯函数没有 hook，翻译函数当参数传进来。
function footnote(totals: Totals, total: number, inputSide: number, t: Translate): string {
  const parts = [
    t("{n} 次请求", { n: fmtInt(totals.requests) }),
    totals.requests > 0
      ? t("平均每次 {n} tok", { n: fmtCompact(Math.round(total / totals.requests)) })
      : "",
    inputSide > 0
      ? t("缓存读取占输入侧 {n}", { n: fmtPct(totals.cacheReadTokens / inputSide, 1) })
      : "",
  ];
  return parts.filter(Boolean).join(" · ");
}
