<p align="center">
  <img src="docs/assets/logo.png" width="88" height="88" alt="ccLoad" />
</p>

# ccLoad Desktop

**给 Claude Code、Codex、Gemini、Grok Build、OpenCode 用的 ccLoad 桌面端。**

**[English](./README.md) | 简体中文**

> 一键接管 CLI | 本机或远端网关 | 不冲掉你的 MCP | 写错可回滚 | 沙箱可先试

<p align="center">
  <img src="docs/assets/hero.png" alt="ccLoad Desktop — 一个网关，接管所有 CLI" />
</p>

ccLoad 已经把多上游的脏活做完了：选渠道、故障切换、协议转换、看用量。还散落在电脑上的，是另一摊 —— 五家 CLI、五套「请求发到哪」，换一次网关要贴五次地址，用切换器还可能把 MCP 和正在用的模型一起盖掉。

ccLoad Desktop 用一个窗口接住这件事：拉起本机网关，或接到你已经在跑的那台，把你真正在用的 CLI 指过去。你还在原来的终端里干活，渠道和密钥仍在 ccLoad 里管。

## 谁、什么、为什么、何时、在哪、怎么用

| | |
|---|---|
| **谁** | 已经在用 Claude Code / Codex / Gemini CLI / Grok Build / OpenCode 里至少一家，并且用（或打算用）[ccLoad](https://github.com/caidaoli/ccLoad) 当网关的人。 |
| **是什么** | 桌面里的总闸。不是再做一个代理，是把网关和这五家 CLI 接上。 |
| **为什么** | 网关侧的路由、冷却、账单 ccLoad 已经管了。电脑侧还在手工改五份配置，漏一家会话就会打到旧地址上；不少切换器会整份覆盖，MCP 和当前模型跟着没。 |
| **何时** | 新装一家 CLI、换一条网关、把朋友的实例指过来、怀疑某家还停在旧入口、想先试而不动正在跑的会话。 |
| **在哪** | 网关可以在这台电脑上，也可以是家里那台、和别人共用的那台。CLI 仍在你平时的终端里。Windows / macOS / Linux 都有安装包；macOS 一份同时覆盖 Intel 和苹果芯片。 |
| **怎么用** | 打开应用 → 选本机启动或填已有地址 → 给要用的 CLI 点接管 → 该怎么用 CLI 还怎么用 → 回到这个窗口看用量、健康和日志。 |

<p align="center">
  <img src="docs/assets/flow.png" alt="五家 CLI → 桌面客户端 → 你的网关" />
</p>

## 解决什么问题

同时开着好几家 AI CLI 时，真正麻烦的是这些问题：

- **入口要贴五遍**：每家都有自己的「请求发到哪」。换网关漏一家，那次对话会打到旧地址上，做到一半断掉。
- **切换器把现场冲掉**：整份配置被盖掉之后，MCP 没了，你正在用的模型也不见了。
- **指错了回不去**：没有快照就只能凭记忆或整盘备份往回改。
- **想先试又怕动正在跑的会话**：开发、对比两条网关时，不该拿正在用的 `~/.claude` 做实验。
- **网关在跑，电脑这边看不到**：用量、进行中的请求、渠道是否健康，还得另开浏览器去猜。

ccLoad Desktop 直接处理这些问题：

- **一次接管，五家一起走**：只点你真正在用的那些。不必打开五份配置文件。
- **只补该补的，不整份覆盖**：MCP、自己加的模型、你手写的其它字段留着。导入是往目录里追加，不会把你当前选中的模型切走。
- **写之前先留底**：每个 CLI 单独快照。回滚只动这一家。最初那份备份不会被删掉。
- **沙箱先试**：打开后写入落到旁边的目录，真实配置原样留着，确认再关掉沙箱。
- **总览和网关对得上**：花费、健康、进行中的请求、日志，数字来自同一份 ccLoad，不是另一套计数。

## 左侧菜单：每一页做什么

侧栏按「先看发生了什么 → 再改它怎么跑 → 最后动环境」分成三组。下面按菜单顺序走一遍。界面图按真实壳体排版（浅色侧栏、分组、左下角内核状态），数字是示意。

### 监控 · 总览

打开应用先看这里：今天打了多少、成功多少、近一分钟有多忙、花了多少。下面是请求曲线、每家渠道是否健康、哪个模型在烧钱。数字来自内核，客户端只做聚合。

![总览](docs/assets/ui/page-dashboard.png)

### 监控 · 实时日志

上半是正在飞的请求（还没写进历史），下半是已经结束的记录。可以按模型、渠道、状态码筛，也可以只看错误。不想一直打内核就关掉「实时」，改成手动刷新。

![实时日志](docs/assets/ui/page-logs.png)

### 配置 · CLI 接管

把 Claude Code / Codex / Gemini CLI / Grok Build / OpenCode 指到当前网关。已经指过的可以再写一次（修手工改坏的配置）。写入前自动快照，右上角进「快照历史」回滚。先去设置里开沙箱的话，这里动的只是旁边一份目录。

![CLI 接管](docs/assets/ui/page-cli.png)

### 配置 · 调度图

用一张表说清「哪种活用哪家的哪个模型」。应用之后，CLI 只认档位别名（opus / sonnet / …），换家、重试、冷却仍由内核做。顺序互相矛盾时会拦住，不会默默挑一个。

![调度图](docs/assets/ui/page-graph.png)

### 配置 · 模型链

一条 fallback：例如 opus 先走这家，不行再走那家。写成按优先级递减的渠道，内核选择器会自动走完。和调度图配合：图管「平时怎么分」，链管「挂了怎么退」。

![模型链](docs/assets/ui/page-fallback.png)

### 配置 · 模型导入

把启用中渠道真正能提供的名字，追加进 Codex / OpenCode 的列表。Claude Code 没有目录文件，只有几个槽位：你选了 opus / sonnet / … 才写，没选的行跳过，避免盖掉你正在用的模型。

![模型导入](docs/assets/ui/page-models.png)

### 配置 · 扩展管理

MCP / Skill / Agent / Hook 一行一项，右边徽章标出装在哪几家 CLI。改一处，推到装了它的每一家；各家自己的文件格式由客户端转换，写之前同样先快照。

![扩展管理](docs/assets/ui/page-extensions.png)

### 系统 · 内核后台

渠道、令牌、内核自己的日志和设置，打开的是 ccLoad 自带的管理界面（独立窗口），字段跟着内核升级走，这里不做一套阉割表单。

![内核后台](docs/assets/ui/page-web-admin.png)

### 系统 · 设置

网关在哪：本机托管，或填已经在跑的地址。开发期打开「CLI 写入走沙箱」。下面列出 Anthropic / OpenAI / Gemini 三套入口，给不走 CLI 接管的工具直接复制。

![设置](docs/assets/ui/page-settings.png)

## 下载

[Releases](https://github.com/ChenYCL/ccload-client/releases)

| 系统 | 你下哪个 |
|---|---|
| macOS | `.dmg` / `.zip`（Intel 和苹果芯片打在同一个文件里） |
| Windows | `.exe` |
| Linux | `.AppImage` 或 `.deb` |

macOS 目前是未签名包：第一次打开请右键应用 → 打开。

开发期请打开「CLI 写入走沙箱」。从源码跑、以及给贡献者的约定见 [AGENTS.md](./AGENTS.md)。

## 许可

MIT，见 [LICENSE](./LICENSE)。打包进去的 [ccLoad](https://github.com/caidaoli/ccLoad) 内核同样是 MIT。
