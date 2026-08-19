/// 模型别名与上游真实模型 ID 的匹配。
///
/// 为什么需要模糊匹配：渠道里的别名是用户/内核起的，上游返回的是它自己的 ID，
/// 两者经常只差一层包装 —— `anthropic/claude-opus-4.1` vs `claude-opus-4.1`、
/// `claude-opus-4.1:batch` vs `claude-opus-4.1`、`GLM-4.6` vs `glm-4.6`。
/// 只做全等比较的话，一大半能用的模型会被判成「上游没有」。
///
/// 匹配只用来**给建议**（哪些行值得勾选），不改任何写入内容：勾中的仍然按别名
/// 原样写进 CLI，因为真正做重定向的是内核里的渠道模型表，不是这里。

/// 归一化：小写 → 去掉 provider 前缀 → 去掉 `:后缀` → 去掉所有非字母数字。
///
/// 前缀只去最后一段：`amazon/nova-2-lite-v1` → `nova2litev1`。
/// `:batch` / `:thinking` 这类后缀是同一个模型的不同调用方式，上游的模型清单
/// 里通常只有裸名。
export function normalizeModelId(raw: string): string {
  const lower = raw.trim().toLowerCase();
  const lastSegment = lower.slice(lower.lastIndexOf("/") + 1);
  const withoutSuffix = lastSegment.split(":")[0];
  return withoutSuffix.replace(/[^a-z0-9]/g, "");
}

export type MatchLevel = "exact" | "fuzzy" | "missing";

export type MatchResult = {
  level: MatchLevel;
  /** 命中的上游 ID；`missing` 时为 null。 */
  upstreamId: string | null;
};

/// 建索引再逐个查，避免 472 × N 的两层遍历。
export function buildUpstreamIndex(upstreamIds: string[]) {
  const exact = new Set(upstreamIds);
  const byNormalized = new Map<string, string>();
  for (const id of upstreamIds) {
    const key = normalizeModelId(id);
    // 同一归一化键有多个上游 ID 时保留第一个：它们本就是同一个模型的不同写法，
    // 展示哪个都行，重要的是「上游确实有」。
    if (!byNormalized.has(key)) byNormalized.set(key, id);
  }
  return { exact, byNormalized };
}

export function matchAlias(
  alias: string,
  index: ReturnType<typeof buildUpstreamIndex>,
): MatchResult {
  if (index.exact.has(alias)) return { level: "exact", upstreamId: alias };
  const hit = index.byNormalized.get(normalizeModelId(alias));
  return hit ? { level: "fuzzy", upstreamId: hit } : { level: "missing", upstreamId: null };
}
