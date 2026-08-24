#!/usr/bin/env node
// 把上游 ccLoad 的更新日志摘出来，塞进我们自己的 release notes。
//
// 为什么需要：跟版流水线自动跟上游内核时，我们的 release notes 里只有一行
// 「chore(kernel): 内核钉到 v4.7.8-beta.1」。那句话说了「换了」，没说「换了什么」
// —— 而内核恰恰是这个产品干活的那一半（代理、故障转移、协议转换全在它里面）。
// 于是用户要知道这一版到底变了什么，还得自己跳到另一个仓库去翻，release 页就
// 只剩下载功能。
//
// 用法（两个 workflow 都调它）：
//   node scripts/kernel-changelog.mjs --from v4.7.7-beta.1 --to v4.7.8-beta.1
//
// `--from` 是**上一版包里装的**内核，不含在输出里（它的日志上一版已经写过了）；
// `--to` 是这一版装的。两者相同 → 什么都不输出，本次没换内核。
// 拿不到网络 / 上游改了接口 → 打一行 stderr 然后照样退 0：release notes 少一段
// 附加信息不该把整个发布卡住。

import { argv, env, exit, stderr, stdout } from "node:process";

/// 上游内核仓库。和 kernel-sync.yml、scripts/fetch-kernel.mjs 写死的是同一个。
const UPSTREAM = "caidaoli/ccLoad";

/// 一次拉多少条。内核一天能出好几个 beta，但 100 条足够覆盖任何两次发布之间的
/// 跨度；真超出了就退化成「只列 --to 这一版」，并在 stderr 说明。
const PAGE = 100;

/// 单个版本的说明最多留多少行。上游偶尔会把整份 diff 贴进 release body，
/// 那种东西灌进我们的 notes 里会把真正的信息淹掉。
const MAX_BODY_LINES = 40;

/// 总共最多列几个版本。跨度特别大时（比如手动跳版）只列最近的几个。
const MAX_RELEASES = 10;

/// 从「最新在前」的 release 列表里，切出 (from, to] 这一段。
///
/// GitHub 的 `/releases` 按 created_at 倒序返回，所以 to 的下标比 from 小。
/// 切片取 [i_to, i_from)：含 to，不含 from —— from 那一版的说明上一次已经写过了。
///
/// 找不到 to：返回空（我们钉的 tag 在上游查无此人，多半是接口或仓库变了）。
/// 找不到 from：只返回 to 那一条 —— 宁可少列，也不要把上游整页历史都倒进来。
export function sliceReleases(releases, from, to) {
  const live = releases.filter((r) => !r.draft);
  const tags = live.map((r) => r.tag_name);
  const iTo = tags.indexOf(to);
  if (iTo < 0) return { picked: [], reason: "to-not-found" };
  const iFrom = from ? tags.indexOf(from) : -1;
  if (from && iFrom < 0) return { picked: [live[iTo]], reason: "from-not-found" };
  const end = iFrom < 0 ? iTo + 1 : iFrom;
  const picked = live.slice(iTo, end);
  return picked.length > MAX_RELEASES
    ? { picked: picked.slice(0, MAX_RELEASES), reason: "too-many" }
    : { picked, reason: null };
}

