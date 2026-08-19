import type { ReactNode } from "react";
import { cn } from "../../lib/cn";
import type { RpmStats } from "../../types";
import { fmtCompact, fmtCost, fmtInt, fmtPct, rateTone, TONE_TEXT } from "../formatters";
import type { Totals } from "./derive";

/// 顶部 KPI 行。每张卡只放一个主数字 + 一行来源明确的副信息 ——
/// 副信息存在的意义是让主数字可被验证（成功率旁边写清成功/失败的绝对数），
/// 而不是凑满版面。

function Card({
  label,
  value,
  tone,
  sub,
}: {
  label: string;
  value: string;
  tone?: string;
  sub?: ReactNode;
}) {
  return (
    <div className="card bg-surface-raised p-3.5">
      <div className="text-xs text-muted">{label}</div>
      <div className={cn("mt-0.5 text-[1.75rem] font-semibold leading-tight tabular-nums", tone)}>
        {value}
      </div>
      <div className="mt-0.5 text-[11px] leading-snug text-muted">{sub ?? " "}</div>
    </div>
  );
}

export function StatCards({
  totals,
  rpm,
  isToday,
}: {
  totals: Totals;
  rpm?: RpmStats;
  isToday: boolean;
}) {
  const tone = rateTone(totals.rate, totals.requests);

  return (
    <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
      <Card
        label="请求数"
        value={fmtInt(totals.requests)}
        sub={
          totals.requests > 0 ? (
            <>
              成功 <span className="text-emerald-600">{fmtInt(totals.success)}</span> · 失败{" "}
              <span className={totals.error ? "text-red-600" : undefined}>
                {fmtInt(totals.error)}
              </span>
            </>
          ) : (
            "该时间段没有请求"
          )
        }
      />

      <Card
        label="成功率"
        value={totals.requests === 0 ? "—" : fmtPct(totals.rate)}
        tone={TONE_TEXT[tone]}
        sub={
          totals.error > 0
            ? `${fmtInt(totals.error)} 次失败待排查`
            : totals.requests > 0
              ? "无失败请求"
              : undefined
        }
      />

      {/* rpm_stats 的 recent_rpm 只在 range=today 有效，其他区间内核不计算，
          所以这里换成 avg_rpm 并把标题改掉，而不是显示一个假的「近期」。 */}
      <Card
        label={isToday ? "近一分钟 RPM" : "平均 RPM"}
        value={rpm ? fmtInt(isToday ? rpm.recent_rpm : rpm.avg_rpm) : "—"}
        sub={
          rpm ? (
            <>
              峰值 <span className="text-content">{fmtInt(rpm.peak_rpm)}</span> · 均值{" "}
              <span className="text-content">{rpm.avg_rpm.toFixed(1)}</span> · 峰值 QPS{" "}
              <span className="text-content">{rpm.peak_qps.toFixed(2)}</span>
            </>
          ) : undefined
        }
      />

      <Card
        label="费用（倍率后）"
        value={fmtCost(totals.effectiveCost)}
        sub={
          <>
            标准价 {fmtCost(totals.standardCost)} · 输出{" "}
            {fmtCompact(totals.outputTokens)} tok
          </>
        }
      />
    </div>
  );
}
