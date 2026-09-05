/// 首选渠道钉住在前端要用的两个小工具。机制见 Rust 侧 `services/pins.rs`：
/// 钉住的别名在内核里是一条私有条目 `grok-4.6@ch21`，内核日志记的就是这个名字，
/// 显示时要剥回原名。

/** 后端 `alias_key` 的镜像：剥 `[1m]` 一类的窗口后缀和内核的 thinking 后缀 `(max)`，
 *  两种谁在外面都行、剥到没有为止，然后小写。 */
export function aliasKey(name: string): string {
  let s = name.trim();
  for (;;) {
    let next = s;
    if (next.endsWith("]")) {
      const open = next.lastIndexOf("[");
      if (open > 0) next = next.slice(0, open).trimEnd();
    }
    next = splitThinking(next)[0].trimEnd();
    if (next === s) break;
    s = next;
  }
  return s.toLowerCase();
}

/** 内核 `thinking/suffix.go` 认的词和非负整数预算；别的括号内容不是后缀。 */
const THINKING_WORDS = new Set(["none", "auto", "-1", "minimal", "low", "medium", "high", "xhigh", "max"]);
function isThinkingSuffix(inner: string): boolean {
  const s = inner.trim().toLowerCase();
  return THINKING_WORDS.has(s) || /^\d+$/.test(s);
}

export function sameAlias(a: string, b: string): boolean {
  const ka = aliasKey(a);
  return ka.length > 0 && ka === aliasKey(b);
}

/** 内核的 thinking 后缀 `(max)` 挂在最末，私有标记在它前面：`gpt-5.6@ch9(max)`。 */
function splitThinking(name: string): [string, string] {
  const open = name.lastIndexOf("(");
  if (open > 0 && name.endsWith(")") && isThinkingSuffix(name.slice(open + 1, -1))) {
    return [name.slice(0, open), name.slice(open)];
  }
  return [name, ""];
}

/** `grok-4.6@ch21` → `{ base: "grok-4.6", channelId: 21 }`；不是私有别名就 null。 */
export function splitPinned(name: string): { base: string; channelId: number } | null {
  const [head, thinking] = splitThinking(name.trim());
  const at = head.lastIndexOf("@ch");
  if (at <= 0) return null;
  const tail = head.slice(at + 3);
  if (!/^\d+$/.test(tail)) return null;
  return { base: head.slice(0, at) + thinking, channelId: Number(tail) };
}

/** 给人看的模型名：私有别名剥回原名，其余原样。 */
export function displayModel(name: string): string;
export function displayModel(name: string | null | undefined): string | undefined;
export function displayModel(name: string | null | undefined): string | undefined {
  if (name == null) return undefined;
  return splitPinned(name)?.base ?? name;
}
