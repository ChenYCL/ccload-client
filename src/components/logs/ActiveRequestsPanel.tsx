import { useT } from "../../i18n";
import { useEffect, useRef, useState } from "react";
import { Radio } from "lucide-react";
import { activeOrigin } from "../../lib/logOrigin";
import type { ActiveRequest, TokenTick } from "../../types";
import { fmtCompact, fmtDuration, fmtTokensPerSec } from "../formatters";

/// 进行中的请求。这是整个界面里唯一真正实时的一块：内核把在飞的请求放在内存里
/// （`/admin/active-requests`），而 `/admin/logs` 只有请求**结束**后才有记录 ——
/// 一个跑了两分钟的流式请求，在日志表里整整两分钟都是不存在的。
///
/// 已耗时必须由前端每秒自己算（Date.now() - start_time），不能等下一次轮询：
/// 1.5 秒才跳一次的秒表会被读成「卡住了」。

/** 每秒一次的重渲染节拍，只为了让已耗时走字。 */
function useTick(active: boolean) {
  const [, setN] = useState(0);
  useEffect(() => {
    if (!active) return;
    const id = window.setInterval(() => setN((n) => n + 1), 1000);
    return () => window.clearInterval(id);
  }, [active]);
}

/**
 * 按落点模型名配对的 tok/s。
 *
 * 数据来自代理的 SSE 观测（`TokenTick`）：`output_tokens` 是累计值，tok/s =
 * 相邻两笔的差分 ÷ 时间差。做指数平滑压毛刺；超过 10s 没有新观测（流已断或
 * 模型在慢思考，没吐新 token）就不显示速度，别让人把 0 读成「坏了」。
 *
 * 键是模型名而不是请求 id：内核的进行中列表没有我们的会话 id，跨代理配对
 * 只落在「模型」这个两边都有的字段上。同名并发会互相串速 —— 两台机器同时
 * 跑同名模型时显示的是合计速度，可接受；精度换覆盖面的取舍。
 */
function useTokenSpeeds(ticks: TokenTick[]) {
  const prev = useRef(new Map<string, { tokens: number; at: number }>());
  const speeds = useRef(new Map<string, number>());
  const keep = useRef(new Set<string>());

  for (const t of ticks) {
    keep.current.add(t.model);
    const p = prev.current.get(t.model);
    const now = Date.now();
    if (p) {
      const dt = (now - p.at) / 1000;
      // 30s 无观测的条目后端已清，这里防的是同一轮询周期内的重复计算。
      if (dt >= 1) {
        const d = Math.max(0, t.outputTokens - p.tokens);
        const point = d / dt;
        const old = speeds.current.get(t.model) ?? 0;
        speeds.current.set(t.model, old === 0 ? point : old * 0.6 + point * 0.4);
        prev.current.set(t.model, { tokens: t.outputTokens, at: now });
      } else if (now - p.at > 10_000) {
        // 观测断了：速度不再可信，等下一笔重新起算。
        speeds.current.delete(t.model);
        prev.current.set(t.model, { tokens: t.outputTokens, at: now });
      }
    } else {
      prev.current.set(t.model, { tokens: t.outputTokens, at: now });
    }
  }
  if (keep.current.size !== prev.current.size) {
    for (const k of [...prev.current.keys()]) {
      if (!keep.current.has(k)) {
        prev.current.delete(k);
        speeds.current.delete(k);
      }
    }
  }
  return speeds;
}

