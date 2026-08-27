import { registerDict } from "./index";

/// 英文词典。key 是界面里的中文原文。
///
/// 没收录的条目会原样回落成中文，所以这份表可以逐页补齐而不会让界面出现空洞。
/// 补的时候直接从组件里把中文抄过来当 key。

registerDict("en", {
  // ---- 导航与外壳 ----
  监控: "Monitor",
  配置: "Configure",
  系统: "System",
  总览: "Overview",
  实时日志: "Live logs",
  订阅用量: "Subscription usage",
  会话管理: "Session manager",
  "CLI 接管": "CLI takeover",
  调度图: "Dispatch graph",
  模型链: "Model chain",
  模型导入: "Model import",
  系统注入: "System injection",
  破禁: "Unlock",
  扩展管理: "Extensions",
  内核后台: "Kernel admin",
  设置: "Settings",
  启动内核: "Start kernel",
  "启动中…": "Starting…",
  停止内核: "Stop kernel",
  "停止中…": "Stopping…",
  运行中: "Running",
  已停止: "Stopped",
  "启动中（约 20s）": "Starting (~20s)",
  展开侧栏: "Expand sidebar",
  收起侧栏: "Collapse sidebar",
  语言: "Language",
  客户端版本: "Client version",
  有新版本: "Update available",
  "有新 beta": "New beta",
  "有新版本 {v}": "Update available: {v}",
  "在浏览器里打开 {v} 的发布页": "Open the {v} release page in your browser",
  当前版本: "Current version",
  最新发布: "Latest release",
  检查更新: "Check for updates",
  "检查中…": "Checking…",
  "去下载新 beta": "Get the new beta",
  去下载新版本: "Get the new version",
  "这次没查到：": "Couldn't check this time: ",
  "已是最新。": "You're on the latest version.",
  "每小时自动查一次 GitHub Releases，回到窗口时若已过期也会补查。只读取版本号，不会自动下载或替换 —— 有新版时侧栏版本号下面会出现一个按钮，点开浏览器由你自己决定。":
    "Checks GitHub Releases once an hour, plus whenever you return to the window and the last check has gone stale. It only reads the version number — nothing is downloaded or replaced automatically. When there's a new version, a button appears under the version in the sidebar; it opens your browser and the rest is up to you.",

  // ---- 通用动作 ----
  保存: "Save",
  取消: "Cancel",
  删除: "Delete",
  编辑: "Edit",
  应用: "Apply",
  新建: "New",
  关闭: "Close",
  刷新: "Refresh",
  安装: "Install",
  移除: "Remove",
  已安装: "Installed",
  未安装: "Not installed",
  "读取中…": "Loading…",
  "写入中…": "Writing…",
  "拉取中…": "Fetching…",
  保存改动: "Save changes",
  已保存: "Saved",

  // ---- 总览 ----
  "全部数字来自内核 Admin API 的真实字段，客户端只做聚合，不做估算。":
    "Every number comes from a real field of the kernel's admin API. The client only aggregates — it never estimates.",
  本日: "Today",
  昨日: "Yesterday",
  本周: "This week",
  本月: "This month",
  "内核未运行，没有可展示的数据。": "The kernel isn't running, so there's nothing to show.",
  "从左下角「启动内核」开始。": "Start it from the bottom-left corner.",
  请求量与成功率: "Request volume and success rate",
  渠道健康: "Channel health",
  模型消耗: "Model spend",
  模型明细: "Model breakdown",
  渠道消耗: "Channel spend",
  渠道明细: "Channel breakdown",
  没有渠道用量: "No channel usage",
  "stats[] 按渠道聚合": "stats[] grouped by channel",
  "条长 = effective_cost（实付费用），右侧是它占总消耗的比例，颜色 = 该渠道成功率":
    "Bar length = effective_cost (what you actually pay); the figure on the right is its share of total spend; colour = that channel's success rate",
  占比: "Share",
  实付: "Actual",
  "Token 合计": "Total tok",
  "· 标准价": "· list price",
  "· 倍率": "· multiplier",
  // ---- 用量合计 ----
  用量合计: "Usage totals",
  "stats[] 全渠道 × 全模型合计": "stats[] summed over every channel × model",
  "{n} 次请求": "{n} requests",
  "平均每次 {n} tok": "{n} tok per request on average",
  "缓存读取占输入侧 {n}": "cache reads are {n} of all input-side tokens",
  "该时间段没有任何请求": "No requests in this period",
  "每桶 {n} 分钟": "{n} min per bucket",
  "每桶 {n} 小时": "{n} h per bucket",
  "{n} 个请求进行中": "{n} request(s) in flight",

  // ---- 实时日志 ----
  进行中: "In flight",
  历史日志: "History",
  实时开: "Live on",
  实时关: "Live off",
  刷新一次: "Refresh once",
  实时跟随: "Following",
  已暂停: "Paused",
  当前没有进行中的请求: "No requests in flight",
  全部模型: "All models",
  全部渠道: "All channels",
  全部状态码: "All status codes",
  只看错误: "Errors only",
  时间: "Time",
  状态: "Status",
  模型: "Model",
  渠道: "Channel",
  耗时: "Duration",
  首字节: "TTFB",
  费用: "Cost",
  "费用（倍率后）": "Cost (after multiplier)",
  标准成本: "List cost",
  渠道倍率: "Channel multiplier",
  "内核没有日志推送通道，这里是轮询：进行中 {a}s、历史 {b}s，窗口切走时自动暂停。":
    "The kernel has no log push channel, so this polls: {a}s for in-flight, {b}s for history. It pauses when the window loses focus.",
  "实时已关闭，不再向内核发起任何轮询；下面显示的是最后一次取到的数据。":
    "Live mode is off — no polling at all. What you see below is the last fetch.",

  // ---- 调度图 ----
  "校验通过。全局优先级顺序：": "Validation passed. Global priority order: ",
  "校验通过。": "Validation passed.",
  全局顺序: "Global order",
  "拖动调整各档队列的排列。应用到内核时只写别名映射，不改渠道绑定，也不改渠道优先级。":
    "Drag to reorder each tier’s queue. Applying to the kernel only writes alias mappings — it does not change channel bindings or channel priority.",
  "{name}，第 {n} 位，可拖动或按左右键调整":
    "{name}, position {n}, drag or use the left/right keys to reorder",
  "别名是 CLI 侧实际请求的模型名。队列从上到下是全局顺序在这一档的投影；加入或移除只改变谁参与，不会改渠道绑定。":
    "The alias is the model name the CLI actually requests. The queue is this tier’s slice of the global order; adding or removing only changes who participates, not channel bindings.",
  "校验未通过，无法应用（不会写入任何东西）":
    "Validation failed — nothing will be written",
  "内核未运行，读不到渠道列表": "Kernel isn't running, so the channel list is unavailable",
  按名称自动匹配: "Auto-match by name",
  应用到内核: "Apply to kernel",
  档位与队列: "Tiers and queues",
  角色映射: "Role mapping",
  别名: "Alias",
  启用: "Enabled",
  未绑定: "Not bound",

  // ---- 模型导入 ----
  上游校验: "Upstream check",
  拉取全部渠道的上游模型: "Fetch upstream models from every channel",
  只选精确命中: "Select exact matches only",
  含模糊命中: "Include fuzzy matches",
  模型别名: "Model alias",
  上下文窗口: "Context window",
  视觉: "Vision",
  上游: "Upstream",
  精确: "Exact",
  模糊: "Fuzzy",
  上游无: "Not upstream",
  原生: "Native",
  需辅助: "Needs help",
  目录: "Catalog",
  填充默认值: "Fill defaults",
  已同步: "Synced",
  视觉辅助: "Vision assist",
  写入到: "Write to",
  "Claude Code 没有模型目录，只能绑 5 个槽位。现在勾选的行全是「不绑定」，导入不会改 Claude Code。":
    "Claude Code has no model catalog — only 5 slots. Every checked row is unbound, so import will not change Claude Code.",
  按名称填槽位: "Fill slots by name",
  把第一个勾选的设为主模型: "Bind the first checked model as default",

  // ---- 模型链 ----
  新建链: "New chain",
  编辑模型链: "Edit model chain",
  添加一层: "Add a hop",
  "上游模型，例如 kimi-k3": "Upstream model, e.g. kimi-k3",
  选择渠道: "Pick a channel",

  // ---- 强制路由 ----
  强制路由: "Forced route",
  "CLI 请求某个模型名，就强制把它发到你选的渠道 + 上游模型。选一个渠道，联动列出它的模型，勾多个即可 —— 不校验上游，手填任意名字照样发。和「模型链」的区别是心智：那边讲主力冷了往下降级，这里是「我说发去哪就发去哪」。":
    "When a CLI requests a given model name, force it to the channel + upstream model you pick. Choose a channel, its models cascade below — check as many as you like; the upstream is never validated, so a name you type by hand is sent all the same. The difference from Model chain is the mindset: that one degrades gracefully when the primary cools; this one just sends it where you say.",
  新建路由: "New route",
  "还没有路由。点「新建路由」把第一个别名钉到一个渠道+模型上。":
    "No routes yet. Click New route to pin your first alias to a channel + model.",
  "把这条路由写进各目标渠道": "Write this route into each target channel",
  "删除路由 {from}": "Delete route {from}",
  "（还没有目标）": "(no targets yet)",
  "第 {n} 个": "#{n}",
  首选: "Primary",
  "备用 {n}": "Backup {n}",
  " · 渠道已禁用，不会被选中": " · channel disabled, will not be picked",
  " · 没绑渠道，应用时跳过": " · no channel bound, skipped on apply",
  未绑渠道: "no channel",
  编辑强制路由: "Edit forced route",
  "命中「请求别名」就强制发到下面的目标。多个目标按序：第一个是首选，命中即用，后面的是备用落点。应用时会把目标排到现有服务该别名的渠道之上，确保独占而不是被平分。":
    "A request matching the alias is forced to the targets below. Multiple targets are ordered: the first is primary and is used when reachable; the rest are backups. On apply, targets are pushed above any existing channel serving this alias, so it's an exclusive take-over rather than a 50/50 split.",
  "请求别名（CLI 里写的模型名）": "Request alias (the model name the CLI sends)",
  请求别名: "Request alias",
  "内核里还没有渠道；名字可以手填": "No channels in the kernel yet; you can type the name",
  "目标（按序，第一个优先级最高）": "Targets (in order; the first has the highest priority)",
  "还没有目标。在下面选个渠道、勾几个模型，点「加入选中」。":
    "No targets yet. Pick a channel below, check a few models, then click Add selected.",
  上移: "Move up",
  下移: "Move down",
  移除这个目标: "Remove this target",
  批量添加目标: "Add targets in bulk",
  目标渠道: "Target channel",
  "（已禁用）": " (disabled)",
  "这个渠道还没配模型 —— 下面手填要发的模型名。":
    "This channel has no models configured — type the model name to send below.",
  "或手填一个模型名（不校验上游，照发）":
    "Or type a model name (upstream not validated, sent as-is)",
  手填模型名: "Type a model name",
  加入选中: "Add selected",
  "（{n}）": " ({n})",

  // ---- 扩展管理 ----
  搜索名称或描述: "Search name or description",
  外部工具服务器: "External tool servers",
  生命周期钩子: "Lifecycle hooks",

  // ---- 设置 ----
  内核版本: "Kernel version",
  壳体打包内核: "Kernel bundled with this app",
  远端内核: "Remote kernel",
  当前运行内核: "Running kernel",
  接入地址: "Endpoints",
  配置迁移: "Config migration",
  导出配置: "Export config",
  导入配置: "Import config",
  确认导入: "Confirm import",
  "包含密钥（管理密码与 API 令牌）": "Include secrets (admin password and API token)",
  "CLI 写入": "CLI writes",
  端口: "Port",
  远端地址: "Remote URL",
  管理密码: "Admin password",
  复制: "Copy",
  已复制: "Copied",
  显示: "Show",
  隐藏: "Hide",
  "API 令牌": "API token",

  // ---- 订阅用量 ----
  刷新额度: "Refresh quota",
  "刷新中…": "Refreshing…",
  已禁用: "Disabled",
  "读取渠道中…": "Loading channels…",
  "各 OAuth 渠道的套餐额度窗口与剩余量。数据由内核在刷新凭证时向上游采样，这里只是读回来 —— 点「刷新额度」会真的去问一次上游。API Key 渠道按量计费、没有套餐窗口，不在这一页。":
    "Plan quota windows and remaining allowance for each OAuth channel. The kernel samples these from upstream when it refreshes credentials; this page just reads them back — “Refresh quota” actually asks upstream again. API-key channels are pay-as-you-go with no plan window, so they are not listed here.",
  "还没有 OAuth 订阅渠道。去「内核后台」用 Codex / Anthropic / Antigravity / xAI / Z.ai 登录后，这里才会有额度可看。":
    "No OAuth subscription channels yet. Sign in with Codex / Anthropic / Antigravity / xAI / Z.ai under “Kernel admin” and quotas will show up here.",
  "还没采到额度窗口。内核只在刷新凭证或你点「刷新」时才向上游采样；有的套餐本来就不提供额度端点。":
    "No quota window sampled yet. The kernel only samples upstream when it refreshes credentials or when you hit “Refresh”; some plans expose no quota endpoint at all.",
  "已刷新 {n} 个渠道的额度": "Refreshed quota for {n} channel(s)",
  // 窗口时长与重置倒计时
  额度窗口: "Quota window",
  每周: "Weekly",
  每月: "Monthly",
  "{n} 分钟": "{n} min",
  "{n} 小时": "{n}h",
  "{n} 天": "{n}d",
  即将重置: "Resetting now",
  "{n} 分钟后重置": "resets in {n} min",
  "{n} 小时后重置": "resets in {n}h",
  "{n} 天后重置": "resets in {n}d",
  "剩余 {n}%": "{n}% left",
  "已用 {n}%": "{n}% used",
  本窗口计费: "Billed this window",
  "本窗口内累计的标准成本，未乘渠道倍率":
    "Standard cost accumulated in this window; channel multiplier not applied",
  // 额度名里用来区分同长度窗口的那一点差异
  额度: "credits",
  时长: "time",
  次数: "count",

  // ---- MCP 工具调用 ----
  "MCP 工具调用": "MCP tool calls",
  看图: "describe image",
  抄图上的字: "transcribe text",
  比对两张图: "compare images",
  看当前屏幕: "capture screen",
  生成图片: "generate image",
  修改图片: "edit image",
  "还没有调用记录。装上「模型导入」页里的视觉辅助 MCP 之后，文本模型每次看图都会记一笔。":
    "No calls recorded yet. Install the vision-assist MCP from the “Model import” page and every image a text-only model looks at gets logged here.",
  "只统计本客户端自带的 MCP 服务器（ccload-vision / ccload-image）。扩展管理里装的第三方 MCP 由 CLI 直接拉起，不经过内核也不经过客户端，无法计入。":
    "Covers only this app's own MCP servers (ccload-vision / ccload-image). Third-party MCP servers installed under Extensions are spawned directly by the CLI — they pass through neither the kernel nor this client, so they cannot be counted.",
  "共 {n} 次调用": "{n} calls",
  "累计耗时 {d}": "{d} total",
  "失败 {n} 次": "{n} failed",
  清空统计: "Clear stats",
  "{n} 次": "{n}×",
  "均 {d}": "avg {d}",
  "峰 {d}": "max {d}",
  "失败 {n}": "{n} failed",
  "平均耗时（仅成功调用）": "Average duration (successful calls only)",
  最慢一次: "Slowest call",
  "统计自 {at}。": "Since {at}.",
  "更早的记录已被丢弃（流水有大小上限）。":
    "Older records were dropped (the log has a size cap).",

  // ---- 系统注入 ----
  "把一段受管的说明写进每个 CLI 的全局指令文件（CLAUDE.md / AGENTS.md / GEMINI.md），启动时无条件进系统提示。只替换我们自己那对标记之间的内容，块外一个字节都不动 —— 你原有的规则不会被覆盖。":
    "Writes a managed block into each CLI's global instructions file (CLAUDE.md / AGENTS.md / GEMINI.md), which is loaded into the system prompt on every launch. Only the content between our own markers is replaced — not a byte outside the block is touched, so your existing rules stay intact.",
  "告诉 CLI 怎么用视觉辅助 MCP": "Tell the CLI how to use the vision-assist MCP",
  "装上 ccload-vision 不等于模型会用它 —— 它只看得见工具名和一句描述，遇到图片会不会想起来调全看运气，而文本模型甚至不知道自己「看不见」。这段会明确告诉它：你看不见图片，遇到 [Image 1] 这种没有路径的占位符时必须调工具并把 image 设成对应编号，不要让用户把图另存一份。":
    "Installing ccload-vision does not mean the model will use it — all it sees is a tool name and one line of description, so whether it remembers to call the tool on an image is luck, and a text-only model does not even know it is blind. This block says it plainly: you cannot see images; when the chat only shows [Image 1] with no path you must call the tool and set image to that number — do not ask the user to save the file somewhere else.",
  "告诉 CLI 怎么用生图 MCP": "Tell the CLI how to use the image MCP",
  "模型不会主动想到「这张图我自己就能画」，默认反应是让你去找别的工具或者拿 SVG 凑数。这段会告诉它：图标、精灵图、贴图、UI 草图都可以用 generate_image 直接生成，改图用 edit_image（原图不动），以及结果回来的是磁盘路径而不是图本身 —— 要看画成什么样得接着调 describe_image。":
    "A model does not spontaneously think “I could just draw this myself”; its default move is to send you off to another tool or fake it with SVG. This block tells it: icons, sprites, textures and UI mockups can be produced directly with generate_image; edits go through edit_image (the original is never touched); and what comes back is a file path, not the image — to see how it turned out, call describe_image on that path.",
  你自己的规则: "Your own rules",
  "原样写进块里，五个 CLI 共用同一份。留空则只写上面勾选的内容。":
    "Written into the block verbatim and shared by all five CLIs. Leave empty to inject only what is ticked above.",
  "例如：永远用中文回答；提交前必须跑一遍测试。":
    "e.g. Always answer in English; always run the tests before committing.",
  预览将写入的内容: "Preview what will be written",
  "{n} 字符": "{n} chars",
  已注入: "Injected",
  未注入: "Not injected",
  文件不存在: "File does not exist",
  可能超长: "May overflow",
  旧版: "Outdated",
  "这段是旧版本写进去的，内容仍然生效。按「更新」会用当前措辞重写它 —— 先展开上面的预览看一眼要写什么。":
    "This block was written by an older version and is still in effect. “Update” rewrites it with the current wording — expand the preview above first to see exactly what goes in.",
  "Grok Build 会把规则文件截断到 10000 字符，超出的部分静默丢失":
    "Grok Build truncates rule files at 10,000 characters and silently drops the rest",
  写入: "Write",
  更新: "Update",
  "用上面的内容重写这一段": "Rewrite this block with the content above",
  "先勾选或填写要注入的内容": "Tick or type something to inject first",
  批量写入: "Write to selected",
  批量移除: "Remove from selected",
  "已选 {n} 个": "{n} selected",
  取消选择: "Clear selection",
  选中: "Select",
  已写入: "Written",
  已移除: "Removed",
  失败: "failed",
  "写入前会自动快照原文件，在「CLI 接管」页的备份列表里可以一键还原。注入的内容每次请求都会占 token，不需要的项就别勾。":
    "The original file is snapshotted before every write; restore it in one click from the backup list on the “CLI takeover” page. Injected content costs tokens on every request, so leave anything you do not need unticked.",

  // ---- 本轮批量补齐：总览 / 日志 / 扩展 / 设置 / 调度图 等页 ----
  "---\nname: my-skill\ndescription: …\n---\n\n正文…": "---\nname: my-skill\ndescription: …\n---\n\nBody…",
  "Anthropic 规范": "Anthropic format",
  "Claude Code 没有模型目录文件：请在 Tier 列给至少一个模型选一个槽位":
    "Claude Code has no model catalog file: pick a tier slot for at least one model in the Tier column",
  "GET /admin/active-requests · 内核内存态": "GET /admin/active-requests · kernel in-memory state",
  "Gemini 规范": "Gemini format",
  "OpenAI 规范": "OpenAI format",
  "[profiles.别名]": "[profiles.<alias>]",
  "ccload-vision · 本地流水": "ccload-vision · local log",
  "http / sse（远端 URL）": "http / sse (remote URL)",
  "stats[] 按模型聚合": "stats[] grouped by model",
  "stdio（本地进程）": "stdio (local process)",
  "← 返回接管": "← Back to takeover",
  "。而 ccLoad 上游没有": ", and your ccLoad upstream has no",
  一句话说明它是干什么的: "One line on what it does",
  "一行一个参数，顺序即命令行顺序": "One argument per line, in command-line order",
  上游协议: "Upstream protocol",
  上游地址: "Upstream URL",
  上游模型名: "Upstream model name",
  上游没有返回任何模型: "Upstream returned no models",
  上游返回: "upstream returned",
  中文: "中文",
  "事件触发时执行的 shell 命令": "Shell command to run when the event fires",
  令牌: "Token",
  传给该进程的环境变量: "Environment variables passed to the process",
  传输方式: "Transport",
  "保存中…": "Saving…",
  健康时间线: "Health timeline",
  先保存再应用: "Save before applying",
  先在上面选一个多模态模型: "Pick a multimodal model above first",
  全不选: "Deselect all",
  全选: "Select all",
  "关闭实时轮询，省下持续的内核请求": "Stop live polling and save the constant kernel requests",
  内核在本机监听的回环端口: "Loopback port the kernel listens on",
  "内核已按桶补齐时间轴，空数组说明区间内确实没有流量":
    "The kernel already pads the timeline bucket by bucket, so an empty array means there really was no traffic",
  "内核未运行，没有日志可看。": "Kernel is not running, so there are no logs.",
  内核运行时配置: "Kernel runtime settings",
  "内核里还没有渠道或渠道没有配置模型，先去「内核后台」添加。":
    "The kernel has no channels yet, or its channels have no models configured. Add them under “Kernel admin” first.",
  写入内核渠道: "Write to kernel channels",
  "写入前会自动快照，可在 CLI 接管页回滚":
    "A snapshot is taken before writing; roll it back on the CLI takeover page",
  写死的: "hard-coded",
  "别名（CLI 里写的模型名）": "Alias (the model name you type in the CLI)",
  "加入：": "Adds:",
  加载中: "Loading",
  "匹配哪些工具，如 Bash|Write；留空等于全部": "Which tools to match, e.g. Bash|Write; empty means all",
  即将导入: "About to import",
  "可执行文件，如 npx": "Executable, e.g. npx",
  可留空: "optional",
  同步中: "Syncing",
  "同步中…": "Syncing…",
  "同步到其他 CLI": "Sync to other CLIs",
  "名称（id）": "Name (id)",
  否: "No",
  "启用（取消勾选会把 enabled: false 写进配置）": "Enabled (unticking writes enabled: false into the config)",
  命令: "Command",
  在浏览器中打开: "Open in browser",
  "处理中…": "Working…",
  失败请求: "Failed requests",
  "如 Authorization": "e.g. Authorization",
  安装失败: "install failed",
  "完整 origin，例如 https://xxx.hf.space": "Full origin, e.g. https://xxx.hf.space",
  实际模型: "Actual model",
  "客户端 IP": "Client IP",
  "客户端为自己申请的那把，可在内核后台吊销":
    "The one this client minted for itself; revoke it in the kernel admin",
  客户端协议: "Client protocol",
  "客户端自己拉起内核进程，数据留在本机":
    "The client spawns the kernel process; data stays on this machine",
  "导入中…": "Importing…",
  已有的: "existing",
  "已装 · 会覆盖": "installed · will overwrite",
  已装位置: "Installed at",
  "平均 RPM": "Average RPM",
  平均首字: "Avg first byte",
  开启实时轮询: "Turn live polling on",
  "开头带 --- frontmatter 就原样写入，否则用名称 + 描述合成一份最小的":
    "Written verbatim if it starts with --- frontmatter; otherwise a minimal one is synthesised from name + description",
  "强制 fallback 模型": "Forced fallback model",
  必填: "required",
  快照历史: "Snapshot history",
  思考强度: "Thinking effort",
  "总是跳到 claude-opus-4-8？": "Always jumping to claude-opus-4-8?",
  总耗时: "Total time",
  恢复为本机现值: "Reset to this machine's value",
  成功: "Success",
  成功率: "Success rate",
  "成功率低于 90%": "Success rate below 90%",
  "成功率（右轴）": "Success rate (right axis)",
  成功请求: "Successful requests",
  技能目录名: "Skill directory name",
  拖动排序: "Drag to reorder",
  "按 models.dev 目录与本地预设重填所有行的上下文窗口":
    "Refill every row's context window from the models.dev catalog and local presets",
  "按名称和 URL 没猜出任何一家，需要手动选。":
    "Could not guess any provider from names and URLs — pick them manually.",
  按模型筛选: "Filter by model",
  "按渠道名称和 URL 里的关键词猜，填完仍需自己核对":
    "Guesses from keywords in channel names and URLs; still check the result yourself",
  按渠道筛选: "Filter by channel",
  按状态码筛选: "Filter by status code",
  "换个时间范围，或先让 CLI 发一次请求": "Try another time range, or send one request from a CLI first",
  推理: "Reasoning",
  描述: "Description",
  收起原始配置: "Hide raw config",
  "改动未保存，保存后需重启生效": "Unsaved changes; a restart is needed after saving",
  "文件名（不含 .md）": "File name (without .md)",
  文件将创建: "File will be created",
  文件已存在: "File exists",
  无: "None",
  无失败请求: "No failed requests",
  日志详情: "Log detail",
  是: "Yes",
  最近的日志里没有错误: "No errors in the recent logs",
  未装: "not installed",
  未设置: "Not set",
  本日还没有日志: "No logs today yet",
  本机内核: "Local kernel",
  "条长 = effective_cost（乘过渠道倍率的实付费用），颜色 = 该模型成功率":
    "Bar length = effective_cost (what you actually pay, after the channel multiplier); colour = that model's success rate",
  来源: "Source",
  "来自 models.dev 目录": "From the models.dev catalog",
  查看原始配置: "Show raw config",
  校验上游模型: "Verify upstream models",
  "校验中…": "Verifying…",
  "校验未通过，不能应用": "Validation failed — cannot apply",
  "正文（markdown）": "Body (markdown)",
  没有模型用量: "No model usage",
  没有渠道健康数据: "No channel health data",
  "没给任何模型选 Tier 槽位，配置未改动；其它 CLI 见各自那行":
    "No model was given a tier slot, so the config was left alone; see each other CLI's own row",
  流式: "Streaming",
  "清单之外的项（点路径或自定义 KEY）": "Keys outside the catalog (dotted path or custom KEY)",
  清空: "Clear",
  渠道级: "Channel-level",
  "点右上角「新建」装一个，或先在某个 CLI 里装好再回来同步":
    "Hit “New” in the top right to install one, or install it in a CLI first and come back to sync",
  用哪个模型看图: "Which model looks at images",
  用当前选中的模型和内核凭证重写这一条:
    "Rewrite this entry with the selected model and current kernel credentials",
  留空则不写入: "Leave empty to skip",
  "留空用 CLI 默认值": "Empty uses the CLI default",
  目录不可用: "Catalog unavailable",
  真实上游模型名: "Real upstream model name",
  移除失败: "remove failed",
  "筛选变量名或说明…": "Filter by name or description…",
  "管理密码（你在设置里填的 CCLOAD_PASS）": "Admin password (the CCLOAD_PASS you set in Settings)",
  "管理密码（本机生成）": "Admin password (generated locally)",
  缓存写入: "Cache write",
  缓存读取: "Cache read",
  "自动（第一个装了它的）": "Auto (first CLI that has it)",
  "装到哪个 CLI": "Install into which CLI",
  "规范事件名，写入时按目标 CLI 翻译（Gemini 的 PreToolUse 叫 BeforeTool）":
    "Canonical event name, translated per CLI on write (Gemini calls PreToolUse BeforeTool)",
  "让 CLI 发一次请求就会出现": "Send one request from a CLI and it will show up",
  该时间段内没有任何渠道产生过请求: "No channel served a request in this time range",
  该时间段没有请求: "No requests in this time range",
  "该时间段费用全部为 0，改按请求数排序":
    "Every cost is 0 in this range, so it is sorted by request count instead",
  请求: "Requests",
  请求数: "Requests",
  费用与来源: "Cost and source",
  "超时（秒）": "Timeout (seconds)",
  路由: "Routing",
  输入: "Input",
  输出: "Output",
  "输出 tok": "output tok",
  过载或不可用: "overloaded or unavailable",
  "近一分钟 RPM": "RPM (last minute)",
  "还没有任何快照。": "No snapshots yet.",
  "还没有调度图。": "No dispatch graph yet.",
  这一行: "this row",
  "远端 ccLoad": "Remote ccLoad",
  "远端模式填该实例的 CCLOAD_PASS": "In remote mode, use that instance's CCLOAD_PASS",
  "连到已有实例（VPS / HF Space）": "Connect to an existing instance (VPS / HF Space)",
  连接方式: "Connection",
  追加: "Append",
  选择多模态模型: "Pick a multimodal model",
  "逐个渠道去问上游要真实模型清单，核对每一层的模型名":
    "Ask each channel's upstream for its real model list and check every hop's model name",
  "逐个渠道向上游要一次模型列表，然后取并集":
    "Ask every channel's upstream for a model list once, then take the union",
  配置里的服务器名: "Server name in the config",
  "里面存的内核地址或令牌已经不是当前这个内核的了，重新安装即可修好":
    "The kernel URL or token stored inside is no longer this kernel's — reinstalling fixes it",
  "重启中…": "Restarting…",
  重启内核: "Restart kernel",
  重新写入: "Rewrite",
  重新打开管理窗口: "Reopen the admin window",
  "重新拉取 models.dev 模型目录": "Re-fetch the models.dev catalog",
  闲置: "idle",
  "默认模型 (ANTHROPIC_MODEL)": "Default model (ANTHROPIC_MODEL)",
  "（内核未运行 / 无渠道）": "(kernel not running / no channels)",
  "（影响该渠道服务的所有模型）": "(affects every model this channel serves)",
  "（改动尚未写入，点下面的安装才生效）": "(not written yet — hit Install below to apply)",
  "（文件不含密钥，本机密码保持不变）":
    "(the file carries no secrets; the local password is unchanged)",
  "（未填模型）": "(no model)",
  "（未填）": "(empty)",
  "（未知模型）": "(unknown model)",
  "（未设置）": "(not set)",
  "（未配置）": "(not configured)",
  "（看图）、": "(describe), ",
  "：上游没有可用的 /v1/models 或返回为空": ": upstream has no usable /v1/models, or returned nothing",
  " · 渠道已禁用，这一层不会被选中": " · channel disabled, this hop will never be picked",

  // ---- JSX 文本节点（跨行段落、跟在元素后的文本、被 {expr} 断开的片段）----
  "+ 添加变量": "+ Add variable",
  "MCP / Skill / Agent / Hook 一处配置，推给装了它们的每一个 CLI —— 各家的原生格式（JSON、TOML、markdown 目录）由后端转换，写入前自动快照。":
    "Configure MCP / Skill / Agent / Hook once and push it to every CLI that supports it. The backend converts to each CLI's native format (JSON, TOML, markdown directories) and snapshots before writing.",
  "Opus 4.8 / Opus 5，完全不看": "Opus 4.8 / Opus 5, ignoring",
  "ccLoad 内核只做一层模型重定向，然后按优先级切渠道。这里把一条 fallback 链（例如 fable-5 → kimi-k3 → opus-5）写成一组按优先级递减的渠道，内核的选择器就会自动走完整个链。":
    "The ccLoad kernel does a single model redirect and then switches channels by priority. This page turns a fallback chain (e.g. fable-5 → kimi-k3 → opus-5) into a set of channels with descending priority, so the kernel's existing selector walks the whole chain for you.",
  "ccLoad 内核只做一层模型重定向，然后按优先级切渠道。这里把一条 fallback 链（例如 opus → glm-5.3[1m] → grok-4.6）写成一组按优先级递减的渠道，内核的选择器就会自动走完整个链。应用时还会把链上最窄那一跳的窗口写进 Claude Code，让原生 /compact 按真实天花板提前动手。":
    "The ccLoad kernel does a single model redirect and then switches channels by priority. This page turns a fallback chain (e.g. opus → glm-5.3[1m] → grok-4.6) into a set of channels with descending priority, so the kernel's existing selector walks the whole chain. Applying it also writes the narrowest hop's window into Claude Code, so native /compact fires against the real ceiling instead of a [1m] suffix that is not there.",
  "ccLoad 自带的管理界面，在独立窗口中打开，字段随内核升级自动跟进。":
    "ccLoad's own admin UI, opened in a separate window. Its fields follow the kernel as it is upgraded.",
  "hook 在配置里没有名字，后端用「事件 + 命令」认它。改动命令等于新增一条，原来那条要回列表里单独删除。":
    "Hooks have no name in the config — the backend identifies them by event + command. Changing the command creates a new entry; the old one has to be deleted separately from the list.",
  "models.dev 拉取失败，上下文窗口暂用本地预设值（claude 20 万、gemini 100 万等），联网后点「同步」重试。":
    "Could not reach models.dev, so context windows fall back to local presets (200k for claude, 1M for gemini, …). Hit “Sync” to retry once you are online.",
  "· 均值": "· avg",
  "· 失败": "· failed",
  "· 峰值 QPS": "· peak QPS",
  "· 模糊": "· fuzzy",
  "· 缺失": "· missing",
  "· 输出": "· output",
  "—— 这些名字必须是该渠道上游认识的。":
    "— these names must be ones that channel's upstream recognises.",
  "✓ 已写入": "✓ Wrote",
  "。两者都不会动你当前正在用的模型。Claude Code 没有目录文件，只有 5 个槽位，所以要在 Tier 列显式指定 —— 没指定的行不写。":
    ". Neither touches the model you are currently using. Claude Code has no catalog file, only 5 slots, so you must pick one explicitly in the Tier column — rows without one are not written.",
  "。这会改动你正在用的配置：": ". This changes the config you are actively using:",
  "一并应用内核连接设置（不勾则只导入模型链）":
    "Also apply the kernel connection settings (unticked imports model chains only)",
  "不支持": "no support for",
  "个": "",
  "个 CLI 支持": "CLIs support",
  "个文件": "file(s)",
  "个模型": "model(s)",
  "个模型正在失败": "model(s) failing",
  "个模型：精确": "models — exact",
  "个渠道上有": "channel(s) have",
  "个渠道未列出，完整明细见下方模型表。":
    "more channel(s) not listed; see the model table below for the full detail.",
  "个渠道没返回模型清单 —— 它们上的别名会被算成「上游无」，别据此删掉":
    "channel(s) returned no model list — aliases served by them will be counted as “not upstream”, so do not delete based on this",
  "个渠道给出清单，并集": "channel(s) returned a list, union of",
  "个请求进行中": "request(s) in flight",
  "主力": "the primary model is",
  "从上到下依次尝试。上面的层优先级更高，拖住左侧手柄可以换顺序（也可以聚焦手柄后按 ↑ ↓）。":
    "Tried top to bottom; higher rows win. Drag the handle on the left to reorder (or focus it and press ↑ ↓).",
  "从内核渠道聚合所有可用模型别名，":
    "Aggregates every model alias the kernel's channels expose and",
  "优先级": "priority",
  "优先级，所以所有档的顺序必须能折成一个全局顺序，折不出来上面会报冲突。":
    "priority, so every tier's order must collapse into one global order; if it cannot, a conflict is reported above.",
  "会覆盖同名的本机链：": "Will overwrite local chains with the same name:",
  "先装一处，再用列表里的「同步」推给其他 CLI。":
    "Install it in one place, then use “Sync” in the list to push it to the other CLIs.",
  "内核同时提供这几套规范的入口，协议转换在内核里完成。第三方工具直接填下面的地址和令牌即可，不必经过 CLI 接管。":
    "The kernel exposes an endpoint for each of these formats and does the protocol conversion itself. Third-party tools can use the URL and token below directly, without going through CLI takeover.",
  "内核未运行。左下角「启动内核」后再打开管理界面。":
    "The kernel is not running. Start it from the bottom left, then open the admin UI.",
  "内核连接：": "Kernel connection:",
  "内核里还没有任何渠道。先去「内核后台」把各家的渠道建好（客户端不替你发明凭据），再回来绑定。":
    "The kernel has no channels yet. Create them under “Kernel admin” first (this client will not invent credentials for you), then come back and bind them.",
  "写入前自动快照；不支持的目标已置灰":
    "Snapshots before writing; unsupported targets are greyed out",
  "写成下面这个别名，请求就会落到对应档。":
    "to the alias below and requests will land on the matching tier.",
  "凭证过期": "Stale credentials",
  "分钟": "min",
  "别名是 CLI 侧实际请求的模型名。队列从上到下依次尝试 —— 但内核只有":
    "The alias is the model name the CLI actually requests. Queues are tried top to bottom — but the kernel only has",
  "原始": "pristine",
  "地址已指向内核，但令牌与当前内核不匹配 —— 调用会 401，请重新写入。":
    "The URL points at the kernel, but the token belongs to a different one — calls will 401. Write it again.",
  "复原": "Reset",
  "字段来自 GET /admin/settings，新增项会自动出现。改任何一项都会写库并让内核约 2 秒后自动重启，在途请求会被打断，请避开使用中修改。":
    "Fields come from GET /admin/settings; new ones appear automatically. Changing any of them writes to the database and restarts the kernel about 2 seconds later, cutting off in-flight requests — avoid editing while it is in use.",
  "导出内核连接方式与模型链，换机器时导入即可。渠道和令牌属于内核数据，在「内核后台」用它自带的导入导出。":
    "Exports the kernel connection settings and model chains; import them on a new machine. Channels and tokens are kernel data — use the kernel admin's own import/export for those.",
  "将从": "Will remove from",
  "峰值": "peak",
  "已停用": "Disabled",
  "已收": "received",
  "已装：": "Installed:",
  "已选": "Selected",
  "应用后会把该渠道的优先级写成": "On apply, this channel's priority will be set to",
  "当前": "Currently",
  "恢复": "Restore",
  "打开管理窗口时会自动登录，正常情况下你看不到登录框。会话有效期 24 小时，过期或密码不对时会退回登录页 —— 那时用上面这个密码手动登一次。":
    "Opening the admin window signs you in automatically, so you normally never see the login box. Sessions last 24 hours; when one expires or the password is wrong you land back on the login page — use the password above to sign in manually that once.",
  "批量安装": "Install selected",
  "把「哪种活用哪家的哪个模型」配成一张表，应用后写进内核渠道：档位别名落成":
    "Lay out which provider and model handles which kind of work, then write it into the kernel's channels: tier aliases become",
  "把各 CLI 的配置指到内核。写入前自动快照，可在「快照历史」回滚；不确定时先在设置里打开「沙箱写入」，改动只落到 ~/.ccload-client/sandbox/，不碰真实配置。":
    "Points each CLI's config at the kernel. A snapshot is taken before writing and can be rolled back from “Snapshot history”. If you are unsure, turn on “sandbox writes” in Settings first — changes then land only in ~/.ccload-client/sandbox/ and never touch the real config.",
  "拿到就能直接调内核全部管理接口，别丢进聊天或云盘":
    "Anyone holding this can call every kernel admin endpoint — do not paste it into chat or cloud storage",
  "时，按下面的顺序换人（写进 settings.json 顶层的": ", switch in the order below (written to the top-level",
  "条新日志": "new log entries",
  "条：": "chain(s):",
  "来源客户端内核版本：": "Source client's kernel version:",
  "标准价": "List price",
  "次": "×",
  "次失败": "failures",
  "每家绑一个内核里已有的渠道（客户端不替你建渠道、不发明凭据），再填它在各档的":
    "Bind each provider to a channel that already exists in the kernel (this client will not create channels or invent credentials for you), then fill in its",
  "每次接管前会自动快照。标记「原始」的是首次接管前的用户配置。":
    "A snapshot is taken before every takeover. The one marked “pristine” is your config from before the first takeover.",
  对比: "Diff",
  "看这份快照和现在的配置差在哪": "See how this snapshot differs from the current config",
  快照对比: "Snapshot diff",
  和谁比: "Compare against",
  磁盘现状: "On disk now",
  上一份快照: "Previous snapshot",
  原始配置: "Original config",
  基准: "baseline",
  "红色 − 是「{base}」里有的，绿色 + 是这份快照里的。恢复后会变成绿色那一侧。":
    "Red − is in “{base}”; green + is in this snapshot. Restore will make the config look like the green side.",
  "对比中…": "Comparing…",
  "没有差异 —— 这份快照和「{base}」的内容完全一致。":
    "No differences — this snapshot matches “{base}” exactly.",
  会新建: "Will be created",
  会被删除: "Will be deleted",
  无差异: "identical",
  "差异太长，只显示了前面一部分；上面的 +N/−M 是完整计数。":
    "The diff is truncated; the +N/−M counts above are complete.",
  "沙箱已开：写入 ~/.ccload-client/sandbox，真实 CLI 配置不会被改。":
    "Sandbox is on: writes go to ~/.ccload-client/sandbox and your real CLI config is left alone.",
  "添加一行": "Add a row",
  "添加参数": "Add an argument",
  "清除": "Clear",
  "渠道上，所以得先从左下角「启动内核」。下面的下拉框在那之前是空的，与配置本身无关。":
    "channels, so the kernel has to be started from the bottom left first. Until then the dropdowns below are empty — that is not a problem with your config.",
  "的扩展清单失败，下面的列表里不含它：":
    "'s extension list could not be read, so it is missing from the list below:",
  "第": "Slot",
  "经": "via",
  "给文本模型装上「眼睛」：本客户端自带一个 MCP 服务器，把图片交给一个多模态模型描述，再把文字交给当前模型。已支持多模态的模型不需要。对话里只有 [Image 1] 没有路径时，把 image 设成 \"1\"，不要让用户把图另存一份。":
    "Gives text-only models eyes: this client ships an MCP server that hands the image to a multimodal model and passes the description back as text. Models that already handle images do not need it. When the chat only shows [Image 1] with no path, set image to \"1\" — do not ask the user to save a copy.",
  列出刚贴的图: "List pasted images",
  "编辑配置": "Edit config",
  "表示默认模型。留空则不写入。": "means the default model. Leave empty to skip.",
  "被标记时先问一句，不自动换模型（": "Ask before switching when a request is flagged (",
  "装在其他 CLI 上的同名扩展不受影响。":
    "Extensions with the same name installed in other CLIs are unaffected.",
  "视觉辅助 MCP": "Vision-assist MCP",

  // ---- 生图 MCP ----
  "生图 MCP": "Image MCP",
  "给每个 CLI 装上「手」：本客户端自带一个 MCP 服务器，把文字变成图，也能按指令改一张已有的图 —— 做游戏素材、图标、UI 草图都用它。生成的图写到磁盘，工具只把路径交回给模型；模型想看自己画的是什么，接着调视觉 MCP 的 describe_image 即可。":
    "Gives every CLI a pair of hands: this client ships an MCP server that turns text into an image, and edits an existing image on instruction — game assets, icons, UI mockups all go through it. The result is written to disk and only the path is handed back to the model; to see what it drew, it calls describe_image from the vision MCP.",
  用哪个模型生图: "Which model generates images",
  选择生图模型: "Pick an image model",
  先在上面选一个生图模型: "Pick an image model above first",
  "这里不做自动筛选：第三方目录里没有「能不能生图」这一项，猜错会把你真正能用的那个模型藏掉。选一个渠道里确实能出图的别名。":
    "No auto-filtering here: the third-party catalog has no “can it generate images” field, and guessing wrong would hide the very model you can use. Pick an alias that one of your channels can actually draw with.",
  走哪条路: "Which endpoint",
  已改成自动: "switched to auto",
  改写失败: "rewrite failed",
  "已装的 {n} 家钉死在一条端点上（配置里存的值，换新版客户端不会自己变）。改成「自动」后会按模型挑端点，上游说走错了就当场换一条重试。":
    "{n} installed CLI(s) are pinned to one endpoint — that value lives in their config files, so updating the client does not change it. Switch to Auto and the endpoint is picked per model, with an immediate retry on the other one if the upstream says it is the wrong endpoint.",
  "这 {n} 家改成自动": "Switch these {n} to auto",
  "自动（按模型挑，推荐）": "Auto (pick by model — recommended)",
  "对话生图（能生成也能改图）": "Chat (generate and edit)",
  "生图端点（只能生成）": "Images endpoint (generate only)",
  "按模型名挑端点：grok-imagine / gpt-image / dall-e 走生图端点，其余先走对话；上游要是回「这个模型不在这个端点上」就当场换另一条重试。改图永远走对话。":
    "Picks the endpoint from the model name: grok-imagine / gpt-image / dall-e go to the images endpoint, everything else tries chat first; if the upstream answers “this model is not available on this endpoint”, it retries on the other one right away. Editing always goes through chat.",
  '/v1/chat/completions + modalities:["image"]，尺寸按宽高比给（1:1@2k）。':
    '/v1/chat/completions + modalities:["image"]; size is an aspect ratio (1:1@2k).',
  "/v1/images/generations，尺寸按像素给（1024x1024）。这条路的请求体里没有放输入图的位置，所以 edit_image 用不了。":
    "/v1/images/generations; size is in pixels (1024x1024). This request body has no slot for an input image, so edit_image is unavailable.",
  图存到哪: "Where to save",
  "留空就是默认目录。工具回给模型的是绝对路径。":
    "Leave empty for the default directory. The tool hands the model an absolute path.",

  "角色靠 CLI 侧表达：在「扩展管理」里建一个同名 agent，把它的":
    "Roles live on the CLI side: create an agent with the same name under “Extensions” and set its",
  "该渠道合计": "This channel totals",
  "该渠道最近一次请求": "This channel's most recent request",
  "读取": "Reading",
  "调度图要把 provider 绑到内核里":
    "The dispatch graph binds each provider to a channel that already",
  "走沙箱（~/.ccload-client/sandbox），不改真实 CLI 配置":
    "Use the sandbox (~/.ccload-client/sandbox) and leave the real CLI config alone",
  "起": "from",
  "还有": "and",
  "还没有客户端令牌。先到「令牌」页新建一个，创建时会自动记下。":
    "No client token yet. Create one on the “Tokens” page — it is recorded automatically.",
  "还没有链。点「新建链」把第一个别名加进来。": "No chains yet. Hit “New chain” to add the first alias.",
  "这个名字，于是每次都跳进一个不存在的模型。唯一的改法是把上面的":
    ", so every switch lands on a model that does not exist. The only fix is to pin",
  "这几个 CLI 用的看图模型不一致（": "These CLIs are using different vision models (",
  "进各 CLI 的模型目录： Codex 每个别名一个": "them into each CLI's model catalog: one",
  "远端内核版本（": "The remote kernel version (",
  "选用）、 OpenCode 合并进":
    "per alias for Codex (selected with codex --profile), merged into",
  "那不是上面这条链干的。请求被 Claude Code 的安全分类器标记时，它会跳到":
    "That is not what the chain above does. When a request is flagged by Claude Code's safety classifier it switches to a",
  "配置编辑": "config editor",
  "重写": "Rewrite",
  "钉成你自己有的模型 —— 设了它之后，所有有 fallback 的分类都改跑这一个。另外把 Fable tier 填上，Claude Code 才认得出当前模型是 Fable 5。":
    "to a model you actually have — once it is set, every flagged category re-runs on that one. Also fill in the Fable tier so Claude Code can recognise the current model as Fable 5.",
  "顺位": "",
  "高级配置": "Advanced",
  "（比对改动前后）、": "(compare before and after), ",
  "（直接截当前屏幕，仅 macOS）。": "(capture the current screen, macOS only).",
  "（逐字抄下图上的文字，报错截图用它）、":
    "(transcribe the text in an image verbatim — use it for error screenshots), ",
  "）。上面的下拉只显示其中一个；要统一就选好模型后对每个 CLI 重新点一次「安装」。":
    "). The dropdown above shows only one of them; to unify, pick a model and hit Install again for each CLI.",
  "）。去重后最多 3 个，多余的 Claude Code 会忽略；填":
    "). At most 3 after deduplication — Claude Code ignores the rest. Use",
  "）不一致。 Admin API 的字段与校验规则随版本变化，建议切回本机内核或把远端升级到同版本，否则设置、渠道编辑等操作可能拿到意料外的响应。":
    ") does not match. Admin API fields and validation rules change between versions, so switch back to the local kernel or upgrade the remote one to match — otherwise settings and channel edits may get unexpected responses.",
  "）与壳体打包版本（": ") and the version bundled with this app (",
  "，队列顺序落成渠道优先级。之后 CLI 只认四个档位别名，换家、重试、冷却全部由内核原有的选择器完成。":
    ", and queue order becomes channel priority. After that the CLI only ever sees the four tier aliases; switching providers, retries and cooldowns are all handled by the kernel's existing selector.",
  "。全局顺序只排各档队列，不会改渠道绑定，也不会改渠道优先级。之后 CLI 只认档位别名，换家、重试、冷却全部由内核原有的选择器完成。":
    ". Global order only arranges each tier’s queue — it does not change channel bindings or channel priority. After that the CLI only ever sees the tier aliases; switching providers, retries and cooldowns are all handled by the kernel's existing selector.",

  // ---- 逐页核对补漏：模块级常量表要在使用处翻译 ----
  "MCP 服务器": "MCP servers",
  "技能目录（SKILL.md）": "Skill directories (SKILL.md)",
  "子代理定义（.md）": "Subagent definitions (.md)",
  "没有匹配「{q}」的{kind}": "No {kind} matching “{q}”",
  "还没有任何{kind}": "No {kind} yet",
  "删除{kind}？": "Delete {kind}?",
  渠道管理: "Channels",
  "上游渠道、Key、模型与降级链配置": "Upstream channels, keys, models and fallback chains",
  令牌管理: "Tokens",
  "CLI 接入用的 API 令牌与限额": "API tokens and limits for CLI access",
  请求日志: "Request logs",
  实时请求流与错误排查: "Live request stream and error triage",
  用量统计: "Usage stats",
  "成本、Token 用量与渠道健康度": "Cost, token usage and channel health",
  内核设置: "Kernel settings",
  内核运行参数与系统设置: "Kernel runtime parameters and system settings",

  "{n}/{total} 个 CLI 支持{kind}": "{n}/{total} CLIs support {kind}",
  "{list} 不支持": "{list} do not",
  "、": ", ",
  "清空搜索可看到全部 {n} 项": "Clear the search to see all {n}",
  // ---- 交互态才出现的文案（弹窗、校验、逐层体检）----
  "新建{kind}": "New {kind}",
  名称不能为空: "Name cannot be empty",
  "名称不能包含 / \\ .. 或以 . 开头": "Name cannot contain / \\ .. or start with .",
  "stdio 类型的 MCP 必须填 command": "An stdio MCP needs a command",
  "http 类型的 MCP 必须填 url": "An http MCP needs a url",
  "正文（markdown）不能为空": "Body (markdown) cannot be empty",
  必须填要执行的命令: "A command to run is required",
  "超时必须是非负整数（秒）": "Timeout must be a non-negative integer (seconds)",
  "没绑渠道 · 应用时这一层会被跳过": "No channel bound · this hop is skipped on apply",
  "渠道已禁用 · 这一层永远不会被选中": "Channel disabled · this hop can never be picked",
  上游清单里有这个模型: "Upstream lists this model",
  "渠道 #{id} 已不存在": "Channel #{id} no longer exists",
  "上游清单拉不到，无法校验：{err}": "Could not fetch the upstream list, so this cannot be verified: {err}",
  "上游清单里没有 {m} · 请求打到这一层会直接失败":
    "Upstream does not list {m} · a request reaching this hop fails outright",
  // ---- 无障碍属性（tooltip / aria-label），读屏和悬停都会读到 ----
  "选中 {name}": "Select {name}",
  "{name}，第 {n} 位，可拖动或按上下键调整": "{name}, position {n}, drag or use arrow keys to reorder",
  "第 {n} 层，拖动或按上下键调整顺序": "Hop {n}, drag or use arrow keys to reorder",
  "第 {n} 层的渠道": "Channel for hop {n}",
  "删除第 {n} 层": "Delete hop {n}",
  "删除第 {n} 个参数": "Delete argument {n}",
  "{cli} 没有 {kind} 的配置位置": "{cli} has nowhere to put a {kind}",
  "第 {n} 层 · 优先级 {p}": "Hop {n} · priority {p}",
  " · 渠道 {c}": " · channel {c}",
  // ---- 可搜索的上游模型选择器 ----
  "第 {n} 层的上游模型": "Upstream model for hop {n}",
  "先选右边的渠道，这里会列出它能服务的模型":
    "Pick a channel on the right first and its models will be listed here",
  "这个渠道还没配模型；点「校验上游模型」去问一次上游":
    "This channel has no models configured yet — hit “Verify upstream models” to ask upstream",
  展开候选: "Show suggestions",
  收起候选: "Hide suggestions",
  没有候选模型: "No suggestions",
  "没有匹配的模型，可直接用你输入的名字":
    "No match — what you typed will be used as-is",
  // ---- 模型候选下拉在其它页面的文案 ----
  "{p} 在 {tier} 档的模型": "{p} model for the {tier} tier",
  先在左边给它绑一个渠道: "Bind a channel on the left first",
  这个渠道还没配模型: "This channel has no models configured",
  "内核里还没有渠道，或渠道没配模型": "No channels yet, or none of them have models",
  // ---- 同步渠道模型清单 ----
  同步渠道模型清单: "Sync channel model lists",
  同步方式: "Sync mode",
  "覆盖：删掉上游已经没有的": "Replace: drop what upstream no longer has",
  "增量：只加新的，不删": "Merge: only add new ones, never delete",
  "同步 {n} 个渠道": "Sync {n} channel(s)",
  没有变化: "unchanged",
  "删掉 {n} 个": "dropped {n}",
  "新增 {n} 个": "added {n}",
  "现在共 {n} 个": "now {n} total",
  "上游改过模型清单（比如去掉了一批旧名字）之后用这个。内核默认的「增量」只增不删，退役的模型会一直留在候选里。":
    "Use this after the upstream changed its model list (e.g. dropped a batch of old names). The kernel's default “merge” only ever adds, so retired models stay in the candidate list forever.",
  "只把上游新增的模型加进来，渠道里已有的一个都不动。":
    "Only pulls in models the upstream added; nothing already in the channel is touched.",
  // ---- 渠道自报用量 ----
  探测自报用量: "Probe self-reported usage",
  "探测中…": "Probing…",
  "问非 OAuth 渠道的上游有没有自报用量的接口":
    "Ask non-OAuth channels whether their upstream reports usage",
  "这些渠道的上游都没有提供 /usage 接口。":
    "None of these upstreams expose a /usage endpoint.",
  上游自报: "self-reported",
  "这份数据是客户端直接问上游要的，内核并不知道它":
    "This came straight from the upstream — the kernel knows nothing about it",
  上游原文: "Upstream says",
  // ---- 系统注入：已装扩展的用法说明 ----
  已装的扩展: "Installed extensions",
  "MCP 的工具描述只说「它是干什么的」，不说「什么时候该想起它」。给一条你自己的判断标准，五家 CLI 一起生效 —— 不填的不会写进去。":
    "An MCP's tool description says what it does, never when to reach for it. Write your own rule of thumb once and it applies to all five CLIs — anything left blank is not written.",
  "什么时候用它，例如：改代码前先查调用链，比 grep 准":
    "When to use it, e.g. check the call graph before editing — more reliable than grep",
  "{n}/5 家": "{n}/5 CLIs",

  // ---- 会话救援 ----
  会话救援: "Session rescue",
  重新扫描: "Rescan",
  瘦身: "Slim down",
  分块总结: "Chunked summary",
  瘦身完成: "Slimmed down",
  分块总结完成: "Chunked summary done",
  备份: "Backup",
  正在运行: "Running",
  瘦身目标上下文: "Target context after slimming",
  总结用的模型: "Model used for summarising",
  两种救法的区别: "How the two rescues differ",
  "选一个内核里有的模型": "Pick a model the kernel actually serves",
  "空着 = 按模型链自动挑窗口够的": "Leave blank to pick a hop whose window fits",
  自动: "auto",
  "用 {model} 切成 {n} 块分别总结，保留最近 {k} 轮原文，摘要约 {s}（原 {before}）":
    "Used {model}; summarised in {n} chunks, kept the last {k} turns verbatim; summary is about {s} (was {before})",
  "对勾上的会话逐条分块总结。空着模型就按模型链挑窗口够的那一跳":
    "Chunk-summarises each checked session in turn. Leave the model blank to pick a hop whose window fits",
  "分块总结后追加一个原生压缩边界。空着模型就按模型链挑窗口够的那一跳":
    "Summarises in chunks, then appends a native compaction boundary. Leave the model blank to pick a hop whose window fits",
  "内核里没有可用模型": "No models available from the kernel",
  "没有找到任何会话。": "No sessions found.",
  "没有匹配的会话。换个关键词或清除筛选。":
    "No matching sessions. Try another keyword or clear the filters.",
  "搜名字、uuid 或路径": "Search name, uuid, or path",
  搜索会话: "Search sessions",
  按项目筛选: "Filter by project",
  "全部项目（{n}）": "All projects ({n})",
  排序: "Sort",
  最近改动: "Most recent",
  最早改动: "Oldest",
  峰值最大: "Largest peak",
  当前最大: "Largest current",
  文件最大: "Largest file",
  清除筛选: "Clear filters",
  "共 {n} 个会话": "{n} sessions",
  "{shown} / {total}": "{shown} / {total}",
  "先在上面选一个模型": "Pick a model above first",
  "{n} 分钟前": "{n} min ago",
  "{n} 小时前": "{n}h ago",
  "{n} 天前": "{n}d ago",
  "压缩过 {n} 次": "compacted {n}\u00d7",
  "会话撑过中转的上限之后会卡死：/compact 自己也要把整段对话发上去，所以它同样超限，从此只会报 400 too long。这里能把它弄回来。表格里的 token 数来自上游回报的用量，不是估算。":
    "Once a session outgrows your relay's ceiling it deadlocks: /compact has to send the whole conversation too, so it overflows just the same and every request comes back 400 too long. This page gets it back. The token counts below come from usage reported by the upstream \u2014 they are not estimates.",
  "目标要给天花板留余量 —— 压缩请求本身也要把整段对话发一遍，顶着上限做不成任何事。":
    "Leave headroom below the ceiling \u2014 a compaction request has to resend the whole conversation, so sitting right at the limit gets you nowhere.",
  "砍掉 {img} 张图、截短 {txt} 处文本，上下文 {before} → {after}，文件 {b1} → {b2}":
    "Stripped {img} images, truncated {txt} texts; context {before} \u2192 {after}, file {b1} \u2192 {b2}",
  "切成 {n} 块分别总结，保留最近 {k} 轮原文，摘要约 {s}（原 {before}）":
    "Summarised in {n} chunks, kept the last {k} turns verbatim; summary is about {s} (was {before})",
  "最后一次压缩之前的内容本来就不进上下文，救援只处理它之后的部分":
    "Anything before the last compaction never enters the context anyway; rescue only touches what came after it",
  "先退出那个 Claude Code 窗口 —— 进程里有内存态，现在改会被它盖回去":
    "Quit that Claude Code window first \u2014 the process holds in-memory state and would write over your changes",
  "这份记录里没有用量数据，拿不到真实上下文 —— 不敢下手":
    "This transcript has no usage data, so the real context size is unknown \u2014 refusing to touch it",
  "砍图 + 截长工具结果。本地完成，不花 token，但信息真的丢了":
    "Strips images and truncates long tool results. Local, costs no tokens \u2014 but what it cuts is genuinely gone",
  "分块总结后追加一个原生压缩边界。旧内容一个字节不动，花 token 但保信息":
    "Summarises in chunks, then appends a native compaction boundary. Costs tokens, keeps the information, and leaves the old content untouched",
  "瘦身 —— 把图片换成占位符、超长工具结果留首尾。纯本地、秒级、不花 token，但被砍掉的内容是真的没了。急着把会话弄活用它。":
    "Slim down \u2014 replaces images with placeholders and keeps only the head and tail of oversized tool results. Purely local, takes seconds, costs no tokens, but whatever it cuts is gone for good. Use it when you just need the session breathing again.",
  "分块总结 —— 把对话切成小段分别总结再合并，然后追加一个和 Claude Code 自己压缩时一模一样的边界。任何一次请求都远低于天花板，所以不会像 /compact 那样自己也超限。花 token，但信息以摘要形式留下来了。":
    "Chunked summary \u2014 splits the conversation into small pieces, summarises each, merges them, then appends exactly the same boundary Claude Code writes when it compacts on its own. Every single request stays far below the ceiling, so unlike /compact it cannot overflow. It costs tokens, but the information survives as a summary.",
  "两种都会先把原文件另存一份 .bak，而且都不删记录 —— transcript 是靠 uuid 串起来的链表，删行会让恢复出来的会话缺胳膊少腿。":
    "Both save the original to a .bak first, and neither deletes any record \u2014 a transcript is a linked list held together by uuids, so dropping lines leaves the resumed session full of holes.",

  全选当前列表: "Select all in this list",
  一键救援: "Rescue selected",
  "一键救援（{n}）": "Rescue selected ({n})",
  "先勾要救的会话": "Check the sessions you want to rescue first",
  "对勾上的会话逐条分块总结。花 token，但信息以摘要留下来":
    "Chunk-summarises each checked session in turn. Costs tokens, keeps the information as a summary",
  "一键救援 = 对勾上的会话逐条分块总结，活着的和没有用量的会跳过。":
    "Rescue selected = chunk-summarise each checked session in turn. Live sessions and ones with no usage data are skipped.",
  "救援中 {i}/{n}：{name}": "Rescuing {i}/{n}: {name}",
  "一键救援完成：成功 {ok}，失败 {fail}": "Rescue finished: {ok} ok, {fail} failed",
  "清太久没碰的 Claude Code 会话。删掉的文件不可恢复，救援留下的备份会一起清。正在运行的会话不会被动。":
    "Clear Claude Code sessions you have not touched in a while. Deleted files cannot be recovered, and any rescue backups go with them. Sessions that are currently running are left alone.",
  按最后改动筛选: "Filter by last change",
  不限时间: "Any time",
  "{n} 天前及更早": "{n} days ago or older",
  "删除选中（{n} · {size}）": "Delete selected ({n} \u00b7 {size})",
  删除选中: "Delete selected",
  "已删除 {n} 个会话，腾出 {size}": "Deleted {n} sessions, freed {size}",
  "跳过 {n} 个正在运行的": "Skipped {n} that are still running",
  "确认删除 {n} 个会话？": "Delete {n} sessions?",
  "将永久删除 {n} 个文件（约 {size}），救援留下的备份一并清掉。没有回收站。":
    "Permanently delete {n} files (about {size}). Rescue backups go with them. There is no recycle bin.",
  "… 还有 {n} 个": "\u2026 and {n} more",

  // ---- 破禁 / 会话预设 ----
  新建预设: "New preset",
  隐藏内置: "Hide built-ins",
  显示内置: "Show built-ins",
  "隐藏内置预设（只是不显示，删不掉）": "Hide the built-in presets (view only — they can't be deleted)",
  "内置预设已隐藏，点击显示": "Built-in presets are hidden; click to show",
  "内置预设已隐藏，且你还没建自己的。点右上「显示内置」，或「新建预设」。":
    "Built-in presets are hidden and you haven't created your own. Click Show built-ins, or New preset.",
  "还没有预设。点右上「新建预设」写第一份。":
    "No presets yet. Click New preset in the top right to write your first.",
  编辑预设: "Edit preset",
  工作目录: "Working directory",
  选目录: "Choose folder",
  内置: "Built-in",
  "{n} 轮": "{n} turns",
  "开一个新会话": "Start a new session",
  "复制一份": "Duplicate",
  "先选一个工作目录": "Pick a working directory first",
  "Claude Code 打开的那个仓库路径": "The repo path Claude Code should open",
  "写在末尾的第一条任务（可选）": "First task to append (optional)",
  "会作为最后一条用户消息追加。留空就只写入预设对白。":
    "Appended as the last user message. Leave empty to write only the preset dialogue.",
  "写完后拉起终端跑 resume": "Open a terminal and run each CLI's resume command when done",
  锁定在这个目录: "Keep the session inside this directory",
  "会话不会被预先解除文件访问的关卡：越出上面这个目录的读写要你当场点头。Codex 还会额外钉死工作根、只给工作区写权限。":
    "The session starts with its normal permission prompts intact, so reads and writes outside the directory above still have to be approved by you. Codex additionally gets its working root pinned and write access limited to the workspace.",
  "关掉之后写出的会话开局就没有任何文件访问关卡，读写整块磁盘都不会问你一声。而 CLI 的项目根在目录处于某个 git 仓库子目录时会落到仓库根上 —— 选了 repo/src，实际摸得到的是整个 repo。":
    "With this off, the session starts with every file-access check already waived — it can read and write anywhere on the disk without asking. On top of that, a CLI resolves its project root to the enclosing git repository, so picking repo/src actually hands it the whole repo.",
  "写给哪些 CLI": "Which CLIs to write",
  "至少勾选一个 CLI": "Tick at least one CLI",
  "要打开的那个仓库路径": "The repo path to open",
  "已经在终端里拉起。先退出正在跑的同目录窗口，免得两份抢同一份文件。":
    "Launched in a terminal. Quit any live window on the same repo first, or the two will fight over the file.",
  已经写好: "Written",
  "已经在终端里拉起 Claude Code。先退出正在跑的同目录窗口，免得两份抢同一份文件。":
    "Claude Code is launching in a terminal. Quit any live window on the same repo first, or the two will fight over the file.",
  "文件写好了，但终端没拉起来：{err}。把上面这条命令自己跑。":
    "The file is written, but the terminal didn't launch: {err}. Run the command above yourself.",
  "把上面这条命令丢进终端。": "Paste the command above into a terminal.",
  "点一下按勾选的 CLI 各写一份已经带好背景的新会话，然后用那一家自己的 resume 接着干。内置几份公开的破禁预设；你也可以自己写一轮对白。拦不拦得住取决于对面那家模型，壳体只负责把历史写进文件。":
    "One click writes a primed session for each CLI you tick, then you resume with that CLI's own command. A few public unlock presets are built in; you can also write your own dialogue. Whether the model plays along depends on the model \u2014 the shell only writes the history.",
  "{title}（副本）": "{title} (copy)",
  标题: "Title",
  摘要: "Summary",
  用户: "User",
  助手: "Assistant",
  删这轮: "Remove turn",
  加一轮: "Add a turn",
  已删除: "Deleted",

  // ---- 快照历史：按 CLI 分组 ----
  "{n} 份快照": "{n} snapshot(s)",
  "{n} 个文件": "{n} file(s)",
  接管写入: "Takeover",
  手工编辑配置: "Manual config edit",
  "改 MCP": "MCP change",
  "改 Hook": "Hook change",
  "首次接管前的用户配置 —— 这一条永远不会被新快照挤掉":
    "Your config from before the first takeover — this one is never evicted by newer snapshots",

  // ---- 系统注入：勾选不等于写入 ----
  "{n} 个，可选填用法说明": "{n} installed — usage notes optional",
  "已写 {n} 条": "{n} written",
  "上面的改动还没写进文件。已注入的 {n} 家里存的还是旧内容。":
    "The changes above are not written yet — the {n} injected CLI(s) still hold the old content.",
  "更新这 {n} 家": "Update those {n}",
  写到哪几家: "Write to which CLIs",
  "上面勾的内容按下这一行的「写入 / 更新」才落到文件里":
    "What you ticked above only reaches the file when you press Write / Update on one of these rows",
  "{server} 还没装到任何 CLI —— 只写说明不装服务器，等于教模型去调一个不存在的工具。先到「模型导入」页最下面的「{panel}」装一下。":
    "{server} is not installed on any CLI — writing the guidance without the server teaches the model to call a tool that isn't there. Install it from “{panel}” at the bottom of the Model import page first.",
});