/// 上游的说明原样搬过来，只做两件事：砍掉过长的，和把标题降级。
///
/// 降级是必须的：上游用 `##` 当小节标题，而我们这段整体挂在 `###` 底下，
/// 不降级的话它们会在目录里跳到我们章节的上面去，读起来像是两份文档拼在一起。
export function normalizeBody(body) {
  const text = (body ?? "").replace(/\r\n/g, "\n").trim();
  if (!text) return "_（上游这一版没写说明）_";
  const lines = text.split("\n").map((l) => (/^#{1,4} /.test(l) ? `##${l}` : l));
  if (lines.length <= MAX_BODY_LINES) return lines.join("\n");
  // 截断要说出来。悄悄截等于骗人：读的人会以为上游就写了这么多。
  return [
    ...lines.slice(0, MAX_BODY_LINES),
    "",
    `_（上游原文还有 ${lines.length - MAX_BODY_LINES} 行，见上面的链接）_`,
  ].join("\n");
}

/// 渲染成一段 markdown。没有可写的就返回空串 —— 调用方据此决定加不加这一节。
export function renderChangelog({ from, to, picked, reason }) {
  if (!picked.length) return "";

  const head = from ? `内核 \`${from}\` → \`${to}\`` : `内核 \`${to}\``;
  const out = [`### ${head}`, ""];

  // 折叠起来：内核日志是「想知道再看」的信息，摊开会把我们自己这一版的改动
  // 挤到屏幕外面去。summary 上写清楚有几个版本，免得折叠等于藏起来。
  out.push("<details>");
  out.push(
    `<summary>上游 ${UPSTREAM} 的更新日志（${picked.length} 个版本）</summary>`,
  );
  out.push("");

  if (reason === "from-not-found") {
    out.push(
      `> 上一版装的 \`${from}\` 在上游最近 ${PAGE} 条发布里没找到，只列了这一版。`,
      "",
    );
  } else if (reason === "too-many") {
    out.push(`> 跨度超过 ${MAX_RELEASES} 个版本，只列了最近的这些。`, "");
  }

  for (const r of picked) {
    out.push(`#### [${r.tag_name}](${r.html_url})`, "");
    out.push(normalizeBody(r.body), "");
  }

  // 末尾留一个空行。调用方是 `cat` 进一份更大的 notes，紧跟着往往就是 `---`，
  // 而 markdown 里 `---` 贴着上一行会被当成 setext 标题的下划线，把 `</details>`
  // 这行渲染成一个大标题。空行把它钉成分隔线。
  out.push("</details>", "", "");
  return out.join("\n");
}

function parseArgs(args) {
  const get = (name) => {
    const i = args.indexOf(name);
    return i >= 0 ? (args[i + 1] ?? "") : "";
  };
  return { from: get("--from").trim(), to: get("--to").trim() };
}

async function fetchReleases() {
  const headers = {
    // 没有 User-Agent 时 GitHub 直接回 403。
    "user-agent": "ccload-client-release-notes",
    accept: "application/vnd.github+json",
  };
  // Actions 里给了 token 就带上：未认证是每小时 60 次/IP，而同一个 runner 网段
  // 上还有别人在打同一个接口。
  const token = env.GH_TOKEN || env.GITHUB_TOKEN;
  if (token) headers.authorization = `Bearer ${token}`;

  const res = await fetch(
    `https://api.github.com/repos/${UPSTREAM}/releases?per_page=${PAGE}`,
    { headers },
  );
  if (!res.ok) throw new Error(`GitHub 回了 HTTP ${res.status}`);
  return res.json();
}

async function main() {
  const { from, to } = parseArgs(argv.slice(2));
  if (!to) {
    stderr.write("kernel-changelog: 没给 --to，跳过\n");
    return;
  }
  if (from === to) {
    stderr.write(`kernel-changelog: 内核没变（${to}），不写这一节\n`);
    return;
  }

  const releases = await fetchReleases();
  const { picked, reason } = sliceReleases(releases, from, to);
  if (reason === "to-not-found") {
    stderr.write(`kernel-changelog: 上游没有 ${to} 这个 release，跳过\n`);
    return;
  }
  stdout.write(renderChangelog({ from, to, picked, reason }));
}

// 直接跑才执行 main；被 import 时（测试）只拿纯函数。
if (import.meta.url === `file://${argv[1]}`) {
  main().catch((e) => {
    // 拿不到就算了。release notes 少一段附加信息，不该把发布卡住 ——
    // 包已经编完躺在那儿了。
    stderr.write(`kernel-changelog: ${e.message}，跳过这一节\n`);
    exit(0);
  });
}
