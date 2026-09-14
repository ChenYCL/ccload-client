import { useT } from "../../i18n";
import { useEffect, useRef, useState } from "react";
import { Radio } from "lucide-react";
import { activeOrigin } from "../../lib/logOrigin";
import type { ActiveRequest } from "../../types";
import { fmtCompact, fmtDuration, fmtSpeed } from "../formatters";

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
 * 每条进行中请求的瞬时速度（B/s）。
 *
 * `bytes_received` 是每次轮询（1.5s）的快照，速度只能由相邻两次快照的差分算出。
 * 差分天然带毛刺（一轮 0B、下一轮 80kB，点速度在 0 和 53kB/s 之间蹦），所以做
 * 指数平滑：speed = 0.6·旧 + 0.4·新点。快照间隔不足 300ms（同轮重渲染）不算，
 * 避免把同一个快照差分重复计入。
 *
 * 上游断流时点速度是 0，EMA 会把历史速度逐渐衰减到 0 —— 这正是想要的：「在动」
 * 的数字几秒内安静下来，和「已收 0B 不动了」的卡住观感一致。
 */
function useSpeeds(items: ActiveRequest[]) {
  const last = useRef(new Map<number, { bytes: number; at: number }>());
  const speeds = useRef(new Map<number, number>());
  const keep = useRef(new Set<number>());

  // 每次渲染都把当前快照喂进差分。写 ref 不触发渲染；数字走字靠 useTick。
  for (const r of items) {
    keep.current.add(r.id);
    const prev = last.current.get(r.id);
    const now = Date.now();
    if (prev) {
      const dt = (now - prev.at) / 1000;
      if (dt >= 0.3) {
        const bytes = Math.max(0, (r.bytes_received ?? 0) - prev.bytes);
        const point = bytes / dt;
        const old = speeds.current.get(r.id) ?? 0;
        speeds.current.set(r.id, old === 0 ? point : old * 0.6 + point * 0.4);
        last.current.set(r.id, { bytes: r.bytes_received ?? 0, at: now });
      }
    } else {
      last.current.set(r.id, { bytes: r.bytes_received ?? 0, at: now });
    }
  }
  // 掉出列表的请求清掉状态，防长会话下 map 无界长。
  if (keep.current.size !== last.current.size) {
    for (const id of [...last.current.keys()]) {
      if (!keep.current.has(id)) {
        last.current.delete(id);
        speeds.current.delete(id);
      }
    }
  }
  return speeds;
}

export function ActiveRequestsPanel({
  items,
  localIps,
}: {
  items: ActiveRequest[];
  /** 已确认属于本机的出口 IP，由历史日志那边学出来（见 logOrigin）。 */
  localIps?: ReadonlySet<string>;
}) {
  const t = useT();
  useTick(items.length > 0);
  const speeds = useSpeeds(items);

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
                {/* bytes_received 是快照值：它在涨说明上游还在吐，停住说明可能卡了。
                    速度是相邻快照的差分（EMA 平滑），见 useSpeeds。 */}
                <span>
                  {t("已收")} {fmtCompact(r.bytes_received ?? 0)}B
                  {(r.bytes_received ?? 0) > 0 && (speeds.current.get(r.id) ?? 0) >= 1 && (
                    <span className="ml-1.5 text-emerald-700">
                      {fmtSpeed(speeds.current.get(r.id))}
                    </span>
                  )}
                </span>
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
