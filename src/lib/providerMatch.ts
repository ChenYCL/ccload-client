/// 按名称/URL 把 provider 猜到内核里已有的渠道上。
///
/// 调度图要求 5 家各绑一个渠道，手动一个个选很烦，而机器上的渠道名往往已经带着
/// 线索（`Anthropic`、`xAI`、`Z.ai-CodingPlan`）。这里做的是**建议**，不是
/// 自动生效 —— 匹配完仍然填进下拉框由用户确认，猜错了改一下就行。
///
/// 只看名称是不够的：中转站常起个跟厂商无关的名字，但它的 URL 里一般
/// 有线索。所以名称和 URL 一起参与匹配。

export type ChannelLite = {
  id?: number;
  name?: string;
  url?: string;
  urls?: unknown;
};

/// 每家的关键词。放在这里而不是后端：这是纯粹的「猜」，规则会随用户的渠道命名
/// 习惯而变，前端改起来更快，猜错也没有任何写入后果。
const KEYWORDS: Record<string, string[]> = {
  claude: ["anthropic", "claude"],
  gpt: ["openai", "chatgpt", "codex", "gpt"],
  grok: ["grok", "x.ai", "xai"],
  glm: ["z.ai", "zai", "zhipu", "bigmodel", "chatglm", "glm"],
  kimi: ["moonshot", "kimi"],
};

/// 把渠道的名称和所有 URL 拼成一段可搜索的文本。
function haystack(c: ChannelLite): string {
  const urls: string[] = [];
  if (typeof c.url === "string") urls.push(c.url);
  // urls 在内核里是个结构（可能是数组，也可能是带 url 字段的对象数组），
  // 这里不假设形状，能挖出字符串就用。
  const walk = (v: unknown) => {
    if (typeof v === "string") urls.push(v);
    else if (Array.isArray(v)) v.forEach(walk);
    else if (v && typeof v === "object") Object.values(v).forEach(walk);
  };
  walk(c.urls);
  return `${c.name ?? ""} ${urls.join(" ")}`.toLowerCase();
}

/// 返回 providerId → channelId 的建议。
///
/// 一个渠道不会被分给两家：先匹配到的占住它。关键词按长度降序试，`z.ai` 比
/// 裸 `glm` 更有说服力，避免一个名字里同时出现两家线索时选错。
export function suggestChannels(
  providerIds: string[],
  channels: ChannelLite[],
): Record<string, number> {
  const usable = channels.filter((c) => typeof c.id === "number");
  const texts = new Map<number, string>();
  for (const c of usable) texts.set(c.id as number, haystack(c));

  const taken = new Set<number>();
  const out: Record<string, number> = {};

  for (const pid of providerIds) {
    const keys = [...(KEYWORDS[pid] ?? [pid])].sort((a, b) => b.length - a.length);
    for (const key of keys) {
      const hit = usable.find(
        (c) => !taken.has(c.id as number) && texts.get(c.id as number)!.includes(key),
      );
      if (hit) {
        out[pid] = hit.id as number;
        taken.add(hit.id as number);
        break;
      }
    }
  }
  return out;
}
