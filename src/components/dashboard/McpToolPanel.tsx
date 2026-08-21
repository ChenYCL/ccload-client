import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { cn } from "../../lib/cn";
import { useT } from "../../i18n";
import { fmtInt } from "../formatters";
import type { McpToolStat } from "../../types";

/// 自带 MCP 工具的调用统计。
///
/// 口径边界必须写在脸上：这里只有客户端自带的两个服务器（`ccload-vision` /
/// `ccload-image`）的数据。别家
/// MCP 服务器是 CLI 直接拉起的独立进程，既不经过内核也不经过本客户端 ——
/// 我们没有任何位置能看见它们的调用。把标题写成「MCP 调用」会让人以为
/// 装在扩展管理里的那些服务器也在统计里，那是骗人。
///
/// 数据来自各 MCP 进程自己追加的 JSONL 流水（见 services/mcp_usage.rs），
/// 所以它跨会话、跨 CLI 累计，和内核的请求日志是两套东西：内核只看得见一次
/// 普通的 /v1/messages，分不清是用户在对话还是 describe_image 在看图。

/// 工具名 → 人话。名字本身是给模型看的（英文、动词开头），给人看要换一套，
/// 而且要跟随语言开关 —— 中文界面里挂一串 `describe_image / 看图` 是一回事，
/// 英文界面里挂一串中文就成了噪声。
const TOOL_LABELS: Record<string, string> = {
  describe_image: "看图",
  read_image_text: "抄图上的字",
  compare_images: "比对两张图",
  list_pasted_images: "列出刚贴的图",
  describe_screen: "看当前屏幕",
  generate_image: "生成图片",
  edit_image: "修改图片",
};

/** 毫秒 → 人话。工具调用普遍是秒级，不必像请求耗时那样精确到毫秒。 */
function fmtMs(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const total = Math.round(ms / 1000);
  return `${Math.floor(total / 60)}m${String(total % 60).padStart(2, "0")}s`;
}

function fmtSince(unixSec: number): string {
  if (!unixSec) return "";
  return new Date(unixSec * 1000).toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

export function McpToolPanel() {
  const t = useT();
  const qc = useQueryClient();
  const usage = useQuery({
    queryKey: ["mcp-usage"],
    queryFn: api.mcpUsageStats,
    refetchInterval: 30_000,
  });
  const clear = useMutation({
    mutationFn: api.mcpUsageClear,
    onSuccess: () => qc.invalidateQueries({ queryKey: ["mcp-usage"] }),
  });

  const data = usage.data;
  const tools = data?.tools ?? [];
  const max = Math.max(1, ...tools.map((t) => t.calls));

  if (!data || data.calls === 0) {
    return (
      <p className="text-sm text-muted">
        {t(
          "还没有调用记录。装上「模型导入」页里的视觉辅助 MCP 之后，文本模型每次看图都会记一笔。",
        )}
      </p>
    );
  }

  return (
    <div>
      <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1 text-sm">
        <span>
          {t("共 {n} 次调用", { n: fmtInt(data.calls) })}
        </span>
        <span className="text-muted">
          {t("累计耗时 {d}", { d: fmtMs(data.total_ms) })}
        </span>
        {data.failed > 0 && (
          <span className="text-red-600">{t("失败 {n} 次", { n: fmtInt(data.failed) })}</span>
        )}
        <button
          onClick={() => clear.mutate()}
          disabled={clear.isPending}
          className="ml-auto rounded-lg border border-border bg-surface-raised px-2 py-0.5 text-xs text-muted hover:bg-surface-2 disabled:opacity-40"
        >
          {t("清空统计")}
        </button>
      </div>

      <ul className="mt-3 space-y-2">
        {tools.map((s) => (
          <ToolRow key={s.tool} t={s} max={max} />
        ))}
      </ul>

      <p className="mt-3 text-[11px] text-muted/80">
        {t(
          "只统计本客户端自带的 MCP 服务器（ccload-vision / ccload-image）。扩展管理里装的第三方 MCP 由 CLI 直接拉起，不经过内核也不经过客户端，无法计入。",
        )}
        {data.since > 0 && ` ${t("统计自 {at}。", { at: fmtSince(data.since) })}`}
        {data.truncated && ` ${t("更早的记录已被丢弃（流水有大小上限）。")}`}
      </p>
    </div>
  );
}

function ToolRow({ t: s, max }: { t: McpToolStat; max: number }) {
  const t = useT();
  const label = TOOL_LABELS[s.tool];
  return (
    <li>
      <div className="flex flex-wrap items-baseline gap-x-2 text-xs">
        <span className="font-mono text-[11px]">{s.tool}</span>
        {label && <span className="text-muted">{t(label)}</span>}
        <span className="ml-auto tabular-nums">{t("{n} 次", { n: fmtInt(s.calls) })}</span>
        {/* 平均耗时只算成功的调用：失败多是「文件不存在」这种 1ms 就返回的，
            混进来会把均值压到看不出真实开销。 */}
        <span className="tabular-nums text-muted" title={t("平均耗时（仅成功调用）")}>
          {t("均 {d}", { d: fmtMs(s.avg_ms) })}
        </span>
        <span className="tabular-nums text-muted" title={t("最慢一次")}>
          {t("峰 {d}", { d: fmtMs(s.max_ms) })}
        </span>
        {s.failed > 0 && (
          <span className="tabular-nums text-red-600">
            {t("失败 {n}", { n: fmtInt(s.failed) })}
          </span>
        )}
      </div>
      <div className="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-surface-2">
        <div
          className={cn("h-full rounded-full", s.failed > 0 ? "bg-amber-500" : "bg-accent")}
          style={{ width: `${(s.calls / max) * 100}%` }}
        />
      </div>
    </li>
  );
}
