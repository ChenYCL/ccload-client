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

具体到几个反复有人想在壳体里重写的：

* **改上游请求的 header / body** —— 内核的 `channels.custom_request_rules`
  已经做了（`internal/app/custom_rules.go`）：`headers[]` 支持
  `remove | override | append`（`remove` 还能按逗号 token 精确剔除，比如从
  `anthropic-beta` 里只去掉一个 flag），`body[]` 支持按点分路径
  `remove | override`。认证头（`authorization` / `x-api-key` /
  `x-goog-api-key`）有黑名单，规则会被静默忽略并打 warn。壳体要做的是把它
  包装成界面，不是再写一个转发层。
* **强制某个模型** —— 渠道模型条目上的 `redirect_model` 是 1:1 重定向
  （`resolveActualModel`）；想「不管请求什么都发 X」用 `custom_request_rules`
  的 body override（路径 `model`），它在协议转换之后、发出之前生效。
* **多层 fallback** —— 内核只做一跳，客户端的「模型链」把链拆成 N 个同别名、
  优先级递减的渠道来实现，见 `services/fallback.rs`。

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

#### 3b. 写用户的 markdown 指令文件：只碰标记块

「系统注入」（`services/system_inject.rs`）往这五个文件里写东西：

| CLI | 全局指令文件 |
| --- | --- |
| Claude Code | `~/.claude/CLAUDE.md` |
| Codex | `~/.codex/AGENTS.md` |
| Gemini CLI | `~/.gemini/GEMINI.md` |
| Grok Build | `~/.grok/AGENTS.md`（Grok 截断到 10000 字符，静默） |
| OpenCode | `~/.config/opencode/AGENTS.md` |

它们和 settings.json 不是一类东西：**用户不认为这是工具在管的文件**，
`~/.claude/CLAUDE.md` 里往往是攒了几个月的个人规则，抹掉不可逆。所以只认
`<!-- ccload:begin -->` / `<!-- ccload:end -->` 之间的内容，块外一个字节都不动。

两个已经写进测试、别退回去的细节：

* 只有 BEGIN 没有 END 时**按「没有块」处理**，不能从 BEGIN 删到文件尾 ——
  半个标记多半是用户手工删了一半，贸然删到尾会吃掉他后面写的所有东西。
* 装了卸、卸了装反复来回，不能攒出越来越多的空行。

块内每一小节前面还有一行自己的标记（`<!-- ccload:vision -->` 这种），**回显靠
它，不靠比对文字**。前端曾经的做法是「把某一段单独渲染一遍，再看块里包不包含
这段文字」—— 那个判断在我们自己改一个字的那天就失效了：用户机器上的块是上个
版本写进去的，逐字对不上，于是勾选框显示成没勾，整段旧文字被当成用户手写内容，
再按一次「更新」就写出一段旧的加一段新的。解析放在 `parse_block` 里，和渲染同
一个文件、同一组测试盯着；没有标记的老块走标题兜底，界面上标成「旧版」，按一次
「更新」就升成新格式。

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

### 会话卡在 400 too long 的时候

Claude Code 按**模型声明的窗口**决定何时自动压缩，而走 ccLoad 时真正拦你的是
**中转那一家的上限**。两个数对不上（典型：模型名挂了 `[1m]`，中转其实只给
500k），阈值就被算在一个不存在的分母上 —— 等它触发已经越过真实天花板了。之后
`/compact` 自己也发不出去，因为它同样要把整段 transcript 发上去。

界面上在「会话救援」页；命令行等价物：

```bash
python3 scripts/rescue-session.py <session.jsonl>          # 只看报告
python3 scripts/rescue-session.py <session.jsonl> --write  # 真的改
```

先退出那个 Claude Code 窗口再动手 —— 进程里有内存态，会把你的修改盖回去。

预防的办法是把真实上限告诉客户端，在「CLI 接管」页的环境变量里填：
`CLAUDE_CODE_MAX_CONTEXT_TOKENS` 填中转的真实上限，
`CLAUDE_CODE_AUTO_COMPACT_WINDOW` 填一个留足余量的值（压缩请求本身也要把整段
对话发一遍，顶着上限做不成任何事）。

