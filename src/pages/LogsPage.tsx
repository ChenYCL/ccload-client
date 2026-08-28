import { useT } from "../i18n";
import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ArrowUp, Pause, RefreshCw, Radio } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import type { ActiveRequest, LogEntry, LogsBootstrap } from "../types";
import { AsyncBlock, Panel } from "../components/StateBlock";
import { ActiveRequestsPanel } from "../components/logs/ActiveRequestsPanel";
import { EMPTY_FILTERS, LogFilters, type LogFilterState } from "../components/logs/LogFilters";
import { LogTable } from "../components/logs/LogTable";
import { LogDetail } from "../components/logs/LogDetail";
import { useLogFeed } from "../components/logs/useLogFeed";
import { matchSessions } from "../lib/sessionMatch";
import { fmtInt } from "../components/formatters";

/// 实时日志页。
///
/// 内核没有日志的 SSE / WebSocket 推送，只能轮询，所以这里分两层：
///   · 上半：GET /admin/active-requests —— 内核内存里的在飞请求。这才是真实时，
///     而且它是唯一能看到「正在跑但还没结束」的请求的地方（日志只在请求结束后落库）。
///     响应体只有几行、不查数据库，所以 1.5s 一轮。
///   · 下半：GET /admin/logs —— 已完成的历史。每轮要走 SQLite 的 LIMIT + COUNT，
///     比上面贵得多，取 2.5s：比人读一行的时间短，又不至于把库打满。
/// 两者都靠 react-query 默认的 refetchIntervalInBackground=false，在窗口不可见时
/// 自动停轮询，不会在后台一直打内核。

const ACTIVE_POLL_MS = 1_500;
const LOGS_POLL_MS = 2_500;
/** bootstrap 只是下拉的取值，变化很慢，跟着日志一起刷纯属浪费。 */
const BOOTSTRAP_POLL_MS = 60_000;

/** 常规拉取条数。开「只看错误」时要多拉一些，原因见 visible 的注释。 */
const LIMIT = 200;
const LIMIT_ERRORS_ONLY = 500;

/// 轮询开关的记忆位。这页离开视线时 react-query 就停轮询了，但只要停在这一页
/// 不动，三条查询会一直打内核 —— 远端内核在公网上时这不是免费的。所以给一个
/// 显式开关，关掉即完全静默；选择要跨会话记住，不然每次进来都要再关一次。
const LIVE_KEY = "ccload.logs.live";

