import type { StatsEntry } from "../../types";

/// 总览页的所有派生计算集中在这里，页面组件只负责摆放。
///
/// 口径说明（重要，别在别处重算）：
///   · /admin/stats 的一行粒度是「渠道 × 模型」，同一个模型会出现多次，
///     所以任何「按模型」的数字都必须先按 model 聚合，不能直接取某一行。
///   · 费用有两个字段：total_cost 是标准价，effective_cost 是乘过渠道
///     cost_multiplier 之后的实付价。展示以 effective_cost 为准。
///   · total 字段内核已经给了，但这里一律用 success + error 重算 ——
///     两者在实测数据里一致，重算能让「卡片数字」和「成功率分母」永远同源。

export type Totals = {
  requests: number;
  success: number;
  error: number;
  /** 乘过渠道倍率的实付费用 */
  effectiveCost: number;
  /** 未乘倍率的标准费用 */
  standardCost: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  /** 没有样本时为 -1，和 HealthPoint.rate 的约定保持一致 */
  rate: number;
};

export function totalsOf(rows: StatsEntry[]): Totals {
  const t: Totals = {
    requests: 0,
    success: 0,
    error: 0,
    effectiveCost: 0,
    standardCost: 0,
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheCreationTokens: 0,
    rate: -1,
  };
  for (const r of rows) {
    t.success += r.success ?? 0;
    t.error += r.error ?? 0;
    t.effectiveCost += r.effective_cost ?? 0;
    t.standardCost += r.total_cost ?? 0;
    t.inputTokens += r.total_input_tokens ?? 0;
    t.outputTokens += r.total_output_tokens ?? 0;
    t.cacheReadTokens += r.total_cache_read_input_tokens ?? 0;
    t.cacheCreationTokens += r.total_cache_creation_input_tokens ?? 0;
  }
  t.requests = t.success + t.error;
  if (t.requests > 0) t.rate = t.success / t.requests;
  return t;
}

/** 四类 token 之和。它们是并列的计量口径，各自单价不同（缓存读通常是输入价的
 *  十分之一），但「一共处理了多少 token」只有加起来才答得上。 */
export function totalTokens(t: Totals): number {
  return t.inputTokens + t.outputTokens + t.cacheReadTokens + t.cacheCreationTokens;
}

/** 按请求数加权求平均。算术平均会让一个 1 次请求的行和一个 500 次请求的行等权，
 *  读出来的数字没有意义。取不到样本时返回 undefined —— 不是 0。 */
function weightedAvg(
  rows: StatsEntry[],
  pick: (r: StatsEntry) => number | undefined,
): number | undefined {
  let sum = 0;
  let weight = 0;
  for (const r of rows) {
    const v = pick(r);
    const n = (r.success ?? 0) + (r.error ?? 0);
    if (!v || n <= 0) continue;
    sum += v * n;
    weight += n;
  }
  return weight > 0 ? sum / weight : undefined;
}

export type ModelRow = Totals & {
  model: string;
  /** 这个模型跑在几个渠道上 */
  channels: number;
  /** 请求数加权的平均首字节时间，秒；没有样本时 undefined */
  firstByte?: number;
};

/** 按模型聚合。 */
export function byModel(rows: StatsEntry[]): ModelRow[] {
  const acc = new Map<string, { rows: StatsEntry[]; channels: Set<number | string> }>();
  for (const r of rows) {
    const key = r.model ?? "（未知模型）";
    const cur = acc.get(key) ?? { rows: [], channels: new Set() };
    cur.rows.push(r);
    cur.channels.add(r.channel_id ?? r.channel_name ?? "?");
    acc.set(key, cur);
  }

  return [...acc.entries()]
    .map(([model, v]) => ({
      model,
      channels: v.channels.size,
      firstByte: weightedAvg(v.rows, (r) => r.avg_first_byte_time_seconds),
      ...totalsOf(v.rows),
    }))
    .sort((a, b) => b.requests - a.requests);
}

export type ChannelRow = Totals & {
  /** 和 channel_health 的 key 同一口径：channel_id 的字符串形式 */
  key: string;
  channel: string;
  /** 这个渠道上跑过几个模型 */
  models: number;
  /** 请求数加权的平均耗时 / 首字节时间，秒；没有样本时 undefined */
  duration?: number;
  firstByte?: number;
};

