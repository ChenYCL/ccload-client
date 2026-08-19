#!/usr/bin/env node
// Rebuild the bundled ccLoad sidecar from vendor/ccLoad as a universal
// (arm64 + x86_64) Mach-O so the shell and sidecar share an architecture on
// every Mac. A Rosetta-translated shell spawning a native arm64 sidecar (or
// vice versa) trips WebKit/SIGBUS crashes on Apple Silicon — both slices must
// agree.
// Invoked by `pnpm kernel:build` and as Tauri's beforeBuildCommand.

import { spawnSync } from "node:child_process";
import { mkdirSync, existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const src = join(root, "vendor", "ccLoad");
const outDir = join(root, "src-tauri", "binaries");
const isWin = process.platform === "win32";
const isMac = process.platform === "darwin";
const out = join(outDir, isWin ? "ccload.exe" : "ccload");

if (!existsSync(src)) {
  console.error("vendor/ccLoad is missing — run `pnpm kernel:fetch` first");
  process.exit(1);
}

mkdirSync(outDir, { recursive: true });

// 版本号必须在这里注入。ccLoad 的 version 包默认是 "dev"，只有 -ldflags -X 才会
// 变成真的 tag（上游 Makefile 就是这么做的）。不注入的话打进去的内核自报 "dev"，
// 而设置页要拿它和远端内核比对版本 —— 之前那个写死的 "v1.2.0" 就是因为没有可信
// 来源才手抄的，抄错了也没人知道。
const VERSION_PKG = "ccLoad/internal/version";
const version = git(["describe", "--tags", "--always"]) || "dev";
// 打出来的版本必须和 KERNEL_VERSION 对得上，否则本机改过 checkout 却忘了改钉子，
// 发布出去的包会和仓库里声明的版本不是同一个东西。
const pinned = readFileSync(join(root, "KERNEL_VERSION"), "utf8").trim();
if (pinned && version !== pinned) {
  console.error(
    `vendor/ccLoad 当前在 ${version}，但 KERNEL_VERSION 钉的是 ${pinned}。\n` +
      `跑 \`pnpm kernel:fetch\` 对齐，或者把 KERNEL_VERSION 改成你真正想发布的版本。`,
  );
  process.exit(1);
}
const commit = git(["rev-parse", "--short", "HEAD"]) || "unknown";
// 不能带空格：整个 -ldflags 是一个 argv，链接器按空格切分，`2026-08-18 18:00:00`
// 会让 `18:00:00` 变成一个不存在的链接器选项，构建直接失败。用 ISO 的 T 分隔。
const buildTime = new Date().toISOString().slice(0, 19);
const ldflags = [
  `-X ${VERSION_PKG}.Version=${version}`,
  `-X ${VERSION_PKG}.Commit=${commit}`,
  `-X ${VERSION_PKG}.BuildTime=${buildTime}`,
  `-X ${VERSION_PKG}.BuiltBy=ccload-client`,
].join(" ");

// 壳体在不启动内核的前提下也要知道打进去的是哪一版（设置页开机就要显示），所以
// 落一个文件让 Rust 侧 include_str! 进去。
writeFileSync(join(root, "src-tauri", "kernel-version.txt"), version);
console.log(`ccLoad version → ${version} (${commit})`);

// 只有 macOS 需要 lipo 合成 universal —— 那是为了让壳体和内核架构一致，否则
// Apple Silicon 上 Rosetta 翻译的 WebKit 会 SIGBUS。Windows / Linux 各自单架构，
// 直接按当前机器的架构编。
if (!isMac) {
  build(src, out, process.env.GOARCH ?? hostArch());
  process.exit(0);
}

const arches = ["arm64", "amd64"];
const slices = arches.map((arch) => join(tmpdir(), `ccload-${arch}-${process.pid}`));

for (let i = 0; i < arches.length; i++) {
  console.log(`building ccLoad sidecar slice ${arches[i]} → ${slices[i]}`);
  build(src, slices[i], arches[i]);
}

console.log(`lipo -create → ${out}`);
const r = spawnSync("lipo", ["-create", ...slices, "-output", out], { stdio: "inherit" });
for (const slice of slices) rmSync(slice, { force: true });
if (r.status !== 0) {
  console.error("lipo failed to create universal binary");
  process.exit(r.status ?? 1);
}

const info = spawnSync("lipo", ["-archs", out], { encoding: "utf8" });
if (info.status === 0) console.log(`ccload architectures: ${info.stdout.trim()}`);
process.exit(0);

/** Node 的 arch 名 → Go 的 GOARCH 名。 */
function hostArch() {
  return { x64: "amd64", arm64: "arm64", ia32: "386" }[process.arch] ?? process.arch;
}

function goos() {
  return { darwin: "darwin", win32: "windows", linux: "linux" }[process.platform] ?? process.platform;
}

function git(args) {
  const r = spawnSync("git", args, { cwd: src, encoding: "utf8" });
  return r.status === 0 ? r.stdout.trim() : "";
}

function build(cwd, output, arch) {
  const r = spawnSync(
    "go",
    ["build", "-tags", "sonic", "-trimpath", "-ldflags", ldflags, "-o", output, "."],
    {
      cwd,
      stdio: "inherit",
      // GOOS 必须跟着宿主平台走。写死 darwin 会让 Linux/Windows 的 CI 产出一个
      // 无法执行的 Mach-O 二进制，而且要等到运行时才暴露。
      env: { ...process.env, CGO_ENABLED: "0", GOOS: goos(), GOARCH: arch },
    },
  );
  if (r.status !== 0) {
    console.error(`go build failed for ${arch}`);
    process.exit(r.status ?? 1);
  }
}
