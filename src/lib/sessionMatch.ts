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
///
/// 配对结果还被「来源」列复用（见 logOrigin.ts）：配上代理记录就说明这条是本机
/// 某个 CLI 发的。所以这里对**所有**代理记录配对，不只是带会话 id 的那些 —— 一条
/// 没带会话头的请求也占着它自己那条日志，把它排除掉会让旁边那条别的会话被错认
/// 领走。

/// 代理打点在请求开始、内核记录在请求结束，差值最大就是一次调用的耗时。
/// 实测最慢的流式回答跑到 80 多秒，留 180s 才不会把长回答漏掉。
const MAX_SKEW_SECONDS = 180;

/// 代理在收到响应后才落记录，而日志每 2.5s 才轮询一次；这段窗口里「记录已在、
/// 日志还没拉回来」是常态，不是盲区。
const GRACE_SECONDS = 8;

/// 一条日志对应的代理记录，匹配不上就没有这个 key。
export function matchRecords(
  logs: LogEntry[],
  records: ProxyRecord[],
): Map<number, ProxyRecord> {
  const out = new Map<number, ProxyRecord>();
  if (records.length === 0) return out;

  // 同一个会话会连着发很多请求，按模型分桶能把候选集缩到很小。
  const byModel = new Map<string, ProxyRecord[]>();
  for (const r of records) {
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

    if (best) {
      claimed.add(best);
      out.set(log.id, best);
    }
  }
  return out;
}

/// 会话归因：`matchRecords` 的结果里取会话 id。没带会话头的记录配上了也没有
/// 会话可显示，这一行就空着。
export function matchSessions(
  logs: LogEntry[],
  records: ProxyRecord[],
): Map<number, string> {
  const out = new Map<number, string>();
  for (const [id, rec] of matchRecords(logs, records)) {
    if (rec.session_id) out.set(id, rec.session_id);
  }
  return out;
}

/// 代理记下了、内核历史日志里却找不到的**失败**请求。
///
/// 内核只在选完渠道、发起 attempt 之后才写日志。请求体读超时（408，
/// `http_read_timeout_seconds`）、体积超限（413）这类失败发生在那之前，
/// 内核**永远不会**为它们留下日志行；代理连不上内核时更是连内核都没到。
/// 结果就是：CLI 那边报了错，历史日志里翻不到任何痕迹。这个函数把这段盲区
/// 捞出来 —— 代理是唯一见过这些请求的一方。
///
/// 判据故意保守，宁可漏报也不误报：
///   * 只看 `status >= 400`。成功的记录没配上，多半只是配对没中或被筛掉了，
///     那是显示问题，不是故障。
///   * 只看落在**已取回日志时间范围内**的记录。代理的环形缓冲比日志页的
///     200 条窗口长得多，更早的记录没有日志可对，不能算“内核漏了”。
///   * 给一段宽限期。代理在收到响应后才落记录，而日志每 2.5s 才轮询一次，
///     刚发生的失败会有一小段“记录已在、日志还没拉回来”的窗口。
///   * 附近有同状态码的日志就跳过。配对偶尔会错认，而这类盲区状态码
///     （408/413/502）本来就不会出现在日志里，同码即在册，说明是配对没中。
export function unloggedFailures(
  logs: LogEntry[],
  records: ProxyRecord[],
  matched: ReadonlyMap<number, ProxyRecord>,
  nowSeconds: number,
): ProxyRecord[] {
  if (logs.length === 0 || records.length === 0) return [];
  const claimed = new Set(matched.values());
  const oldestLog = Math.min(...logs.map((l) => l.time));
  const cutoff = nowSeconds - GRACE_SECONDS;

  return records
    .filter((r) => r.status >= 400)
    .filter((r) => !claimed.has(r))
    .filter((r) => r.time >= oldestLog && r.time <= cutoff)
    .filter(
      (r) =>
        !logs.some(
          (l) => l.status_code === r.status && Math.abs(l.time - r.time) <= MAX_SKEW_SECONDS,
        ),
    )
    .sort((a, b) => b.time - a.time);
}
