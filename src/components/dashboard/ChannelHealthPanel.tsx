import type { HealthPoint, StatsEntry } from "../../types";
import { ChannelHealthRow } from "../charts/HealthStrip";
import { fmtHm } from "../formatters";
import { channelNames } from "./derive";

/// 渠道健康时间线面板。
///
/// channel_health 固定 48 个桶，但**桶的宽度随时间范围变**：内核对 today 用
/// 「最近 4 小时 / 每桶 5 分钟」，其他范围用「整段时长 / 48」。所以标题里的
/// 时间窗只能从数据里的第一个和最后一个 ts 反推，不能写死成「最近 4 小时」。

export function ChannelHealthPanel({
  health,
  rows,
}: {
  health: Record<string, HealthPoint[]>;
  rows: StatsEntry[];
}) {
  const names = channelNames(rows);
  const durations = avgDurationByChannel(rows);

  const entries = Object.entries(health)
    .map(([id, points]) => ({
      id,
      points,
      name: names.get(id) ?? `渠道 #${id}`,
      avgDuration: durations.get(id),
      requests: points.reduce((s, p) => s + p.success + p.error, 0),
    }))
    // 有流量的排前面；都没流量时按名字稳定排序，避免每次轮询顺序跳动。
    .sort((a, b) => b.requests - a.requests || a.name.localeCompare(b.name));

  return (
    <div className="space-y-3.5">
      <p className="text-[11px] text-muted">{windowLabel(entries[0]?.points)}</p>
      {entries.map((e) => (
        <ChannelHealthRow
          key={e.id}
          name={e.name}
          points={e.points}
          avgDurationSeconds={e.avgDuration}
        />
      ))}
    </div>
  );
}

function windowLabel(points?: HealthPoint[]): string {
  if (!points || points.length < 2) return "48 个时间桶，灰格 = 该桶无请求";
  const from = fmtHm(points[0].ts);
  const to = fmtHm(points[points.length - 1].ts);
  return `${from} – ${to} · ${points.length} 桶 · 灰格 = 该桶无请求（不是 0%）`;
}

/** 渠道的平均耗时要按请求数加权，stats 是渠道 × 模型粒度，一个渠道有多行。 */
function avgDurationByChannel(rows: StatsEntry[]): Map<string, number> {
  const acc = new Map<string, { sum: number; weight: number }>();
  for (const r of rows) {
    if (r.channel_id == null || !r.avg_duration_seconds) continue;
    const n = (r.success ?? 0) + (r.error ?? 0);
    if (n === 0) continue;
    const key = String(r.channel_id);
    const cur = acc.get(key) ?? { sum: 0, weight: 0 };
    cur.sum += r.avg_duration_seconds * n;
    cur.weight += n;
    acc.set(key, cur);
  }
  const out = new Map<string, number>();
  for (const [k, v] of acc) if (v.weight > 0) out.set(k, v.sum / v.weight);
  return out;
}
