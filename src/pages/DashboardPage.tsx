import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Radio } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { useT, type Translate } from "../i18n";
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
import { UsageTotals } from "../components/dashboard/UsageTotals";
import { CliUsageBreakdown } from "../components/dashboard/CliUsageBreakdown";
import { AnomalyPanel } from "../components/dashboard/AnomalyPanel";
import { ModelBreakdown, ModelTable } from "../components/dashboard/ModelBreakdown";
import { ChannelBreakdown, ChannelTable } from "../components/dashboard/ChannelBreakdown";
import { ChannelHealthPanel } from "../components/dashboard/ChannelHealthPanel";
import { McpToolPanel } from "../components/dashboard/McpToolPanel";
import { anomaliesOf, byChannel, byModel, totalsOf } from "../components/dashboard/derive";

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
  { id: "this_month", label: "本月" },
];

// 桶宽按范围调，目标是让柱子数量落在 45–100 之间：太少看不出形状，
// 太多每根不到 3px 就糊成一片。
const BUCKET_MIN: Record<StatsRange, number> = {
  today: 15,
  yesterday: 30,
  this_week: 180,
  // 整月最长 31 天 = 44640 分钟，720（12 小时）一桶最多 62 根。
  this_month: 720,
};

/// 桶宽写成人话。本月那一档是 720 分钟，照原样写出来没人愿意去除一遍。
/// 模块级纯函数没有 hook，翻译函数当参数传。
function bucketLabel(min: number, t: Translate): string {
  if (min < 60) return t("每桶 {n} 分钟", { n: min });
  return t("每桶 {n} 小时", { n: min / 60 });
}

export function DashboardPage() {
  const t = useT();
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
  const channels = byChannel(rows);
  const anomalies = anomaliesOf(rows);
  const health: Record<string, HealthPoint[]> = stats.data?.data?.channel_health ?? {};
  const points = metrics.data?.data ?? [];
  const inFlight = active.data?.data?.length ?? 0;

  return (
    <div className="space-y-5">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="t-display">{t("总览")}</h1>
          <p className="mt-0.5 text-sm text-muted">
            {t("全部数字来自内核 Admin API 的真实字段，客户端只做聚合，不做估算。")}
          </p>
        </div>
        <div className="flex items-center gap-3">
          {running && inFlight > 0 && (
            <span className="flex items-center gap-1.5 rounded-full bg-emerald-500/12 px-2.5 py-1 text-xs font-medium text-emerald-700">
              <Radio className="h-3.5 w-3.5 animate-pulse" />
              {inFlight} {t("个请求进行中")}
            </span>
          )}
          <RangeSwitch value={range} onChange={setRange} />
        </div>
      </header>

      {!running ? (
        <div className="card bg-surface-raised px-4 py-8 text-center">
          <p className="text-sm text-muted">{t("内核未运行，没有可展示的数据。")}</p>
          <p className="mt-1 text-xs text-muted/70">{t("从左下角「启动内核」开始。")}</p>
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

          {/* 用量合计紧跟 KPI 行：卡片里的「费用」只说了花多少钱，这一块说的是
              这些钱买到了多少 token、又分成了哪四类。 */}
          <Panel title={t("用量合计")} hint={t("stats[] 全渠道 × 全模型合计")}>
            <AsyncBlock
              isLoading={stats.isPending}
              error={stats.error}
              isEmpty={totals.requests === 0}
              emptyText={t("该时间段没有任何请求")}
              emptyHint={t("换个时间范围，或先让 CLI 发一次请求")}
              skeletonLines={3}
            >
              <UsageTotals totals={totals} />
            </AsyncBlock>
          </Panel>

          <Panel
            title={t("请求量与成功率")}
            hint={`GET /admin/metrics · ${bucketLabel(BUCKET_MIN[range], t)}`}
          >
            <AsyncBlock
              isLoading={metrics.isPending}
              error={metrics.error}
              isEmpty={points.length === 0}
              emptyText={t("该时间段没有任何请求")}
              emptyHint={t("内核已按桶补齐时间轴，空数组说明区间内确实没有流量")}
              skeletonLines={5}
            >
              <TrendChart points={points} bucketMin={BUCKET_MIN[range]} />
            </AsyncBlock>
          </Panel>

          <div className="grid gap-5 lg:grid-cols-2">
            <Panel title={t("渠道消耗")} hint={t("stats[] 按渠道聚合")}>
              <AsyncBlock
                isLoading={stats.isPending}
                error={stats.error}
                isEmpty={channels.length === 0}
                emptyText={t("没有渠道用量")}
                emptyHint={t("换个时间范围，或先让 CLI 发一次请求")}
              >
                <ChannelBreakdown rows={channels} />
              </AsyncBlock>
            </Panel>

            <Panel title={t("模型消耗")} hint={t("stats[] 按模型聚合")}>
              <AsyncBlock
                isLoading={stats.isPending}
                error={stats.error}
                isEmpty={models.length === 0}
                emptyText={t("没有模型用量")}
                emptyHint={t("换个时间范围，或先让 CLI 发一次请求")}
              >
                <ModelBreakdown rows={models} />
              </AsyncBlock>
            </Panel>
          </div>

          <CliUsageBreakdown />

          <Panel title={t("渠道健康")} hint="stats.channel_health">
            <AsyncBlock
              isLoading={stats.isPending}
              error={stats.error}
              isEmpty={Object.keys(health).length === 0}
              emptyText={t("没有渠道健康数据")}
              emptyHint={t("该时间段内没有任何渠道产生过请求")}
            >
              <ChannelHealthPanel health={health} rows={rows} />
            </AsyncBlock>
          </Panel>

          {/* 两张明细表都独占整行：渠道名和模型名（含 claude-fable-5[1m] 这类）
              在半列里会被截成「claude-fable…」，而左边是大片空白。列表本身可能
              几十行，限高内滚，避免整页被拉得很长。 */}
          <Panel title={t("渠道明细")} hint={t("stats[] 按渠道聚合")}>
            <AsyncBlock
              isLoading={stats.isPending}
              error={stats.error}
              isEmpty={channels.length === 0}
              emptyText={t("没有渠道用量")}
              emptyHint={t("换个时间范围，或先让 CLI 发一次请求")}
            >
              <div className="max-h-[26rem] overflow-y-auto">
                <ChannelTable rows={channels} />
              </div>
            </AsyncBlock>
          </Panel>

          <Panel title={t("模型明细")} hint={t("stats[] 按模型聚合")}>
            <AsyncBlock
              isLoading={stats.isPending}
              error={stats.error}
              isEmpty={models.length === 0}
              emptyText={t("没有模型用量")}
              emptyHint={t("换个时间范围，或先让 CLI 发一次请求")}
            >
              <div className="max-h-[26rem] overflow-y-auto">
                <ModelTable rows={models} />
              </div>
            </AsyncBlock>
          </Panel>
        </>
      )}

      {/* MCP 工具统计不挂在 `running` 里面：流水是各 MCP 进程自己写的文件，
          内核停着也读得到，而且这正是「内核关了为什么工具还在动」最该被看见
          的时候。时间范围开关同样管不到它 —— 那三个范围是内核日志的口径。 */}
      <Panel title={t("MCP 工具调用")} hint={t("ccload-vision · 本地流水")}>
        <McpToolPanel />
      </Panel>
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
  const t = useT();
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
          {t(r.label)}
        </button>
      ))}
    </div>
  );
}
