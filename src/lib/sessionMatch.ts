import type { LogEntry, ProxyRecord } from "../types";

/// 把内核日志和代理记录对上，好在日志行上显示「这条是哪个会话发的」。
///
/// 为什么要匹配而不是直接读：内核的日志记录里【没有】session_id，也没有任何
/// 上游 request id（实测 1000 条里 0 条含 `req_`），而会话标识只有我们自己的
/// 代理那一层看得到。两边唯一的公共坐标就是「时间 + 模型名」。
///
/// 所以这是**启发式**，不是精确关联。判据：
///   * 模型名要对上 —— 代理记的是 CLI 发来的原名，内核记的可能是改写后的名字，
///     所以两边都试（`model` 和 `sent_model`）；
///   * 时间要挨得够近 —— 代理在**收到请求**时打点，内核在**完成**后记录，
///     两者差的是整个请求耗时，所以窗口要能容下最慢的那次调用。
///
/// 匹配不上就不显示，宁可空着也不要标错会话。

/// 代理打点在请求开始、内核记录在请求结束，差值最大就是一次调用的耗时。
/// 实测最慢的流式回答跑到 80 多秒，留 180s 才不会把长回答漏掉。
const MAX_SKEW_SECONDS = 180;

/// 一条日志对应的会话 id，匹配不上就是 undefined。
export function matchSessions(
  logs: LogEntry[],
  records: ProxyRecord[],
): Map<number, string> {
  const out = new Map<number, string>();
  if (records.length === 0) return out;

  // 同一个会话会连着发很多请求，按模型分桶能把候选集缩到很小。
  const byModel = new Map<string, ProxyRecord[]>();
  for (const r of records) {
    if (!r.session_id) continue;
    for (const name of [r.model, r.sent_model]) {
      if (!name) continue;
      const bucket = byModel.get(name);
      if (bucket) bucket.push(r);
      else byModel.set(name, [r]);
    }
  }

  // 一条代理记录只认领一条日志：同一会话连发多次时，不这样做会让所有日志
  // 都贴上最近那一条的会话，看起来「全中」其实是重复计数。
  const claimed = new Set<ProxyRecord>();

  for (const log of logs) {
    const names = [log.model, log.actual_model].filter(Boolean) as string[];
    let best: ProxyRecord | undefined;
    let bestGap = Infinity;

    for (const name of names) {
      for (const r of byModel.get(name) ?? []) {
        if (claimed.has(r)) continue;
        // 代理先于内核，所以只接受「代理时间 <= 日志时间」这个方向；
        // 反过来的差值必然是另一次请求。
        const gap = log.time - r.time;
        if (gap < 0 || gap > MAX_SKEW_SECONDS) continue;
        if (gap < bestGap) {
          bestGap = gap;
          best = r;
        }
      }
    }

    if (best?.session_id) {
      claimed.add(best);
      out.set(log.id, best.session_id);
    }
  }
  return out;
}
