import { useT } from "../../i18n";
import { AlertTriangle } from "lucide-react";
import { cn } from "../../lib/cn";
import { fmtClock, fmtInt, fmtPct } from "../formatters";
import type { ChannelAnomaly } from "./derive";

/// 异常面板：只有真的有失败时才出现。没有异常时整块不渲染 —— 一个常驻的
/// 「一切正常」空面板会稀释它出现时的信号强度。
///
/// 按渠道分组，而不是平铺「模型 × 渠道」。原因见 derive.ts::anomaliesOf：
/// last_request_message 是渠道口径的，平铺会把同一条错误重复安到五个模型头上。
/// 这里它只出现一次，并明确写成「该渠道最近一次请求」，不冒充某个模型的失败原因。

const MAX_CHANNELS = 4;
const MAX_MODELS = 5;
const MAX_MESSAGE = 200;

export function AnomalyPanel({ items }: { items: ChannelAnomaly[] }) {
  const t = useT();
  if (items.length === 0) return null;
  const shown = items.slice(0, MAX_CHANNELS);
  const failing = items.reduce((s, g) => s + g.models.length, 0);

  return (
    <section role="alert" className="card overflow-hidden border-amber-300/70 bg-amber-50/50 p-0">
      <header className="flex flex-wrap items-baseline gap-x-2 gap-y-1 border-b border-amber-200/70 px-4 py-2.5">
        <AlertTriangle className="h-4 w-4 shrink-0 self-center text-amber-600" />
        <h2 className="t-title text-amber-900">
          {items.length} 个渠道上有 {failing} 个模型正在失败
        </h2>
        <span className="text-[11px] text-amber-700/80">{t("成功率低于 90%")}</span>
      </header>

      <ul className="divide-y divide-amber-200/60">
        {shown.map((g) => (
          <li key={g.key} className="px-4 py-2.5">
            <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5">
              <span className="text-sm font-medium">{g.channel}</span>
              <span className="text-[11px] tabular-nums text-muted">
                该渠道合计 {fmtInt(g.error)}/{fmtInt(g.requests)} 次失败
              </span>
            </div>

            <ul className="mt-1.5 space-y-1">
              {g.models.slice(0, MAX_MODELS).map((m) => (
                <li key={m.model} className="flex items-baseline gap-2.5">
                  <span
                    className={cn(
                      "w-12 shrink-0 rounded px-1 py-0.5 text-center text-[11px] font-semibold tabular-nums",
                      m.rate === 0
                        ? "bg-red-500/14 text-red-700"
                        : "bg-amber-500/16 text-amber-800",
                    )}
                  >
                    {fmtPct(m.rate, 0)}
                  </span>
                  <span className="truncate font-mono text-xs">{m.model}</span>
                  <span className="shrink-0 text-[11px] tabular-nums text-muted">
                    {fmtInt(m.error)}/{fmtInt(m.requests)} 次失败
                  </span>
                </li>
              ))}
              {g.models.length > MAX_MODELS && (
                <li className="pl-[3.6rem] text-[11px] text-muted">
                  还有 {g.models.length - MAX_MODELS} 个模型
                </li>
              )}
            </ul>

            {/* 渠道级信息，措辞必须让人看出它不属于上面某一个模型。 */}
            {g.lastMessage && g.lastMessage !== "ok" && (
              <p className="mt-1.5 break-all rounded-md bg-amber-500/8 px-2 py-1.5 font-mono text-[11px] leading-snug text-amber-900/80">
                <span className="font-sans text-muted">
                  该渠道最近一次请求
                  {g.lastAt != null ? ` ${fmtClock(Math.round(g.lastAt / 1000))}` : ""}
                  {g.lastStatus ? ` · ${g.lastStatus}` : ""}：
                </span>{" "}
                {g.lastMessage.slice(0, MAX_MESSAGE)}
                {g.lastMessage.length > MAX_MESSAGE ? "…" : ""}
              </p>
            )}
          </li>
        ))}
      </ul>

      {items.length > MAX_CHANNELS && (
        <p className="border-t border-amber-200/60 px-4 py-2 text-[11px] text-amber-800/80">
          还有 {items.length - MAX_CHANNELS} 个渠道未列出，完整明细见下方模型表。
        </p>
      )}
    </section>
  );
}