/**
 * 按渠道聚合 —— 「这个月的钱花在哪一家上了」。
 *
 * 和 byModel 是同一批行的另一个切法：stats 的粒度是「渠道 × 模型」，按谁聚合
 * 就得到谁的口径。两者的费用总和必然相等，对不上就是聚合写错了。
 *
 * 排序按实付费用，费用全为 0（本地/免费渠道）时自动退到请求数 —— 一排等长的
 * 零费用条排不出先后，那时候用户真正在比的是谁跑得多。
 */
export function byChannel(rows: StatsEntry[]): ChannelRow[] {
  const acc = new Map<string, { rows: StatsEntry[]; name: string; models: Set<string> }>();
  for (const r of rows) {
    const key = String(r.channel_id ?? r.channel_name ?? "?");
    const cur = acc.get(key) ?? {
      rows: [],
      name: r.channel_name ?? `渠道 #${r.channel_id ?? "?"}`,
      models: new Set<string>(),
    };
    cur.rows.push(r);
    cur.models.add(r.model ?? "（未知模型）");
    acc.set(key, cur);
  }

  return [...acc.entries()]
    .map(([key, v]) => ({
      key,
      channel: v.name,
      models: v.models.size,
      duration: weightedAvg(v.rows, (r) => r.avg_duration_seconds),
      firstByte: weightedAvg(v.rows, (r) => r.avg_first_byte_time_seconds),
      ...totalsOf(v.rows),
    }))
    .sort((a, b) => b.effectiveCost - a.effectiveCost || b.requests - a.requests);
}

export type AnomalyModel = {
  model: string;
  requests: number;
  error: number;
  rate: number;
};

export type ChannelAnomaly = {
  key: string;
  channel: string;
  /** 该渠道下成功率低于阈值的模型，最差的在前 */
  models: AnomalyModel[];
  /** 该渠道全部模型的合计，用来说明「这个渠道整体有多糟」 */
  requests: number;
  error: number;
  /** 渠道口径的最近一次请求 —— 不是某个模型的，见 types.ts 里 StatsEntry 的注释 */
  lastStatus?: number;
  lastMessage?: string;
  lastAt?: number;
};

/**
 * 挑出正在出问题的组合，并**按渠道分组**。
 *
 * 分组不是为了排版好看，而是因为 last_request_status / last_request_message 在
 * 不带 model 筛选时是渠道口径的（内核会把渠道最近一次请求复制给该渠道的每一行）。
 * 平铺成「模型 → 错误信息」会把一条渠道级的错误安到五个模型头上，看起来像五个
 * 独立故障，实际是同一个。分组后这条信息只出现一次，且能如实标成「该渠道最近一次请求」。
 *
 * 成功率本身是逐行真实的（success / error 就是这一行渠道 × 模型的计数），所以
 * 每个模型的百分比照常逐个列出。
 *
 * 门槛只要求 error > 0 且成功率低于 90%，不设最小样本量 —— 一个刚配错的渠道
 * 往往只有一两次请求且全挂，恰恰最该被看见。样本数原样展示，让用户自己判断。
 */
export function anomaliesOf(rows: StatsEntry[]): ChannelAnomaly[] {
  const groups = new Map<string, ChannelAnomaly>();

  for (const r of rows) {
    const key = String(r.channel_id ?? r.channel_name ?? "?");
    const success = r.success ?? 0;
    const error = r.error ?? 0;
    const requests = success + error;

    let g = groups.get(key);
    if (!g) {
      g = {
        key,
        channel: r.channel_name ?? `渠道 #${r.channel_id ?? "?"}`,
        models: [],
        requests: 0,
        error: 0,
        lastStatus: r.last_request_status,
        lastMessage: r.last_request_message,
        lastAt: r.last_request_at,
      };
      groups.set(key, g);
    }
    // 合计覆盖渠道的所有模型，健康的也算进去，否则失败率会被夸大。
    g.requests += requests;
    g.error += error;

    if (requests === 0 || error === 0) continue;
    const rate = success / requests;
    if (rate >= 0.9) continue;
    g.models.push({ model: r.model ?? "（未知模型）", requests, error, rate });
  }

  return [...groups.values()]
    .filter((g) => g.models.length > 0)
    .map((g) => ({
      ...g,
      models: g.models.sort((a, b) => a.rate - b.rate || b.error - a.error),
    }))
    // 全挂的模型多的渠道排最前，其次按失败总数——影响面大的先看。
    .sort(
      (a, b) =>
        b.models.filter((m) => m.rate === 0).length -
          a.models.filter((m) => m.rate === 0).length || b.error - a.error,
    );
}
