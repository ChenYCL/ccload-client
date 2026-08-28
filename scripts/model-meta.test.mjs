import assert from "node:assert/strict";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const out = join(mkdtempSync(join(tmpdir(), "ccload-mm-")), "modelMeta.mjs");
execFileSync(
  "npx",
  ["esbuild", "src/lib/modelMeta.ts", "--format=esm", `--outfile=${out}`, "--log-level=error"],
  { stdio: "inherit" },
);
const { defaultContextWindow, formatWindow } = await import(out);

/// 必须和 src-tauri/src/services/context_window.rs 的 family_window 对齐。
/// 调度图格子上标的窗口、导入表的默认值、CLI compact 挑模型，三处看见同一个数。

test("claude 4.6 起是 1M，haiku 和 4.5 仍是 200k", () => {
  assert.equal(defaultContextWindow("claude-opus-5"), 1_000_000);
  assert.equal(defaultContextWindow("claude-opus-4-8"), 1_000_000);
  assert.equal(defaultContextWindow("claude-fable-5"), 1_000_000);
  assert.equal(defaultContextWindow("claude-sonnet-5"), 1_000_000);
  assert.equal(defaultContextWindow("claude-sonnet-4-5-20250929"), 200_000);
  assert.equal(defaultContextWindow("claude-haiku-4-5-20251001"), 200_000);
});

test("带空格的人手别名也要认得出", () => {
  assert.equal(defaultContextWindow("Fable 5"), 1_000_000);
  assert.equal(defaultContextWindow("Opus 4.8 (1M context)"), 1_000_000);
});

test("glm-5.3 是 1M，不是 glm 家族的 200k", () => {
  assert.equal(defaultContextWindow("glm-5.3"), 1_000_000);
  assert.equal(defaultContextWindow("glm-5.3-flash"), 1_000_000);
  assert.equal(defaultContextWindow("glm-5.1"), 200_000);
  assert.equal(defaultContextWindow("glm-4.6"), 200_000);
});

test("grok-4.6 钉死 500k", () => {
  assert.equal(defaultContextWindow("grok-4.6"), 500_000);
  assert.equal(defaultContextWindow("grok-4.5"), 500_000);
  assert.equal(defaultContextWindow("grok-3"), 256_000);
});

test("后缀声明压过家族猜测", () => {
  assert.equal(defaultContextWindow("glm-5.3[1m]"), 1_000_000);
  assert.equal(defaultContextWindow("claude-opus-5[1m]"), 1_000_000);
});

test("短标签给人看", () => {
  assert.equal(formatWindow(1_000_000), "1M");
  assert.equal(formatWindow(500_000), "500k");
  assert.equal(formatWindow(200_000), "200k");
  assert.equal(formatWindow(0), "");
});
