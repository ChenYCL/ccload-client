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
  "CLI 接管": "CLI takeover",
  调度图: "Dispatch graph",
  模型链: "Model chain",
  模型导入: "Model import",
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
  "内核未运行，没有可展示的数据。": "The kernel isn't running, so there's nothing to show.",
  "从左下角「启动内核」开始。": "Start it from the bottom-left corner.",
  请求量与成功率: "Request volume and success rate",
  渠道健康: "Channel health",
  模型消耗: "Model spend",
  模型明细: "Model breakdown",
  "该时间段没有任何请求": "No requests in this period",
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
  "还没有调用记录。装上「模型导入」页里的视觉辅助 MCP 之后，文本模型每次看图都会记一笔。":
    "No calls recorded yet. Install the vision-assist MCP from the “Model import” page and every image a text-only model looks at gets logged here.",
  "只统计本客户端自带的 MCP 服务器（ccload-vision）。扩展管理里装的第三方 MCP 由 CLI 直接拉起，不经过内核也不经过客户端，无法计入。":
    "Covers only this app's own MCP server (ccload-vision). Third-party MCP servers installed under Extensions are spawned directly by the CLI — they pass through neither the kernel nor this client, so they cannot be counted.",
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
});
