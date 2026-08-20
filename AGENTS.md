# AGENTS.md

给编码代理（Claude Code / Codex / OpenCode / Gemini CLI …）和新加入的人类开发者看的
仓库须知。**先读这一份，再动代码。**

`CLAUDE.md` 只是指向本文件的软链接式入口，内容不重复。

---

## 这是什么

`ccload-client` 是 [ccLoad](https://github.com/caidaoli/ccLoad) 的桌面客户端：
Tauri 2 壳体（Rust）+ React 18 前端，把 Go 写的 ccLoad 内核作为 sidecar 托管起来，
并把本机各个 CLI（Claude Code / Codex / Gemini CLI / Grok Build / OpenCode）的配置
指向它。

```
┌─ src/            React 前端（Vite + TS + Tailwind + TanStack Query）
├─ src-tauri/      Rust 壳体（commands/ 是 IPC 边界，services/ 是实现）
├─ vendor/ccLoad/  上游内核源码，由 scripts/fetch-kernel.mjs clone 到此（普通
│                  目录，不是 git submodule），**只读**
├─ scripts/        内核拉取与编译（Node，无依赖）
└─ KERNEL_VERSION  钉住的上游内核 tag
```

---

## 硬约束

### 1. 不要改 `vendor/ccLoad`

内核是上游项目，本仓库只消费它。需要新内核能力时的正确做法是：改
`KERNEL_VERSION` 里的 tag → `pnpm kernel:fetch` → `pnpm kernel:build`。

在 `vendor/` 下打补丁会让「壳体能编过、用户机器上编不过」，而且下一次
`kernel:fetch` 会把补丁冲掉。

### 2. 内核已有的能力，客户端不要重做一遍

分工是明确的：

| 归内核 | 归壳体 |
| --- | --- |
| 代理转发、渠道选择、故障转移、协议转换（Anthropic ↔ OpenAI ↔ Gemini ↔ Codex）、计费统计、模型清单拉取 | 托管进程、写各 CLI 的配置文件、备份/回滚、把内核的 Admin API 包装成界面 |

动手写新功能之前先在 `vendor/ccLoad` 里查一遍：`/v1/*` 和 `/v1beta/*` 已经是
Any 路由（内核**本身就是**本地 OpenAI/Anthropic 出口代理），
`GET /admin/channels/:id/models/fetch` 已经能问上游要模型清单，
`POST /admin/channels/models/refresh-batch` 已经支持 `merge|replace`。
重复实现一遍只会多一份要维护的、行为还不一致的代码。

### 3. 写用户配置文件的三条规矩

这部分踩过的坑最多，改 `src-tauri/src/services/cli_*.rs` 之前务必看完：

* **原子写 + 保权限。** 一律走 `cli_io::write_atomic`。`std::fs::rename` 会
  **交换 inode**，目标文件原有的权限位不会保留 —— `~/.claude.json` 里有
  OAuth 账号和一堆 MCP bearer token，被我们从 `0600` 降到 `0644` 过一次。
  `carry_permissions` 失败时宁可整个写入失败，也不能让带凭据的文件以更宽松的
  权限落地。
* **先快照。** 任何写入前调 `BackupStore::snapshot`，用户能在「快照历史」回滚。
  每个 CLI 最多保留 5 份，按时间覆盖，但**首份 pristine 快照永不淘汰**。
* **合并，不要整块替换。** MCP 服务器、profile、模型目录都要按键合并 ——
  整块 `insert` 会把用户手写的 `startup_timeout_sec` / `cwd` / 自定义模型
  一次抹掉。「导入」在语义上必须是**追加**：不动用户当前选中的模型、不动他
  已经绑好的槽位。见 `services/model_import.rs` 的模块注释。

开发期请在设置里打开「CLI 写入走沙箱」，写入会落到
`~/.ccload-client/sandbox/`，不碰真实配置。

### 4. macOS 包必须是 universal

壳体和内核的架构不一致时，Apple Silicon 上被 Rosetta 翻译的 WebKit 会 SIGBUS，
表现是白屏/闪退。`tauri build --target universal-apple-darwin`，内核也要出
两份再 `lipo`。CI 里已经这么做，本地手打包别偷懒。

---

## 常用命令

```bash
pnpm install
pnpm kernel:fetch        # 按 KERNEL_VERSION 检出 vendor/ccLoad
pnpm kernel:build        # 编内核 → src-tauri/binaries/
pnpm dev                 # 前端 + 壳体热重载

pnpm typecheck                                   # tsc --noEmit
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo test
```

**提交前这三条必须全绿**，CI 跑的就是它们。

不要直接运行 `src-tauri/binaries/ccload`：它没有 `--version` 之类的短路参数，
任何参数都会让它启动服务器，并在当前目录建一个 `data/ccload.db`。

---

## 代码约定

**注释写「为什么」，不写「做了什么」。** 代码本身已经说清做了什么。值得写下来的
是那些一旦忘记就会被人「顺手改回去」的判断：

```rust
// 宁可失败也不要让带凭据的文件以更宽松权限落地
if let Err(e) = carry_permissions(path, &tmp) { … }
```

**Rust**

* `commands/` 只做参数校验和 `AppState` 取用，实现放 `services/`。
* 错误一律 `AppError`，面向用户的消息用中文，写清「发生了什么 + 该怎么办」。
* serde 的坑：**字段级** `#[serde(default)]` 用的是**字段类型**的 Default
  （`bool` → `false`），不是结构体的 `Default` impl。要用后者得写在容器级。
* 每个非平凡的行为配一个测试，测试名就是那句断言（`atomic_write_keeps_the_target_permissions`）。

**前端**

* 文本控件统一走 `components/ui/Input.tsx`（`TextInput` / `TextArea` / `Select`）。
  它关掉了自动首字母大写/拼写纠正，样式来自 `.field` 一套类。
* 样式类要写进 `@layer components`。Tailwind 把 components 排在 utilities
  **之前**；直接写在 `@tailwind utilities` 之后的同优先级规则会反过来压掉调用点
  的 `pl-8` / `w-56`。
* 按下反馈用**独立的 `scale` 属性**，不要写 `transform: scale(…)` ——
  后者会整体替换元素原有的 transform，靠 `translate-x-1/2` 定位的元素一按下去
  就跳出指针底下，表现是「要连点好几次才有反应」。
* `mask-image` / `filter` / `transform` / `backdrop-filter` 会让元素成为
  `position: fixed` 子元素的包含块 —— 模态框必须 `createPortal` 到 body。
* 界面文案走 `i18n`：`t("中文原文")`，**key 就是中文原文**，英文词典在
  `src/i18n/en.ts`。没收录的条目自动回落成中文，可以一页一页地补。

---

## 发版

两条线，互不干扰。

**beta —— 不打 tag，跑 workflow。** Actions → **Beta Release** → Run workflow，填
分支即可（`workflow_dispatch`，见 `.github/workflows/beta.yml`）。版本号由流水线
自己算：`package.json` 的 version + 当天日期 + 该日第 N 次，形如
`v0.1.0-beta.20260819.2`，tag 也由它创建。只发 **prerelease**，`latest` 永远不会
指到它。

> 别为了打 beta 去手改 `package.json` 的 version。那个字段是 beta 版本号的
> **基座**，把它写成 `0.2.0-beta.1` 会让流水线算出
> `v0.2.0-beta.1-beta.20260819.1`。beta 序号是流水线的事，不是人的事。
>
> 流水线会在打包前把算出来的完整 tag 戳进工作区的 `package.json` 和
> `tauri.conf.json`（不 commit），侧栏才能显示 `v0.1.0-beta.20260820.2`
> 而不是基座 `v0.1.0`。

beta 的 Windows 只出 NSIS，不出 MSI —— 原因写在 `beta.yml` 的 matrix 注释里
（MSI 的 ProductVersion 比较时忽略第四段，两个 beta 会被当成同一版本，覆盖安装
报 1638）。要动这块之前先读那段注释。

**正式版 —— 人推 tag。** tag 形如 `v0.1.0`，只标客户端版本；打进包里的内核版本由
`KERNEL_VERSION` 单独钉住，两者互不牵连。推 tag 触发
`.github/workflows/release.yml`，三平台并行构建，产物汇总成一个**草稿**
release —— 包体上百 MB，发出去之前人眼看一眼产物齐不齐。草稿在 Releases 列表里
对非 owner 不可见，产物是在的，别以为没打出来。

正式版的版本号写在三处且必须一致：`package.json`、`src-tauri/tauri.conf.json`、
`src-tauri/Cargo.toml`（`Cargo.lock` 跑一次 `cargo check` 自动跟上）。流水线里的
「Verify tag matches app version」会拿 tag 去比前两处，对不上直接红。

Apple 签名相关的 secret 没配时流水线照常出未签名包（用户首次打开需右键「打开」），
不会因为缺开发者账号整条红掉。

---

## 提交之前

* `pnpm typecheck` / `cargo clippy -D warnings` / `cargo test` 三条全绿。
* 别把这些提上去：内核二进制（约 128 MB）、`data/`、`*.db`、截图、
  `~/.claude` 之类的真实配置、任何 API key。`.gitignore` 已覆盖常见的，
  但仍然请 `git status` 看一眼再 `git add`。
* 提到具体渠道/域名时用占位符（`https://example.com`），不要写自己在用的中转站。