---

## 自带的两个 MCP 服务器

客户端二进制自己就是 MCP 服务器 —— 按 `argv[1]` 分流（见 `lib.rs`），所以装进
CLI 的只是一条指向本二进制的 stdio 命令，没有第二个东西要安装或打包。

| 子命令 | 服务器名 | 实现 | 干什么 |
| --- | --- | --- | --- |
| `vision-mcp` | `ccload-vision` | `services/vision_mcp.rs` | 把图交给多模态模型描述 —— 让文本模型「看得见」 |
| `image-mcp` | `ccload-image` | `services/image_mcp.rs` | 文生图 / 改图 —— 让所有模型「画得出」 |

两边共用一套东西，**不要各抄一份**：`Image`、`mcp_text`、`load_source`（含
`[Image N]` 按编号取图）、`same_endpoint`、`record_call` 都在 `vision_mcp` 里，
`pub(crate)` 导出。界面上那段「哪几家装了 / 装 / 卸 / 重写」是
`components/models/McpTargetList.tsx`。

三条容易踩的：

* **工具名要具体。** 宿主模型是看名字决定调不调的。一个泛化的
  `image_op(action=…)` 不会在用户说「给我画个图标」时被想起来，所以是
  `generate_image` / `edit_image` 两个工具，不是一个。
* **结果回路径，不回图。** 一张 1024×1024 的 PNG base64 之后一兆多，塞进工具
  结果等于每生成一张就往 transcript 里灌一兆 —— 正是上一节要清理的东西。图写到
  磁盘，工具只回绝对路径；模型想看就接着调 `describe_image`。
* **生图有两条路，端点由 `auto` 自己挑**（形状照抄内核的
  `admin_testing_image.go`，不是照 OpenAI 文档猜的）：

  | | 端点 | 能改图 |
  | --- | --- | --- |
  | `chat` | `/v1/chat/completions` + `modalities:["image"]` | 能 |
  | `images` | `/v1/images/generations` | 不能 |

  改图只能走 chat —— images 的请求体里没有放输入图的位置。

  **默认是 `auto`，不是 `chat`。** 钉死一条是装的时候选的，代价却是之后每次
  生图都失败，而错误信息说的是上游的话（「这个模型不在这个端点上」），没人会
  想到要回客户端改一个下拉框。`ImageApi::plan()` 按模型名排尝试顺序
  （`grok-imagine` / `gpt-image` / `dall-e` 先 images，其余先 chat，改图只排
  chat），失败时只对 `ApiErr::wrong_surface()` 认定的「端点选错了」重试 ——
  余额、限流、提示词被拒换个端点也是一样的下场，白烧一次计费。

* **images 端点的 body 要按上游那一家写。** 内核对 images 请求族没有注册任何
  跨协议转换（`protocol/types.go` 的
  `supportedTransformFamiliesByClientAndUpstream` 里一条都没有），发什么上游就
  原样收到什么，形状错了没人替我们纠。xAI 不认 `size`，要 `aspect_ratio` +
  `resolution`，也不认 `n`；`dall-e` 默认回链接，得显式要 `b64_json`；
  `gpt-image` 系列反过来，多送这个字段直接 400。见 `images_body()`。

* **`size` 两种写法都要认。** 工具描述里只有一个 `size` 参数，而 auto 替模型挑
  端点 —— 模型没法知道该写哪种。宽高比（`1:1@2k`，只有
  `1:1 16:9 9:16 3:2 2:3` 五种，档位内核会转大写）和像素（`1024x1536`）互转，
  换算时挑的是**这个模型收得下**的值：`gpt-image` 是 `1536x1024`，`dall-e 3` 是
  `1792x1024`，等比算一个「数学上对」的尺寸发出去只会 400。

* **扩展名按文件头定，不信 MIME。** images 端点回的是裸 base64，
  `extract_images_api` 只能给它套一个写死 `image/png` 的壳，而 xAI 实际回的是
  JPEG。存成 `.png` 的 JPEG 看图工具不在乎，按扩展名分发的（打包器、上传接口）
  当场报错，且报错指向文件本身，没人会想到是生图那一步给错了名字。见
  `sniff_ext()`。

