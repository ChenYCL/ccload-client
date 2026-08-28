import assert from "node:assert/strict";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/// `matchSessions` 是 TS，node:test 直接 import 不了 —— 先用仓库自带的 esbuild
/// 转一份到临时目录再跑。为一个纯函数装 vitest 不值当，这里的成本只是一次转译。
const out = join(mkdtempSync(join(tmpdir(), "ccload-sm-")), "sessionMatch.mjs");
execFileSync(
  "npx",
  ["esbuild", "src/lib/sessionMatch.ts", "--format=esm", `--outfile=${out}`, "--log-level=error"],
  { stdio: "inherit" },
);
const { matchSessions } = await import(out);

/// 内核日志和代理记录唯一的公共坐标是「时间 + 模型」，所以这里的用例都围绕
/// 这两个维度构造。代理在**请求开始**打点、内核在**结束**记录，所以正确的
/// 时间方向永远是「代理早于日志」。
const log = (id, time, model) => ({ id, time, model });
const rec = (time, model, session_id) => ({
  time,
  cli: "claude-code",
  session_id,
  model,
  sent_model: model,
  path: "/v1/messages",
  status: 200,
});

test("同模型多条日志各自认领最近的一条代理记录，不会全贴同一个会话", () => {
  const logs = [log(1, 1000, "opus"), log(2, 1010, "opus")];
  const records = [rec(997, "opus", "A"), rec(1007, "opus", "B")];
  const m = matchSessions(logs, records);
  assert.equal(m.get(1), "A");
  assert.equal(m.get(2), "B");
});

test("代理记录晚于日志：方向不对，不认", () => {
  const m = matchSessions([log(1, 1000, "opus")], [rec(1100, "opus", "A")]);
  assert.equal(m.size, 0);
});

test("隔得太久：跨过窗口就不认，宁可空着也不标错会话", () => {
  const m = matchSessions([log(1, 100000, "opus")], [rec(1000, "opus", "A")]);
  assert.equal(m.size, 0);
});

test("模型名对不上就不认", () => {
  const m = matchSessions([log(1, 1000, "opus")], [rec(998, "grok", "A")]);
  assert.equal(m.size, 0);
});

/// 代理转发前会改写模型名（`claude-opus-5[1m]` → `claude-opus-5`），内核记的是
/// 改写后的名字，所以两边都要能对上。
test("内核记的是改写后的名字，仍要匹配得上", () => {
  const records = [
    {
      time: 998,
      cli: "claude-code",
      session_id: "A",
      model: "claude-opus-5[1m]",
      sent_model: "claude-opus-5",
      path: "/v1/messages",
      status: 200,
    },
  ];
  const m = matchSessions([log(1, 1000, "claude-opus-5")], records);
  assert.equal(m.get(1), "A");
});

test("没有代理记录时安静地什么都不标", () => {
  assert.equal(matchSessions([log(1, 1000, "opus")], []).size, 0);
});

test("代理记录没有会话 id 就跳过，不会写出 undefined", () => {
  const m = matchSessions([log(1, 1000, "opus")], [rec(998, "opus", undefined)]);
  assert.equal(m.size, 0);
});