export function LogsPage({ onNavigate }: { onNavigate?: (page: "session-manage") => void }) {
  const t = useT();
  const [filters, setFilters] = useState<LogFilterState>(EMPTY_FILTERS);
  const [selected, setSelected] = useState<LogEntry | null>(null);

  const [live, setLive] = useState(() => localStorage.getItem(LIVE_KEY) !== "0");
  const toggleLive = () => {
    setLive((v) => {
      localStorage.setItem(LIVE_KEY, v ? "0" : "1");
      return !v;
    });
  };

  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  const running = kernel.data?.state === "running";
  // 关掉实时后仍然保留「手动刷新」：用户要的是别一直轮询，不是看不到数据。
  const polling = running && live;

  const limit = filters.errorsOnly ? LIMIT_ERRORS_ONLY : LIMIT;
  const query = useMemo(() => {
    const p = new URLSearchParams({ range: "today", limit: String(limit) });
    if (filters.model) p.set("model", filters.model);
    if (filters.channelId) p.set("channel_id", filters.channelId);
    if (filters.statusCode) p.set("status_code", filters.statusCode);
    return p.toString();
  }, [filters, limit]);

  const active = useQuery({
    queryKey: ["active-requests"],
    queryFn: () => api.admin<ActiveRequest[]>("GET", "active-requests"),
    enabled: running,
    refetchInterval: polling ? ACTIVE_POLL_MS : false,
    placeholderData: (prev) => prev,
  });
  const logs = useQuery({
    queryKey: ["logs", query],
    queryFn: () => api.admin<LogEntry[]>("GET", "logs", { query }),
    enabled: running,
    refetchInterval: polling ? LOGS_POLL_MS : false,
    // 换筛选时保留上一份结果，避免表格闪一下空白再填回来。
    placeholderData: (prev) => prev,
  });
  const bootstrap = useQuery({
    queryKey: ["logs-bootstrap"],
    queryFn: () => api.admin<LogsBootstrap>("GET", "logs/bootstrap", { query: "range=today" }),
    enabled: running,
    refetchInterval: polling ? BOOTSTRAP_POLL_MS : false,
  });

  const fetched = logs.data?.data ?? [];
  // 内核的 LogFilter 只有「状态码等于某个值」，没有「状态码 >= 400」，所以
  // 「只看错误」只能在客户端筛已取回的这一页 —— 拉 500 条再筛，并在下面
  // 把「从多少条里筛出多少条」写清楚，不让用户把它当成全量结果。
  const visible = useMemo(
    () => (filters.errorsOnly ? fetched.filter((l) => (l.status_code ?? 0) >= 400) : fetched),
    [fetched, filters.errorsOnly],
  );

  const feed = useLogFeed(visible, query + String(filters.errorsOnly));
  const activeItems = active.data?.data ?? [];
  const total = logs.data?.count ?? 0;

  // 会话归因。内核日志里没有 session_id，只有我们自己的代理留了痕迹，所以
  // 单独拉一份记录再按「时间 + 模型」对上（见 sessionMatch）。代理没起来时
  // 这里是空数组，会话列自动不显示。
  const proxyRecords = useQuery({
    queryKey: ["cli-proxy-records"],
    queryFn: api.cliProxyRecords,
    refetchInterval: polling ? LOGS_POLL_MS : false,
    placeholderData: (prev) => prev,
  });
  const sessions = useMemo(
    () => matchSessions(visible, proxyRecords.data ?? []),
    [visible, proxyRecords.data],
  );
  // 标题要读磁盘上的会话文件，按 id 逐个解析并缓存 —— 同一个会话会占很多行，
  // 逐行去查会把同一个文件读上几十遍。
  const sessionIds = useMemo(
    () => Array.from(new Set(sessions.values())).sort(),
    [sessions],
  );
  const titles = useQuery({
    queryKey: ["cli-proxy-session-titles", sessionIds],
    enabled: sessionIds.length > 0,
    queryFn: async () => {
      const out = new Map<string, string>();
      const refs = await Promise.all(
        sessionIds.map((id) => api.cliProxySession(id).catch(() => null)),
      );
      for (const r of refs) {
        if (r?.title) out.set(r.session_id, r.title);
      }
      return out;
    },
  });

  return (
    <div className="space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="t-display">{t("实时日志")}</h1>
          <p className="mt-0.5 text-sm text-muted">
            {live
              ? t("内核没有日志推送通道，这里是轮询：进行中 {a}s、历史 {b}s，窗口切走时自动暂停。", {
                  a: ACTIVE_POLL_MS / 1000,
                  b: LOGS_POLL_MS / 1000,
                })
              : t("实时已关闭，不再向内核发起任何轮询；下面显示的是最后一次取到的数据。")}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {!live && running && (
            <button
              onClick={() => {
                active.refetch();
                logs.refetch();
              }}
              disabled={logs.isFetching || active.isFetching}
              className="flex items-center gap-1.5 rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 text-xs hover:bg-surface-2 disabled:opacity-40"
            >
              <RefreshCw
                className={cn("h-3.5 w-3.5", (logs.isFetching || active.isFetching) && "animate-spin")}
              />
              {t("刷新一次")}
            </button>
          )}
          <LiveSwitch on={live} onToggle={toggleLive} />
        </div>
      </header>

      {!running ? (
        <div className="card bg-surface-raised px-4 py-8 text-center">
          <p className="text-sm text-muted">{t("内核未运行，没有日志可看。")}</p>
          <p className="mt-1 text-xs text-muted/70">{t("从左下角「启动内核」开始。")}</p>
        </div>
      ) : (
        <>
          <Panel
            title={t("进行中")}
            hint={t("GET /admin/active-requests · 内核内存态")}
            right={
              activeItems.length > 0 ? (
                <span className="flex items-center gap-1.5 rounded-full bg-emerald-500/12 px-2 py-0.5 text-xs font-medium text-emerald-700">
                  <Radio className="h-3 w-3 animate-pulse" />
                  {activeItems.length}
                </span>
              ) : undefined
            }
          >
            <AsyncBlock
              isLoading={active.isPending}
              error={active.error}
              isEmpty={false}
              emptyText=""
              skeletonLines={2}
            >
              <ActiveRequestsPanel items={activeItems} />
            </AsyncBlock>
          </Panel>

          <Panel
            title={t("历史日志")}
            hint={
              filters.errorsOnly
                ? `在最近 ${fmtInt(fetched.length)} 条中筛出 ${fmtInt(visible.length)} 条错误`
                : `本日共 ${fmtInt(total)} 条，显示最近 ${fmtInt(fetched.length)} 条`
            }
            right={<FollowBadge following={feed.following} pending={feed.pending} />}
          >
            <div className="space-y-3">
              <LogFilters
                value={filters}
                onChange={setFilters}
                bootstrap={bootstrap.data?.data}
              />

              <AsyncBlock
                isLoading={logs.isPending}
                error={logs.error}
                isEmpty={feed.logs.length === 0}
                emptyText={filters.errorsOnly ? t("最近的日志里没有错误") : t("本日还没有日志")}
                emptyHint={
                  filters.errorsOnly
                    ? `已检查最近 ${fmtInt(fetched.length)} 条`
                    : t("让 CLI 发一次请求就会出现")
                }
                skeletonLines={6}
              >
                <div className="relative">
                  <div
                    ref={feed.scrollRef}
                    onScroll={feed.onScroll}
                    className="max-h-[28rem] overflow-auto rounded-lg border border-border"
                  >
                    <LogTable
                      logs={feed.logs}
                      flashIds={feed.flashIds}
                      selectedId={selected?.id}
                      onSelect={setSelected}
                      sessions={sessions}
                      sessionTitles={titles.data}
                      onOpenSession={(id) => {
                        // 会话管理页自己按 id 定位，这里只负责把 id 交出去
                        // 再切页 —— 用 sessionStorage 而不是 URL，是因为这个
                        // 应用的路由就是一个 useState，没有可挂参数的地方。
                        sessionStorage.setItem("ccload:focus-session", id);
                        onNavigate?.("session-manage");
                      }}
                    />
                  </div>

                  {!feed.following && feed.pending > 0 && (
                    <button
                      onClick={feed.resume}
                      className="material-modal animate-materialize absolute inset-x-0 bottom-3 mx-auto flex w-max items-center gap-1.5 rounded-full border border-border px-3 py-1.5 text-xs font-medium text-accent"
                    >
                      <ArrowUp className="h-3.5 w-3.5" />
                      {feed.pending} {t("条新日志")}
                    </button>
                  )}
                </div>
              </AsyncBlock>
            </div>
          </Panel>
        </>
      )}

      {selected && <LogDetail log={selected} onClose={() => setSelected(null)} />}
    </div>
  );
}

