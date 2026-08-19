import { X } from "lucide-react";
import { cn } from "../../lib/cn";
import type { LogEntry } from "../../types";
import { Overlay } from "../Modal";
import {
  effectiveCost,
  fmtCost,
  fmtDuration,
  fmtInt,
  statusTone,
  TONE_BADGE,
} from "../formatters";

/// 单条日志详情。存在的主要理由是 `message` 字段 —— 失败时它往往是上游原样返回的
/// 错误体（配额、模型不存在、鉴权失败……），是排查里最有用的一段文本，而它在表格
/// 里放不下。其余字段按「排查时会一起看」分组，不按数据库列顺序排。

export function LogDetail({ log, onClose }: { log: LogEntry; onClose: () => void }) {
  // Esc 关闭：这是个覆盖层，键盘必须能退出。
  const tone = statusTone(log.status_code);
  const failed = tone === "warn" || tone === "bad";

  return (
    <Overlay onClose={onClose} className="flex justify-end">
      <div
        className="animate-scrim absolute inset-0 bg-content/20"
        onClick={onClose}
        aria-hidden
      />
      <aside
        role="dialog"
        aria-modal="true"
        aria-label="日志详情"
        className="material-modal animate-materialize relative flex h-full w-[26rem] max-w-full flex-col border-l border-border"
      >
        <header className="flex items-start justify-between gap-3 border-b border-border px-4 py-3">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span
                className={cn(
                  "rounded px-1.5 py-px text-xs font-medium tabular-nums",
                  TONE_BADGE[tone],
                )}
              >
                {log.status_code ?? "—"}
              </span>
              <h2 className="t-title truncate">{log.model ?? "（未知模型）"}</h2>
            </div>
            <p className="mt-0.5 text-[11px] tabular-nums text-muted">
              #{log.id} · {new Date(log.time * 1000).toLocaleString("zh-CN", { hour12: false })}
            </p>
          </div>
          <button
            onClick={onClose}
            aria-label="关闭"
            className="rounded-lg p-1 text-muted hover:bg-surface-2 hover:text-content"
          >
            <X className="h-4 w-4" />
          </button>
        </header>

        <div className="scroll-edge flex-1 overflow-auto px-4 py-3">
          {log.message && (
            <section className="mb-4">
              <h3 className="mb-1 text-[11px] text-muted">上游返回</h3>
              <pre
                className={cn(
                  "max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-lg border px-2.5 py-2 font-mono text-[11px] leading-relaxed",
                  failed
                    ? "border-red-200 bg-red-50 text-red-800"
                    : "border-border bg-surface-2 text-muted",
                )}
              >
                {log.message}
              </pre>
            </section>
          )}

          <Group title="路由">
            <Row k="渠道" v={log.channel_name ?? (log.channel_id ? `#${log.channel_id}` : "—")} />
            <Row k="实际模型" v={log.actual_model ?? log.model ?? "—"} mono />
            <Row k="上游地址" v={log.base_url ?? "—"} mono wrap />
            <Row k="客户端协议" v={log.client_protocol ?? "—"} />
            <Row k="上游协议" v={log.upstream_protocol ?? "—"} />
            <Row k="流式" v={log.is_streaming ? "是" : "否"} />
            {log.thinking_effort && <Row k="思考强度" v={log.thinking_effort} />}
          </Group>

          <Group title="耗时">
            <Row k="总耗时" v={fmtDuration(log.duration)} />
            <Row k="首字节" v={fmtDuration(log.first_byte_time)} />
          </Group>

          <Group title="Tokens">
            <Row k="输入" v={fmtInt(log.input_tokens)} />
            <Row k="输出" v={fmtInt(log.output_tokens)} />
            {log.reasoning_tokens != null && (
              <Row k="推理" v={fmtInt(log.reasoning_tokens)} />
            )}
            <Row k="缓存读取" v={fmtInt(log.cache_read_input_tokens)} />
            <Row k="缓存写入" v={fmtInt(log.cache_creation_input_tokens)} />
          </Group>

          <Group title="费用与来源">
            {/* logs.cost 是标准成本，实付要乘渠道倍率 —— 见 effectiveCost。 */}
            <Row k="费用（倍率后）" v={fmtCost(effectiveCost(log))} />
            {log.cost_multiplier != null && log.cost_multiplier !== 1 && (
              <>
                <Row k="标准成本" v={fmtCost(log.cost)} />
                <Row k="渠道倍率" v={`× ${log.cost_multiplier}`} />
              </>
            )}
            <Row k="令牌" v={log.auth_token_description ?? "—"} />
            <Row k="API Key" v={log.api_key_used ?? "—"} mono />
            <Row k="客户端 IP" v={log.client_ip ?? "—"} mono />
            <Row k="来源" v={log.log_source ?? "—"} />
          </Group>
        </div>
      </aside>
    </Overlay>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="mb-4">
      <h3 className="mb-1 text-[11px] text-muted">{title}</h3>
      <dl className="space-y-1 rounded-lg border border-border bg-surface-raised px-2.5 py-2 text-xs">
        {children}
      </dl>
    </section>
  );
}

function Row({ k, v, mono, wrap }: { k: string; v: string; mono?: boolean; wrap?: boolean }) {
  return (
    <div className="flex justify-between gap-3">
      <dt className="shrink-0 text-muted">{k}</dt>
      <dd
        className={cn(
          "text-right tabular-nums",
          mono && "font-mono text-[11px]",
          wrap ? "break-all" : "truncate",
        )}
        title={v}
      >
        {v}
      </dd>
    </div>
  );
}