装了不等于会被用：**「系统注入」页里还要勾上对应那一段**，把「什么时候该想起
这个工具」写进各 CLI 的全局指令文件（`services/system_inject.rs`）。那两段里的
工具名和 `tool_specs()` 必须一致，有测试盯着。

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
  三个反复踩的点：
  - `t` 来自组件里的 `useT()`。**模块作用域没有它** —— `TIER_LABELS` /
    `GROUPS` / `TOOL_LABELS` 这类常量表要在**使用处**翻译（`t(group.title)`），
    在定义处包一层既编译不过，也把已经正确的做法改坏。
  - `map((t) => …)` 这种参数会**遮住**翻译函数。回调参数别叫 `t`。
  - 模块级**纯函数**（`draftProblem` / `hopVerdict` 这种返回文案的）没有 hook，
    把 `t: Translate` 当参数传进去，别在里面硬编码中文。
  - 中文文案里**不要靠源码换行断句**：JSX 会把换行折成一个空格，英文正好，
    中文就成了句中多一个空格（「各家的原生 格式」）。
  - 拼接的句子要**整句**进词典。`{n} + "个 CLI 支持" + {kind}` 拼出来中文就已经
    黏在一起，换成英文语序和空格规则都不一样，只有整句有救。
  - 全局扫一遍用两个 codemod，都得跑到 0 命中：
    `python3 scripts/i18n-wrap.py`（属性与字符串字面量）、
    `node scripts/i18n-jsx.mjs`（JSX 文本节点，走 TS 解析器）。不带参数是 dry run，
    改完必须 `pnpm typecheck` 兜底。
  - **静态扫描不够**。只有交互才出现的文案（弹窗标题、表单校验、逐层体检结论）
    和 `title` / `aria-label` / `placeholder` 都扫不到，得真的把页面点开看：
    `npx vite --port 5399` 起 dev server，再用 Playwright 遍历每一页 × 两种语言，
    正文和这三个属性一起查。

---

## 发版

两条线，互不干扰。

**beta —— 不打 tag，跑 workflow。** Actions → **Beta Release** → Run workflow，填
分支即可（`workflow_dispatch`，见 `.github/workflows/beta.yml`）。命令行等价物是

```bash
gh workflow run beta.yml -f ref=main
```

输入项叫 **`ref`** 不是 `branch` —— 名字写错的话 GitHub 直接回 422
`Unexpected inputs provided`，网页上是下拉选所以碰不到这个坑。版本号由流水线
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

**跟内核版本 —— 自动。** `.github/workflows/kernel-sync.yml` 每小时看一眼上游最新
release，和 `KERNEL_VERSION` 不一样就跟上、提交、派发一次 beta。几个点：

* 取版本用 `/releases` 而不是 `/releases/latest` —— 后者按定义跳过 prerelease，
  而内核发的正是 beta（实测会停在 v4.7.0，落后三个 beta 却看着像最新）。
* **提交之前先 `kernel:build` 编一遍。** 内核偶尔会引入新的构建前置
  （v4.7.3 加的 `third_party/cursor-sdk-bridge` 就是一次），编不出来该红在这条
  流水线上，而不是把一个编不出来的钉子推上 `main`。
* 打包必须显式 `gh workflow run`：GITHUB_TOKEN 推的 commit 不会触发其它
  workflow，这是 GitHub 防死循环的硬规则。
* 手动触发能指定 tag（回退某一版，或抢在 release 前试），`dry_run` 只跑到编译
  为止，不提交也不打包。

要临时停掉跟版就在 Actions 里禁用这条 workflow；别改 cron 表达式来「关掉它」，
下一个改的人看不出那是关的意思。

---

## 提交之前

* `pnpm typecheck` / `cargo clippy -D warnings` / `cargo test` 三条全绿。
* 别把这些提上去：内核二进制（约 128 MB）、`data/`、`*.db`、截图、
  `~/.claude` 之类的真实配置、任何 API key。`.gitignore` 已覆盖常见的，
  但仍然请 `git status` 看一眼再 `git add`。
* 提到具体渠道/域名时用占位符（`https://example.com`），不要写自己在用的中转站。