/// 轮询总开关。做成开关而不是按钮：它表达的是一个持续状态（正在/没有在轮询），
/// 而按钮表达的是一次动作。
function LiveSwitch({ on, onToggle }: { on: boolean; onToggle: () => void }) {
  const t = useT();
  return (
    <button
      role="switch"
      aria-checked={on}
      onClick={onToggle}
      title={on ? t("关闭实时轮询，省下持续的内核请求") : t("开启实时轮询")}
      className={cn(
        "flex items-center gap-2 rounded-full border px-2.5 py-1.5 text-xs font-medium",
        on
          ? "border-accent/40 bg-accent/10 text-accent"
          : "border-border bg-surface-raised text-muted hover:bg-surface-2",
      )}
    >
      {/* 行程用 translate-x-full 而不是写死的 rem：滑块宽 0.625rem、轨道宽
          1.5rem、两侧各留 0.125rem，行程恰好等于滑块自身宽度。写死数值时
          html 的根字号（这里是 17px 而不是 16px）一变，滑块就会顶出轨道、
          压到旁边的文字上。 */}
      <span
        className={cn(
          "relative h-3.5 w-6 shrink-0 rounded-full transition-colors duration-150",
          on ? "bg-accent" : "bg-border",
        )}
      >
        <span
          className={cn(
            "absolute left-0.5 top-0.5 h-2.5 w-2.5 rounded-full bg-white shadow-sm",
            "transition-transform duration-[220ms] ease-[cubic-bezier(0.32,0.72,0,1)]",
            on ? "translate-x-full" : "translate-x-0",
          )}
        />
      </span>
      {on ? t("实时开") : t("实时关")}
    </button>
  );
}

/** 跟随状态要一直可见 —— 用户得知道自己看的是实时流还是一份定住的快照。 */
function FollowBadge({ following, pending }: { following: boolean; pending: number }) {
  const t = useT();
  return (
    <span
      className={cn(
        "flex items-center gap-1.5 rounded-full px-2 py-0.5 text-[11px] font-medium",
        following ? "bg-accent/10 text-accent" : "bg-surface-2 text-muted",
      )}
    >
      {following ? (
        <>
          <span className="h-1.5 w-1.5 rounded-full bg-accent" />
          {t("实时跟随")}
        </>
      ) : (
        <>
          <Pause className="h-3 w-3" />
          {t("已暂停")}{pending > 0 ? ` · ${pending} 条待读` : ""}
        </>
      )}
    </span>
  );
}
