import { useT } from "../../i18n";
import { useState } from "react";
import type { MetricPoint } from "../../types";
import { fmtCost, fmtDuration, fmtHm, fmtInt, fmtPct } from "../formatters";

/// 请求量趋势：堆叠柱（成功 + 失败）叠一条成功率折线。
///
/// 没有图表库，所以用手写 SVG。坐标系是「每桶 10 个单位宽 × 100 单位高」的
/// viewBox，配 preserveAspectRatio="none" 横向拉满容器 —— 柱子被等比拉宽没关系，
/// 但折线的描边会跟着变形，所以折线加了 vector-effect="non-scaling-stroke"。
/// 文字全部放在 SVG 外面用 HTML 画，避免被同一个变换拉扁。

const UNIT = 10; // 每个时间桶的横向单位宽
const BAR = 6.4; // 柱宽，剩下的是桶间留白
const H = 100;

type Props = {
  points: MetricPoint[];
  /** 桶时长（分钟），tooltip 里要说明这一根代表多久 */
  bucketMin: number;
};

export function TrendChart({ points, bucketMin }: Props) {
  const t = useT();
  const [hover, setHover] = useState<number | null>(null);

  const totals = points.map((p) => (p.success ?? 0) + (p.error ?? 0));
  const max = Math.max(1, ...totals);
  const W = Math.max(points.length, 1) * UNIT;

  // 成功率折线：没有样本的桶必须断开，而不是画成 0% —— 空闲和全挂是两件事。
  const segments: string[] = [];
  let run: string[] = [];
  points.forEach((p, i) => {
    if (totals[i] === 0) {
      if (run.length) segments.push(runToPoints(run));
      run = [];
      return;
    }
    const x = i * UNIT + UNIT / 2;
    const y = H - ((p.success ?? 0) / totals[i]) * H;
    run.push(`${x},${y}`);
  });
  if (run.length) segments.push(runToPoints(run));

  const hovered = hover != null ? points[hover] : null;
  const tickIdx = axisTicks(points.length);

  return (
    <div>
      <div className="flex gap-2">
        <YAxis labels={[fmtInt(max), fmtInt(Math.round(max / 2)), "0"]} align="right" />

        <div
          className="relative h-44 flex-1"
          onMouseLeave={() => setHover(null)}
        >
          <svg
            viewBox={`0 0 ${W} ${H}`}
            preserveAspectRatio="none"
            className="h-full w-full overflow-visible"
            aria-hidden
          >
            {[0, 0.5, 1].map((f) => (
              <line
                key={f}
                x1={0}
                x2={W}
                y1={H * f}
                y2={H * f}
                stroke="rgb(var(--border))"
                strokeWidth={1}
                vectorEffect="non-scaling-stroke"
              />
            ))}

            {points.map((p, i) => {
              const okH = ((p.success ?? 0) / max) * H;
              const errH = ((p.error ?? 0) / max) * H;
              const x = i * UNIT + (UNIT - BAR) / 2;
              const dim = hover != null && hover !== i;
              return (
                <g key={p.ts} opacity={dim ? 0.35 : 1}>
                  {errH > 0 && (
                    <rect x={x} y={H - okH - errH} width={BAR} height={errH} fill="rgb(220 38 38)" />
                  )}
                  {okH > 0 && (
                    <rect x={x} y={H - okH} width={BAR} height={okH} fill="rgb(var(--accent) / 0.55)" />
                  )}
                </g>
              );
            })}

            {/* key 用下标而不是 points 串：两段折线可能算出完全相同的坐标串
                （例如都只剩一个点、且落在同一格），那样 key 就撞了。 */}
            {segments.map((pts, i) => (
              <polyline
                key={i}
                points={pts}
                fill="none"
                stroke="rgb(16 185 129)"
                strokeWidth={1.75}
                strokeLinejoin="round"
                strokeLinecap="round"
                vectorEffect="non-scaling-stroke"
              />
            ))}
          </svg>

          {/* 命中区域独立于 SVG：每桶一格，指针一进去就响应，不用等点击。 */}
          <div className="absolute inset-0 flex">
            {points.map((p, i) => (
              <div
                key={p.ts}
                className="flex-1"
                onMouseEnter={() => setHover(i)}
                onFocus={() => setHover(i)}
                tabIndex={-1}
              />
            ))}
          </div>

          {hovered && hover != null && (
            <Tooltip point={hovered} index={hover} count={points.length} bucketMin={bucketMin} />
          )}
        </div>

        <YAxis labels={["100%", "50%", "0%"]} align="left" />
      </div>

      <div className="ml-12 mr-12 mt-1.5 flex text-[10px] text-muted">
        {points.map((p, i) => (
          <span key={p.ts} className="flex-1 text-center tabular-nums">
            {tickIdx.has(i) ? fmtHm(p.ts) : ""}
          </span>
        ))}
      </div>

      <div className="mt-2.5 flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-muted">
        <Legend className="bg-accent/55" label={t("成功请求")} />
        <Legend className="bg-red-600" label={t("失败请求")} />
        <Legend className="h-0.5 w-3 rounded-none bg-emerald-500" label={t("成功率（右轴）")} />
      </div>
    </div>
  );
}

