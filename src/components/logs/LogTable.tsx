import { useT } from "../../i18n";
import { cn } from "../../lib/cn";
import { displayModel, splitPinned } from "../../lib/pins";
import type { LogEntry } from "../../types";
import {
  effectiveCost,
  fmtClock,
  fmtCompact,
  fmtCost,
  fmtDuration,
  statusTone,
  TONE_BADGE,
} from "../formatters";

/// 历史日志表。/admin/logs 按 time 倒序返回，所以最新的一条在最上面 ——
/// 「实时边缘」在顶部，这也是下面 LogsPage 里跟随/暂停逻辑的方向。
///
/// 新到的行会带 1 秒的高亮淡出：`flashIds` 里的 id 渲染时就带底色，随后由父组件
/// 清空该集合，靠 transition-colors 淡回透明。用「淡出」而不是「淡入」当信号，
/// 是因为行是新插入的，淡入的起点根本没人看见。

export function LogTable({
  logs,
  flashIds,
  selectedId,
  onSelect,
  sessions,
  sessionTitles,
  onOpenSession,
}: {
  logs: LogEntry[];
  flashIds: ReadonlySet<number>;
  selectedId?: number;
  onSelect: (log: LogEntry) => void;
  /** 日志 id -> 会话 id，由 sessionMatch 算出来；匹配不上的行不显示会话。 */
  sessions?: ReadonlyMap<number, string>;
  /** 会话 id -> 标题，异步解析出来的；没解析到就退回显示短 id。 */
  sessionTitles?: ReadonlyMap<string, string>;
  onOpenSession?: (sessionId: string) => void;
}) {
  const t = useT();
  const showSessions = (sessions?.size ?? 0) > 0;
  return (
    // table-fixed：列宽由表头决定，不再随内容抖动。轮询每 2.5s 换一批行，
    // auto 布局会让整张表在每次刷新时重新算列宽，视觉上就是「闪一下」。
    <table className="w-full min-w-[52rem] table-fixed text-sm">
      <thead className="sticky top-0 z-10">
        <tr className="material-chrome text-left text-[11px] text-muted">
          <Th className="w-[5.5rem]">{t("时间")}</Th>
          <Th className="w-14">{t("状态")}</Th>
          {/* 模型列是唯一会长到失控的一列（别名 + 重定向目标），给它一个上限
              而不是让它吃掉所有剩余宽度 —— 那正是右侧数字列被挤扁的原因。 */}
          <Th className="w-[22rem]">{t("模型")}</Th>
          {/* 会话列只在代理有记录时出现：没代理就一列空白，白占宽度。 */}
          {showSessions && <Th className="w-40">{t("会话")}</Th>}
          <Th className="w-32">{t("渠道")}</Th>
          <Th className="w-[4.5rem] text-right">{t("耗时")}</Th>
          <Th className="w-[4.5rem] text-right">{t("首字节")}</Th>
          <Th className="w-28 text-right">tokens</Th>
          <Th className="w-20 text-right">{t("费用")}</Th>
        </tr>
      </thead>
      <tbody>
        {logs.map((log) => (
          <LogRow
            key={log.id}
            log={log}
            flash={flashIds.has(log.id)}
            selected={selectedId === log.id}
            onSelect={onSelect}
            showSession={showSessions}
            sessionId={sessions?.get(log.id)}
            sessionTitles={sessionTitles}
            onOpenSession={onOpenSession}
          />
        ))}
      </tbody>
    </table>
  );
}

function Th({ className, children }: { className?: string; children: React.ReactNode }) {
  return (
    <th className={cn("border-b border-border px-2 py-1.5 font-normal", className)}>
      {children}
    </th>
  );
}

