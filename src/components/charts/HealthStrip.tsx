import { useT } from "../../i18n";
import type { HealthPoint } from "../../types";
import { fmtDuration, fmtInt, fmtPct, rateTone, TONE_FILL, TONE_TEXT } from "../formatters";

/// 渠道健康时间线：内核 stats 响应里 channel_health 直接给的 48 个桶（本日 =
/// 最近 4 小时，每桶 5 分钟）。一格一桶，颜色即成功率。
///
/// 关键点：rate === -1 表示这一桶**没有样本**，必须画成灰色而不是红色 ——
/// 空闲和全挂看起来必须不一样。

export function HealthStrip({ points }: { points: HealthPoint[] }) {
  const t = useT();
  return (
    <div className="flex h-5 items-stretch gap-[1px]" role="img" aria-label={t("健康时间线")}>
      {points.map((p) => {
        const sample = p.success + p.error;
        const tone = rateTone(p.rate, sample);
        return (
          <div
            key={p.ts}
            title={
              sample === 0
                ? `${new Date(p.ts).toLocaleTimeString("zh-CN", { hour12: false })} · 无请求`
                : `${new Date(p.ts).toLocaleTimeString("zh-CN", { hour12: false })} · ${fmtPct(
                    p.rate,
                  )} · ${sample} 次${p.rate_limited ? ` · 429×${p.rate_limited}` : ""}`
            }
            className={`flex-1 rounded-[2px] ${sample === 0 ? "bg-surface-2" : TONE_FILL[tone]}`}
          />
        );
      })}
    </div>
  );
}

/** 一行渠道：名称 + 汇总成功率 + 时间线。汇总口径就是这 48 桶的加总。 */
export function ChannelHealthRow(props: {
  name: string;
  points: HealthPoint[];
  avgDurationSeconds?: number;
}) {
  const t = useT();
  const success = props.points.reduce((s, p) => s + p.success, 0);
  const error = props.points.reduce((s, p) => s + p.error, 0);
  const limited = props.points.reduce((s, p) => s + p.rate_limited, 0);
  const total = success + error;
  const rate = total === 0 ? -1 : success / total;
  const tone = rateTone(rate, total);

  return (
    <div className="space-y-1.5">
      <div className="flex items-baseline justify-between gap-3">
        <span className="truncate text-sm font-medium">{props.name}</span>
        <span className="flex shrink-0 items-baseline gap-2 text-xs tabular-nums">
          {limited > 0 && (
            <span className="rounded bg-amber-500/14 px-1.5 py-0.5 text-[10px] text-amber-700">
              429 × {limited}
            </span>
          )}
          {props.avgDurationSeconds != null && (
            <span className="text-muted">{fmtDuration(props.avgDurationSeconds)}</span>
          )}
          <span className="text-muted">{fmtInt(total)} {t("次")}</span>
          <span className={`w-12 text-right font-medium ${TONE_TEXT[tone]}`}>
            {total === 0 ? t("闲置") : fmtPct(rate)}
          </span>
        </span>
      </div>
      <HealthStrip points={props.points} />
    </div>
  );
}