function runToPoints(run: string[]): string {
  // 孤立的单点用 polyline 画不出来，拉成一小段横线才看得见。
  if (run.length > 1) return run.join(" ");
  const [x, y] = run[0].split(",").map(Number);
  return `${x - UNIT / 3},${y} ${x + UNIT / 3},${y}`;
}

/** 只标 5 个刻度，标满会糊成一条。 */
function axisTicks(n: number): Set<number> {
  const out = new Set<number>();
  if (n === 0) return out;
  const step = Math.max(1, Math.round((n - 1) / 4));
  for (let i = 0; i < n; i += step) out.add(i);
  out.add(n - 1);
  return out;
}

function YAxis({ labels, align }: { labels: string[]; align: "left" | "right" }) {
  // justify-between 把三个刻度顶在 0 / 50 / 100 三条网格线上，正好对上 SVG 里的
  // 那三条线，不需要再手工偏移。
  return (
    <div
      className={`flex h-44 w-10 shrink-0 flex-col justify-between text-[10px] leading-none tabular-nums text-muted ${
        align === "right" ? "items-end" : "items-start"
      }`}
    >
      {labels.map((l) => (
        <span key={l}>{l}</span>
      ))}
    </div>
  );
}

function Legend({ className, label }: { className: string; label: string }) {
  return (
    <span className="flex items-center gap-1.5">
      <span className={`h-2 w-2 rounded-sm ${className}`} />
      {label}
    </span>
  );
}

function Tooltip(props: {
  point: MetricPoint;
  index: number;
  count: number;
  bucketMin: number;
}) {
  const t = useT();
  const { point: p, index, count } = props;
  const total = (p.success ?? 0) + (p.error ?? 0);
  const left = ((index + 0.5) / count) * 100;
  // 贴边时改成单侧对齐，否则 tooltip 会被容器裁掉一半。
  const pos =
    left < 22
      ? { left: 0 }
      : left > 78
        ? { right: 0 }
        : { left: `${left}%`, transform: "translateX(-50%)" };
  return (
    <div
      className="material-modal pointer-events-none absolute top-1 z-10 w-52 rounded-xl border border-border p-2.5 text-xs"
      style={pos}
    >
      <div className="font-medium tabular-nums">
        {fmtHm(p.ts)}
        <span className="ml-1 font-normal text-muted">起 {props.bucketMin} 分钟</span>
      </div>
      <dl className="mt-1.5 space-y-0.5 text-muted">
        <Row k={t("请求")} v={fmtInt(total)} />
        <Row k={t("成功")} v={fmtInt(p.success)} tone="text-emerald-600" />
        <Row k={t("失败")} v={fmtInt(p.error)} tone={p.error ? "text-red-600" : undefined} />
        <Row k={t("成功率")} v={total ? fmtPct((p.success ?? 0) / total) : "—"} />
        <Row k={t("平均首字")} v={fmtDuration(p.avg_first_byte_time_seconds)} />
        <Row k={t("费用")} v={fmtCost(p.effective_cost ?? p.total_cost)} />
      </dl>
    </div>
  );
}

function Row({ k, v, tone }: { k: string; v: string; tone?: string }) {
  return (
    <div className="flex justify-between gap-3">
      <dt>{k}</dt>
      <dd className={`tabular-nums ${tone ?? "text-content"}`}>{v}</dd>
    </div>
  );
}
