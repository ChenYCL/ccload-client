import type { HealthPoint, StatsEntry } from "../../types";
import { ChannelHealthRow } from "../charts/HealthStrip";
import { fmtHm } from "../formatters";
import { byChannel } from "./derive";

/// 渠道健康时间线面板。
///
/// channel_health 固定 48 个桶，但**桶的宽度随时间范围变**：内核对 today 用
/// 「最近 4 小时 / 每桶 5 分钟」，其他范围用「整段时长 / 48」。所以标题里的
/// 时间窗只能从数据里的第一个和最后一个 ts 反推，不能写死成「最近 4 小时」。
///
/// 渠道名和平均耗时从 byChannel 拿：channel_health 的 key 只有 channel_id，
/// 其余信息都在 stats 行里，而那套「按请求数加权」的口径在 derive.ts 里已经
/// 有一份，这里再算一遍迟早会和别处漂移。

export function ChannelHealthPanel({
  health,
  rows,
}: {
  health: Record<string, HealthPoint[]>;
  rows: StatsEntry[];
}) {
  const byId = new Map(byChannel(rows).map((c) => [c.key, c]));

  const entries = Object.entries(health)
    .map(([id, points]) => ({
      id,
      points,
      name: byId.get(id)?.channel ?? `渠道 #${id}`,
      avgDuration: byId.get(id)?.duration,
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
