import { useT } from "../../i18n";
import { AlertTriangle } from "lucide-react";
import { cliLabel } from "../../lib/logOrigin";
import { displayModel } from "../../lib/pins";
import type { ProxyRecord } from "../../types";
import { cn } from "../../lib/cn";
import { fmtClock, statusTone, TONE_BADGE } from "../formatters";

/// 代理见过、内核历史日志里没有的失败请求。
///
/// 这一块的存在理由是「盲区」两个字：内核在选完渠道之后才写日志，所以请求体读
/// 超时（408）、体积超限（413）这类在那之前就失败的请求，历史日志里一行都没有；
/// 代理连不上内核时（502）更是连内核都没到。以前这些请求在界面上完全隐形 ——
/// CLI 报错，日志页却干干净净，只能去翻 CLI 自己的输出。
///
/// 判定在 `unloggedFailures` 里，故意保守到宁可漏报。

/// 常见盲区状态码的人话解释。查不到的就只显示状态码，不瞎猜。
function explain(status: number, t: (s: string) => string): string | undefined {
  switch (status) {
    case 408:
      return t("请求体没能在内核的读取超时内传完（http_read_timeout_seconds）");
    case 413:
      return t("请求体超过体积上限（max_body_bytes）");
    case 502:
      return t("代理连不上内核，或响应中途断了");
    case 504:
      return t("内核没在超时内完成握手");
    default:
      return undefined;
  }
}

export function UnloggedFailures({
  records,
  sessionTitles,
  onOpenSession,
}: {
  records: ProxyRecord[];
  sessionTitles?: ReadonlyMap<string, string>;
  onOpenSession?: (sessionId: string) => void;
}) {
  const t = useT();
  if (records.length === 0) return null;

  return (
    <ul className="space-y-1.5">
      {records.map((r, i) => {
        const why = explain(r.status, t);
        return (
          <li
            // 代理记录没有 id，同一秒内同一个 CLI 也可能有多条，只能靠下标补齐。
            key={`${r.time}-${r.cli}-${i}`}
            className="flex items-center gap-3 rounded-lg border border-amber-200/70 bg-amber-50/40 px-3 py-2"
          >
            <AlertTriangle className="h-3.5 w-3.5 shrink-0 text-amber-600" />
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                <span
                  className={cn(
                    "inline-block rounded px-1.5 py-px text-[11px] font-medium tabular-nums",
                    TONE_BADGE[statusTone(r.status)],
                  )}
                >
                  {r.status}
                </span>
                <span className="truncate font-mono text-xs">
                  {displayModel(r.sent_model ?? r.model) ?? r.path}
                </span>
                <span className="text-[11px] text-muted">{cliLabel(r.cli) ?? r.cli}</span>
                {r.session_id && (
                  <button
                    onClick={() => onOpenSession?.(r.session_id!)}
                    title={r.session_id}
                    className="max-w-[12rem] truncate rounded text-[11px] text-accent hover:underline"
                  >
                    {sessionTitles?.get(r.session_id) ?? r.session_id.slice(0, 8)}
                  </button>
                )}
              </div>
              {why && <div className="mt-0.5 text-[11px] text-muted">{why}</div>}
            </div>
            <span className="shrink-0 text-xs tabular-nums text-muted">{fmtClock(r.time)}</span>
          </li>
        );
      })}
    </ul>
  );
}
