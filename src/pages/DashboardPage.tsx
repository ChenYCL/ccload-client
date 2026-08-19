import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Radio } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import type {
  ActiveRequest,
  HealthPoint,
  MetricPoint,
  StatsRange,
  StatsResponse,
} from "../types";
import { AsyncBlock, Panel } from "../components/StateBlock";
import { TrendChart } from "../components/charts/TrendChart";
import { StatCards } from "../components/dashboard/StatCards";
import { AnomalyPanel } from "../components/dashboard/AnomalyPanel";
import { ModelBreakdown, ModelTable } from "../components/dashboard/ModelBreakdown";
import { ChannelHealthPanel } from "../components/dashboard/ChannelHealthPanel";
import { anomaliesOf, byModel, totalsOf } from "../components/dashboard/derive";

/// 总览页。页面本身只做三件事：取数、选时间范围、摆放面板。
/// 所有聚合口径在 components/dashboard/derive.ts，图形在 components/charts/。
///
/// 数据来源（每个数字都能追到一个真实字段，没有占位值）：
///   · GET /admin/stats?range=   → stats[] / channel_health / rpm_stats
///   · GET /admin/metrics?range=&bucket_min= → 按桶补齐的时序点
///   · GET /admin/active-requests → 内存里的进行中请求，用作「现在」指示器

const RANGES: { id: StatsRange; label: string }[] = [
  { id: "today", label: "本日" },
  { id: "yesterday", label: "昨日" },
  { id: "this_week", label: "本周" },
];

// 桶宽按范围调，目标是让柱子数量落在 45–100 之间：太少看不出形状，
// 太多每根不到 3px 就糊成一片。
const BUCKET_MIN: Record<StatsRange, number> = {
  today: 15,
  yesterday: 30,
  this_week: 180,
};

export function DashboardPage() {
  const [range, setRange] = useState<StatsRange>("today");

  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  const running = kernel.data?.state === "running";

  const stats = useQuery({
    queryKey: ["stats", range],
    queryFn: () =>
      api.admin<StatsResponse>("GET", "stats", { query: `range=${range}` }),
    enabled: running,
    refetchInterval: 15_000,
    placeholderData: (prev) => prev,
  });
  const metrics = useQuery({
    queryKey: ["metrics", range],
    queryFn: () =>
      api.admin<MetricPoint[]>("GET", "metrics", {
        query: `range=${range}&bucket_min=${BUCKET_MIN[range]}`,
      }),
    enabled: running,
    refetchInterval: 30_000,
    placeholderData: (prev) => prev,
  });
  // 进行中请求读的是内核内存态，很便宜，可以刷得比统计勤。
  const active = useQuery({
    queryKey: ["active-requests"],
    queryFn: () => api.admin<ActiveRequest[]>("GET", "active-requests"),
    enabled: running,
    refetchInterval: 5_000,
    placeholderData: (prev) => prev,
  });

  const rows = stats.data?.data?.stats ?? [];
  const totals = totalsOf(rows);
  const models = byModel(rows);
  const anomalies = anomaliesOf(rows);
  const health: Record<string, HealthPoint[]> = stats.data?.data?.channel_health ?? {};
  const points = metrics.data?.data ?? [];
  const inFlight = active.data?.data?.length ?? 0;

  return (
    <div className="space-y-5">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="t-display">总览</h1>
          <p className="mt-0.5 text-sm text-muted">
            全部数字来自内核 Admin API 的真实字段，客户端只做聚合，不做估算。
          </p>
        </div>
        <div className="flex items-center gap-3">
          {running && inFlight > 0 && (
            <span className="flex items-center gap-1.5 rounded-full bg-emerald-500/12 px-2.5 py-1 text-xs font-medium text-emerald-700">
              <Radio className="h-3.5 w-3.5 animate-pulse" />
              {inFlight} 个请求进行中
            </span>
          )}
          <RangeSwitch value={range} onChange={setRange} />
        </div>
      </header>

      {!running ? (
        <div className="card bg-surface-raised px-4 py-8 text-center">
          <p className="text-sm text-muted">内核未运行，没有可展示的数据。</p>
          <p className="mt-1 text-xs text-muted/70">从左下角「启动内核」开始。</p>
        </div>
      ) : (
        <>
          {/* 统计出错时卡片区退化成骨架，而不是显示一排 0 —— 0 会被读成真实数字。 */}
          <AsyncBlock
            isLoading={stats.isPending}
            error={stats.error}
            isEmpty={false}
            emptyText=""
            skeletonLines={4}
          >
            <StatCards
              totals={totals}
              rpm={stats.data?.data?.rpm_stats}
              isToday={stats.data?.data?.is_today ?? range === "today"}
            />
          </AsyncBlock>

          <AnomalyPanel items={anomalies} />

          <Panel
            title="请求量与成功率"
            hint={`GET /admin/metrics · 每桶 ${BUCKET_MIN[range]} 分钟`}
          >
            <AsyncBlock
              isLoading={metrics.isPending}
              error={metrics.error}
              isEmpty={points.length === 0}
              emptyText="该时间段没有任何请求"
              emptyHint="内核已按桶补齐时间轴，空数组说明区间内确实没有流量"
              skeletonLines={5}
            >
              <TrendChart points={points} bucketMin={BUCKET_MIN[range]} />
            </AsyncBlock>
          </Panel>

          <div className="grid gap-5 lg:grid-cols-2">
            <Panel title="渠道健康" hint="stats.channel_health">
              <AsyncBlock
                isLoading={stats.isPending}
                error={stats.error}
                isEmpty={Object.keys(health).length === 0}
                emptyText="没有渠道健康数据"
                emptyHint="该时间段内没有任何渠道产生过请求"
              >
                <ChannelHealthPanel health={health} rows={rows} />
              </AsyncBlock>
            </Panel>

            <Panel title="模型消耗" hint="stats[] 按模型聚合">
              <AsyncBlock
                isLoading={stats.isPending}
                error={stats.error}
                isEmpty={models.length === 0}
                emptyText="没有模型用量"
                emptyHint="换个时间范围，或先让 CLI 发一次请求"
              >
                <ModelBreakdown rows={models} />
              </AsyncBlock>
            </Panel>
          </div>

          {/* 明细表独占整行：模型名（含 claude-fable-5[1m] 这类）在半列里会被
              截成「claude-fable…」，而左边是大片空白。列表本身可能几十行，
              限高内滚，避免整页被拉得很长。 */}
          <Panel title="模型明细" hint="stats[] 按模型聚合">
            <AsyncBlock
              isLoading={stats.isPending}
              error={stats.error}
              isEmpty={models.length === 0}
              emptyText="没有模型用量"
              emptyHint="换个时间范围，或先让 CLI 发一次请求"
            >
              <div className="max-h-[26rem] overflow-y-auto">
                <ModelTable rows={models} />
              </div>
            </AsyncBlock>
          </Panel>
        </>
      )}
    </div>
  );
}

function RangeSwitch({
  value,
  onChange,
}: {
  value: StatsRange;
  onChange: (r: StatsRange) => void;
}) {
  return (
    <div role="tablist" className="flex rounded-lg bg-surface-2 p-0.5">
      {RANGES.map((r) => (
        <button
          key={r.id}
          role="tab"
          aria-selected={value === r.id}
          onClick={() => onChange(r.id)}
          className={cn(
            "rounded-[6px] px-2.5 py-1 text-xs font-medium",
            value === r.id
              ? "bg-surface-raised text-content shadow-sm"
              : "text-muted hover:text-content",
          )}
        >
          {r.label}
        </button>
      ))}
    </div>
  );
}