export function ActiveRequestsPanel({
  items,
  localIps,
  ticks,
}: {
  items: ActiveRequest[];
  /** 已确认属于本机的出口 IP，由历史日志那边学出来（见 logOrigin）。 */
  localIps?: ReadonlySet<string>;
  /** 代理的 SSE token 观测，按落点模型名配对。经代理的请求才有。 */
  ticks: TokenTick[];
}) {
  const t = useT();
  useTick(items.length > 0);
  const speeds = useTokenSpeeds(ticks);

  // 空态和「一条进行中」占一样高。请求每 1.5s 来去一次，如果空态塌成一行、
  // 有请求时撑成三行，整页会跟着上下跳 —— 那就是肉眼看到的「闪屏」。
  // 这里给容器一个下限高度，让它只在超过一条时才增高。
  if (items.length === 0) {
    return (
      <div className="flex min-h-[3.25rem] items-center gap-2 text-sm text-muted">
        <span className="h-1.5 w-1.5 rounded-full bg-border" />
        {t("当前没有进行中的请求")}
      </div>
    );
  }

  const now = Date.now();
  return (
    <ul className="min-h-[3.25rem] space-y-1.5">
      {items.map((r) => {
        // start_time 是 unix 毫秒。时钟有偏差时可能算出负数，钳到 0。
        const elapsed = Math.max(0, (now - r.start_time) / 1000);
        const landing = r.model ?? "";
        const speed = speeds.current.get(landing) ?? 0;
        // 慢思考阶段几分钟不吐 token 是常态：速度只在有读数时显示，
        // 显示时配「已收字节」一起看，停了还是动了一目了然。
        const tick = ticks.find((x) => x.model === landing);
        return (
          <li
            key={r.id}
            className="animate-materialize flex items-center gap-3 rounded-lg border border-emerald-200/70 bg-emerald-50/40 px-3 py-2"
          >
            <Radio className="h-3.5 w-3.5 shrink-0 animate-pulse text-emerald-600" />

            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                <span className="truncate font-mono text-xs font-medium">
                  {r.model ?? t("（未知模型）")}
                </span>
                <span className="text-[11px] text-muted">
                  {t("经")} {r.channel_name ?? `#${r.channel_id ?? "?"}`}
                </span>
                {r.upstream_status && (
                  <span className="rounded bg-emerald-500/12 px-1.5 py-px text-[10px] text-emerald-700">
                    {r.upstream_status}
                  </span>
                )}
                {r.is_streaming && (
                  <span className="rounded bg-surface-2 px-1.5 py-px text-[10px] text-muted">
                    stream
                  </span>
                )}
                {r.upstream_websocket && (
                  <span className="rounded bg-surface-2 px-1.5 py-px text-[10px] text-muted">
                    ws
                  </span>
                )}
                {/* 内存态里没有令牌描述，也来不及和代理记录配对，所以这里只回答
                    「本机还是别人」；具体是哪个 CLI 等它落进历史日志再看。 */}
                {activeOrigin(r.client_ip, localIps ?? new Set()) === "remote" && (
                  <span
                    title={`${t("另一台机器")} · ${r.client_ip}`}
                    className="rounded bg-amber-500/15 px-1.5 py-px text-[10px] text-amber-700"
                  >
                    {t("远端")}
                  </span>
                )}
              </div>
              <div className="mt-0.5 flex flex-wrap gap-x-3 text-[11px] tabular-nums text-muted">
                {/* bytes_received 是快照值：它在涨说明上游还在吐，停住说明可能卡了。 */}
                <span>
                  {t("已收")} {fmtCompact(r.bytes_received ?? 0)}B
                </span>
                {speed >= 1 && tick && (
                  <span className="text-emerald-700">
                    {fmtTokensPerSec(speed)}
                    {tick.outputTokens > 0 &&
                      ` · ${t("已出")} ${fmtCompact(tick.outputTokens)}`}
                  </span>
                )}
                {r.client_first_byte_time != null && r.client_first_byte_time > 0 && (
                  <span>{t("首字节")} {fmtDuration(r.client_first_byte_time)}</span>
                )}
                {/* 上游协议 —— 内核用哪种 API 跟上游说话（anthropic / codex …），
                    不是发起请求的 CLI 名字。 */}
                {r.upstream_protocol && (
                  <span title={t("上游协议")}>{r.upstream_protocol}</span>
                )}
              </div>
            </div>

            <span className="shrink-0 text-sm font-medium tabular-nums text-emerald-700">
              {fmtDuration(elapsed)}
            </span>
          </li>
        );
      })}
    </ul>
  );
}
