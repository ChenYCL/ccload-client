import type { LogEntry, ProxyRecord } from "../types";

/// 一条日志「是谁打过来的」。
///
/// 内核日志里没有任何调用方身份可以直接读：`client_ip` 在内核远端部署时，本机
/// 所有 CLI 的请求都共用同一个公网出口；`auth_token_description` 又常常是同一把
/// 令牌分给了别人用。所以本机的归属只能由**我们自己的 CLI 代理**提供 —— 只有它
/// 知道每条转发是哪个 CLI 发的（见 `detect_cli`）。
///
/// 判据分三层，从确定到推断：
///   1. 配对上了代理记录 → 本机，而且知道具体是哪个 CLI；
///   2. 没配上，但 `client_ip` 在「已确认属于本机的出口 IP」里 → 还是本机，只是
///      没走代理（例如 MCP 拿着同一把令牌直接打内核）；
///   3. 出口 IP 不属于本机 → 另一台机器。
///
/// 第 2、3 层完全依赖第 1 层学出来的出口 IP：凡是配对成功的日志，它的 client_ip
/// 必然就是本机的出口地址。学不到的时候（代理刚起来、或当前这页恰好全是别人的
/// 请求）**不猜**，标成未知 —— 把别人的请求说成本机，比空着更糟。

/// 代理记的 CLI id（`detect_cli` 的返回值）→ 界面显示名。
/// 注意这里的 key 是**代理**的口径，和 `targets.ts` 的 `CliTarget` 不是一套：
/// 代理按 User-Agent 认出的是 `grok`，而接管目标那边叫 `grok-build`。
const CLI_LABELS: Record<string, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
  grok: "Grok",
  "grok-build": "Grok",
  "gemini-cli": "Gemini",
  gemini: "Gemini",
  opencode: "OpenCode",
};

/// `unknown` 是代理认不出 UA 时的兜底值，直接显示会变成界面上的一个英文单词；
/// 返回 undefined 让调用方用自己的文案。
export function cliLabel(cli: string): string | undefined {
  if (!cli || cli === "unknown") return undefined;
  return CLI_LABELS[cli] ?? cli;
}

export type LogOrigin = {
  kind: "local-cli" | "local-direct" | "remote" | "unknown";
  /** local-cli：CLI 显示名；remote/unknown：令牌描述或 IP。local-direct 没有。 */
  who?: string;
  ip?: string;
  token?: string;
};

/// 给一页日志判定来源，并把这一页里新学到的本机出口 IP 一并返回。
///
/// `knownLocalIps` 传上一次学到的集合：出口 IP 只在换网络时才变，攒着能让「当前
/// 页没有任何本机请求」的情况仍然判得出远端。集合只增不减 —— 换过 Wi-Fi 之后旧
/// IP 会留在里面，代价是那个 IP 上如果后来换了别人会被误判成本机，概率极低，
/// 而反过来（漏判）每次切网都会发生。
export function classifyOrigins(
  logs: LogEntry[],
  matched: ReadonlyMap<number, ProxyRecord>,
  knownLocalIps: ReadonlySet<string>,
): { origins: Map<number, LogOrigin>; localIps: Set<string> } {
  // 配对本身是启发式的（时间 + 模型名），偶尔会把别人的日志认成我们的记录。
  // 那种错配如果直接拿去学 IP，就会把整台别人的机器标成本机 —— 一次巧合污染
  // 一大片。所以只采信「占了本页配对结果一半以上」的 IP：我们自己的请求必然
  // 是配对成功里的绝大多数，零星的错配过不了这道线。
  const hits = new Map<string, number>();
  for (const log of logs) {
    if (matched.has(log.id) && log.client_ip) {
      hits.set(log.client_ip, (hits.get(log.client_ip) ?? 0) + 1);
    }
  }
  const total = [...hits.values()].reduce((a, b) => a + b, 0);
  const localIps = new Set(knownLocalIps);
  for (const [ip, n] of hits) {
    if (n * 2 >= total) localIps.add(ip);
  }

  const origins = new Map<number, LogOrigin>();
  for (const log of logs) {
    const ip = log.client_ip;
    const token = log.auth_token_description;
    const rec = matched.get(log.id);
    if (rec) {
      origins.set(log.id, { kind: "local-cli", who: cliLabel(rec.cli), ip, token });
    } else if (ip && localIps.has(ip)) {
      origins.set(log.id, { kind: "local-direct", ip, token });
    } else if (ip && localIps.size > 0) {
      // 已经知道本机长什么样，这个 IP 不是 —— 才敢说是别的机器。
      origins.set(log.id, { kind: "remote", who: token || ip, ip, token });
    } else {
      origins.set(log.id, { kind: "unknown", who: token || ip, ip, token });
    }
  }
  return { origins, localIps };
}

/// 进行中的请求只有 IP 可用（内核内存态里没有令牌描述，也来不及和代理记录配对），
/// 所以只回答「本机还是别人」这一个问题。
export function activeOrigin(
  clientIp: string | undefined,
  localIps: ReadonlySet<string>,
): "local" | "remote" | "unknown" {
  if (!clientIp || localIps.size === 0) return "unknown";
  return localIps.has(clientIp) ? "local" : "remote";
}