function LogRow({
  log,
  flash,
  selected,
  onSelect,
  showSession,
  sessionId,
  sessionTitles,
  onOpenSession,
}: {
  log: LogEntry;
  flash: boolean;
  selected: boolean;
  onSelect: (log: LogEntry) => void;
  showSession: boolean;
  sessionId?: string;
  sessionTitles?: ReadonlyMap<string, string>;
  onOpenSession?: (sessionId: string) => void;
}) {
  const t = useT();
  const tone = statusTone(log.status_code);
  // 输入 token 里 cache_read 通常是大头，合并展示才有可比性；分项在详情里给。
  const tokensIn =
    (log.input_tokens ?? 0) +
    (log.cache_read_input_tokens ?? 0) +
    (log.cache_creation_input_tokens ?? 0);

  return (
    <tr
      onClick={() => onSelect(log)}
      aria-selected={selected}
      className={cn(
        "cursor-pointer border-b border-border/40 transition-colors duration-1000",
        flash ? "bg-accent/10" : "bg-transparent",
        selected && "!bg-accent/12",
        "hover:!bg-surface-2",
      )}
    >
      <td className="px-2 py-1 text-xs tabular-nums text-muted">{fmtClock(log.time)}</td>
      <td className="px-2 py-1">
        <span
          className={cn(
            "inline-block rounded px-1.5 py-px text-[11px] font-medium tabular-nums",
            TONE_BADGE[tone],
          )}
        >
          {log.status_code ?? "—"}
        </span>
      </td>
      <td className="max-w-0 truncate px-2 py-1 font-mono text-xs" title={log.model}>
        {/* 钉住的首选渠道走的是私有别名 grok-4.6@ch21，显示时剥回原名、标一个「首选」 */}
        {displayModel(log.model) ?? "—"}
        {log.model && splitPinned(log.model) && (
          <span className="ml-1 rounded bg-accent/15 px-1 text-[10px] text-accent" title={log.model}>
            {t("首选")}
          </span>
        )}
        {/* actual_model 是重定向后真正打到上游的模型，和请求的不一样时必须说清 */}
        {log.actual_model && log.actual_model !== displayModel(log.model) && (
          <span className="ml-1 text-muted">→ {log.actual_model}</span>
        )}
      </td>
      {showSession && (
        <td className="truncate px-2 py-1 text-xs">
          {sessionId ? (
            <button
              // 点会话不该同时选中这一行 —— 那会在右侧弹出日志详情，把
              // 用户想去的会话页盖住。
              onClick={(e) => {
                e.stopPropagation();
                onOpenSession?.(sessionId);
              }}
              title={sessionId}
              className="max-w-full truncate rounded px-1 text-accent hover:underline"
            >
              {sessionTitles?.get(sessionId) ?? sessionId.slice(0, 8)}
            </button>
          ) : (
            <span className="text-muted/50">—</span>
          )}
        </td>
      )}
      <td className="truncate px-2 py-1 text-xs text-muted" title={log.channel_name}>
        {log.channel_name ?? "—"}
      </td>
      <td className="px-2 py-1 text-right text-xs tabular-nums text-muted">
        {fmtDuration(log.duration)}
      </td>
      <td className="px-2 py-1 text-right text-xs tabular-nums text-muted">
        {fmtDuration(log.first_byte_time)}
      </td>
      <td className="px-2 py-1 text-right text-xs tabular-nums text-muted">
        {fmtCompact(tokensIn)}
        <span className="mx-0.5 text-muted/50">/</span>
        {fmtCompact(log.output_tokens ?? 0)}
      </td>
      {/* 乘过倍率，和总览的「费用（倍率后）」同口径 —— 见 effectiveCost。 */}
      <td
        className="px-2 py-1 text-right text-xs tabular-nums"
        title={
          log.cost_multiplier != null && log.cost_multiplier !== 1
            ? `标准 ${fmtCost(log.cost)} × 渠道倍率 ${log.cost_multiplier}`
            : undefined
        }
      >
        {fmtCost(effectiveCost(log))}
      </td>
    </tr>
  );
}
