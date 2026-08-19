#!/usr/bin/env node
// 把 vendor/ccLoad 拉到 KERNEL_VERSION 指定的 tag。
//
// 内核源码不进本仓库（它是上游的独立仓库，而且我们的硬约束是**不改它**），
// 但打包又必须是可复现的 —— 所以版本号以一个单行文件的形式固定下来：改内核
// 版本就是改这一行，diff 里看得见，CI 和本机拿到的是同一份。

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const src = join(root, "vendor", "ccLoad");
const REPO = "https://github.com/caidaoli/ccLoad.git";

const pinned = readFileSync(join(root, "KERNEL_VERSION"), "utf8").trim();
if (!pinned) {
  console.error("KERNEL_VERSION is empty");
  process.exit(1);
}

if (!existsSync(join(src, ".git"))) {
  console.log(`cloning ${REPO} → vendor/ccLoad`);
  run("git", ["clone", "--filter=blob:none", REPO, src], root);
}

run("git", ["fetch", "--tags", "--force", "--quiet"], src);
// detached checkout：这是一个被钉住的只读依赖，不该有本地分支去承接改动。
run("git", ["checkout", "--quiet", "--detach", pinned], src);

const at = out("git", ["describe", "--tags", "--always"], src);
console.log(`vendor/ccLoad @ ${at} (pinned ${pinned})`);

function run(cmd, args, cwd) {
  const r = spawnSync(cmd, args, { cwd, stdio: "inherit" });
  if (r.status !== 0) {
    console.error(`${cmd} ${args.join(" ")} failed`);
    process.exit(r.status ?? 1);
  }
}

function out(cmd, args, cwd) {
  const r = spawnSync(cmd, args, { cwd, encoding: "utf8" });
  return r.status === 0 ? r.stdout.trim() : "?";
}
