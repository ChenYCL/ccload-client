import assert from "node:assert/strict";
import { test } from "node:test";
import { normalizeBody, renderChangelog, sliceReleases } from "./kernel-changelog.mjs";

/// 上游接口按 created_at 倒序返回，最新的在前 —— 所有切片逻辑都建立在这上面。
const RELEASES = [
  { tag_name: "v4.7.8-beta.1", html_url: "u8", body: "修了 A", draft: false },
  { tag_name: "v4.7.7-beta.1", html_url: "u7", body: "加了 B", draft: false },
  { tag_name: "v4.7.6-beta.1", html_url: "u6", body: "改了 C", draft: false },
  { tag_name: "v4.7.5-beta.4", html_url: "u5", body: "", draft: false },
];

test("切出 (from, to]：含 to，不含 from", () => {
  const { picked, reason } = sliceReleases(RELEASES, "v4.7.6-beta.1", "v4.7.8-beta.1");
  assert.equal(reason, null);
  assert.deepEqual(
    picked.map((r) => r.tag_name),
    ["v4.7.8-beta.1", "v4.7.7-beta.1"],
  );
});

test("相邻两版只列一条 —— from 那一版的日志上一次已经写过了", () => {
  const { picked } = sliceReleases(RELEASES, "v4.7.7-beta.1", "v4.7.8-beta.1");
  assert.deepEqual(picked.map((r) => r.tag_name), ["v4.7.8-beta.1"]);
});

test("没有 from（第一次发布）时只列 to，不把上游整页历史倒进来", () => {
  const { picked } = sliceReleases(RELEASES, "", "v4.7.7-beta.1");
  assert.deepEqual(picked.map((r) => r.tag_name), ["v4.7.7-beta.1"]);
});

test("from 太老翻不到时退化成只列 to，并说明原因", () => {
  const { picked, reason } = sliceReleases(RELEASES, "v4.0.0", "v4.7.8-beta.1");
  assert.equal(reason, "from-not-found");
  assert.deepEqual(picked.map((r) => r.tag_name), ["v4.7.8-beta.1"]);
});

test("上游查无此 tag 时不硬凑，返回空", () => {
  const { picked, reason } = sliceReleases(RELEASES, "v4.7.6-beta.1", "v9.9.9");
  assert.equal(reason, "to-not-found");
  assert.deepEqual(picked, []);
});

test("草稿不算数：它对非 owner 不可见，链接过去是 404", () => {
  const withDraft = [
    { tag_name: "v4.7.9-beta.1", html_url: "u9", body: "未发布", draft: true },
    ...RELEASES,
  ];
  const { picked } = sliceReleases(withDraft, "v4.7.7-beta.1", "v4.7.8-beta.1");
  assert.deepEqual(picked.map((r) => r.tag_name), ["v4.7.8-beta.1"]);
});

test("标题降级，免得上游的 ## 跳到我们的章节上面去", () => {
  assert.equal(normalizeBody("## 改动\n- x"), "#### 改动\n- x");
  assert.equal(normalizeBody("正文不动 ## 不是标题"), "正文不动 ## 不是标题");
});

test("空说明有话说，不留一段空白让人以为是渲染坏了", () => {
  assert.match(normalizeBody(""), /没写说明/);
  assert.match(normalizeBody(null), /没写说明/);
});

test("截断必须说出来 —— 悄悄截等于让人以为上游就写了这么多", () => {
  const long = Array.from({ length: 60 }, (_, i) => `第 ${i} 行`).join("\n");
  const out = normalizeBody(long);
  assert.match(out, /上游原文还有 20 行/);
  assert.ok(out.split("\n").length < 60);
});

test("渲染出来的是一段可折叠的 markdown，标题带前后两版", () => {
  const { picked, reason } = sliceReleases(RELEASES, "v4.7.7-beta.1", "v4.7.8-beta.1");
  const md = renderChangelog({ from: "v4.7.7-beta.1", to: "v4.7.8-beta.1", picked, reason });
  assert.match(md, /### 内核 `v4\.7\.7-beta\.1` → `v4\.7\.8-beta\.1`/);
  assert.match(md, /<details>/);
  assert.match(md, /1 个版本/);
  assert.match(md, /#### \[v4\.7\.8-beta\.1\]\(u8\)/);
  assert.match(md, /修了 A/);
  assert.match(md, /<\/details>/);
});

test("没东西可写就返回空串，调用方据此整节都不加", () => {
  assert.equal(renderChangelog({ from: "a", to: "b", picked: [], reason: null }), "");
});

test("末尾留空行 —— 紧跟其后的 `---` 不能被当成 setext 标题下划线", () => {
  const { picked, reason } = sliceReleases(RELEASES, "v4.7.7-beta.1", "v4.7.8-beta.1");
  const md = renderChangelog({ from: "v4.7.7-beta.1", to: "v4.7.8-beta.1", picked, reason });
  assert.ok(md.endsWith("</details>\n\n"), `末尾是 ${JSON.stringify(md.slice(-20))}`);
  // 拼进真实上下文后，`</details>` 和 `---` 之间必须隔着空行
  assert.match(md + "---\n", /<\/details>\n\n---/);
});
