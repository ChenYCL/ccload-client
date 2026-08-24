export type KernelMode = "managed" | "remote";

export type KernelStatus =
  | { state: "stopped" }
  | { state: "starting" }
  | { state: "running"; base_url: string; version: string }
  | { state: "failed"; message: string };

export type KernelConfig = {
  mode: KernelMode;
  port: number;
  remote_url: string | null;
  admin_password: string;
  data_dir: string | null;
};

export type AppSettings = {
  kernel: KernelConfig;
  sandbox_cli_writes: boolean;
  client_api_token: string | null;
};

export type CliTarget =
  | "claude-code"
  | "codex"
  | "gemini-cli"
  | "grok-build"
  | "opencode";

export type TakeoverPreview = {
  target: CliTarget;
  label: string;
  path: string;
  exists: boolean;
  current_endpoint: string | null;
  next_endpoint: string;
  already_active: boolean;
  /** Endpoint points at the kernel but the stored token is from another one. */
  token_stale: boolean;
};

export type TakeoverResult = {
  target: CliTarget;
  written: string[];
  backup_id: string;
  restart_required: boolean;
};

export type BackupFile = {
  rel: string;
  stored: string | null;
  existed: boolean;
};

export type BackupEntry = {
  id: string;
  target: CliTarget;
  created_at: number;
  reason: string;
  pristine: boolean;
  files: BackupFile[];
};

export type TakeoverOptions = {
  anthropic_model?: string;
  sonnet_model?: string;
  opus_model?: string;
  haiku_model?: string;
  /**
   * Claude Code 顶层 `fallbackModel` 数组：主力**过载/不可用**时按顺序换人。
   * 去重后最多 3 个，`"default"` 展开成默认模型。留空则不写入。
   *
   * 注意它管不到「被安全分类器标记后跳 claude-opus-4-8」那种换模型 ——
   * 那条路只认 `ANTHROPIC_DEFAULT_OPUS_MODEL`（也就是上面的 opus_model）。
   */
  fallback_models?: string[];
  /** `switchModelsOnFlag`：false = 被标记时先问一句，不自动换。 */
  switch_models_on_flag?: boolean;
  extra_env?: Record<string, string>;
  codex_model?: string;
  codex_reasoning_effort?: string;
  codex_context_window?: number;
};

export type ConfigFormat = "json" | "toml";

export type ConfigFileView = {
  rel: string;
  path: string;
  format: ConfigFormat;
  exists: boolean;
  body: string;
};

export type EnvKeyInfo = {
  key: string;
  description: string;
  /** Suggested value when the key is absent; "" means no opinion. */
  default: string;
  /** What this machine currently has configured, if anything. */
  current: string | null;
};

export type FallbackHop = {
  upstream: string;
  channel_id: number | null;
  channel_name: string | null;
};

export type FallbackChain = {
  alias: string;
  hops: FallbackHop[];
};

export type Envelope<T> = {
  success: boolean;
  data: T;
  error: string;
  count: number;
};

export type ImportEntry = {
  alias: string;
  contextWindow?: number | null;
  tier?: string | null;
};

export type ImportResult = {
  written: string[];
  backup_id: string;
  /** 这个 CLI 放不下的别名（Claude Code 里没选 Tier 槽位的行）。 */
  skipped: string[];
};

export type GraphProvider = {
  id: string;
  label: string;
  enabled: boolean;
  channelId: number | null;
  /** 档位 id → 该家在该档的真实上游模型名。 */
  models: Record<string, string>;
};

export type GraphTier = {
  id: string;
  label: string;
  /** CLI 侧实际请求的模型名。 */
  alias: string;
  /** 有序候选队列，越靠前越先用。 */
  providers: string[];
};

export type GraphRole = { id: string; label: string; tier: string };

export type GraphDoc = {
  id: string;
  label: string;
  enabled: boolean;
  providers: GraphProvider[];
  tiers: GraphTier[];
  roles: GraphRole[];
  /** 用户钉住的全局顺序。空则从各档队列做拓扑排序。 */
  providerOrder?: string[];
};

/** 校验结果。ok=false 时禁止应用，后端也会再拦一次。 */
export type GraphValidation = {
  ok: boolean;
  problems: string[];
  globalOrder: string[];
  priorities: Record<string, number>;
};

export type Page =
  | "dashboard"
  | "logs"
  | "usage"
  | "web-admin"
  | "cli"
  | "fallback"
  | "forced-route"
  | "models"
  | "inject"
  | "sessions"
  | "unlock"
  | "extensions"
  | "graph"
  | "settings";

/* ---------------------------------------------------------------------------
   强制路由：CLI 请求某个模型名，就把它钉到你选的渠道 + 上游模型。

   和「模型链」（FallbackChain）是两个概念：模型链讲优雅降级、要校验上游；
   强制路由是「我说发去哪就发去哪」，不校验上游，照发。底层落到渠道上的写法两者
   一样（都是往渠道 upsert 一条 {model: 别名, redirect_model: 上游} 并设优先级）。

   一条路由 = 一个请求别名 → 一组目标。第一个目标优先级最高，命中即用；后面的是
   备用落点。绝大多数场景只配一个目标。
--------------------------------------------------------------------------- */

export type ForcedTarget = {
  /** 绑定的渠道。null 时应用会跳过这个目标 —— 没绑渠道就没有落点。 */
  channel_id: number | null;
  /** 渠道名，存一份让列表页不必再查渠道表。 */
  channel_name: string | null;
  /** 发给上游的真实模型名。不校验上游清单，手填任意名字照样发。 */
  model: string;
};

export type ForcedRoute = {
  /** CLI 请求时写的模型名（例如 claude-fable-5）。命中它就强制走 targets。 */
  from: string;
  /** 落点，按顺序：第一个优先级最高。 */
  targets: ForcedTarget[];
};

/* ---------------------------------------------------------------------------
   系统注入：把受管的说明块写进各 CLI 的全局指令文件
   （CLAUDE.md / AGENTS.md / GEMINI.md）。

   装了 ccload-vision 不等于模型会用它 —— 工具描述只是一句话，写进系统提示的
   规则才管用。块用 `<!-- ccload:begin -->` 标记，块外内容一个字节都不动。
--------------------------------------------------------------------------- */

export type InjectSpec = {
  /** 视觉工具用法。文本模型不知道自己「看不见」，得明说。 */
  vision: boolean;
  /** 生图工具用法。模型不会主动想到「这张图我自己就能画」，也得明说。 */
  image: boolean;
  /** 给已装的第三方扩展写的「什么时候用它」。没写说明的条目会被后端丢掉。 */
  tools: ToolNote[];
  /** 用户自己的规则，原样写进块里。 */
  custom: string;
};

/**
 * 一条第三方扩展的用法说明。
 *
 * MCP 的工具描述只有一句「这个工具是干什么的」，不是「什么时候该想起它」。
 * 装了 codegraph 不等于模型会在改代码前先查调用链 —— 那句话得有人写下来，
 * 而且写一次要能推给五家 CLI。
 */
export type ToolNote = {
  /** 扩展 id（MCP 服务器名 / skill 目录名 / agent 文件名）。 */
  name: string;
  /** 什么时候用它。留空 = 不写进去。 */
  note: string;
};

export type InjectState = {
  target: CliTarget;
  label: string;
  /** 全局指令文件的绝对路径 —— 用户得知道我们在改哪个文件。 */
  path: string;
  exists: boolean;
  injected: boolean;
  /** 块内现有内容，用来回显。 */
  block: string | null;
  /**
   * 块里解析出来的勾选状态。**由后端解析**（`system_inject::parse_block`）——
   * 前端按文本比对来猜哪几段开着，会在后端改一个字的那天全线失灵。
   */
  spec: InjectSpec | null;
  /** 装的是旧版本的措辞：内容仍然生效，按「更新」就会用当前措辞重写。 */
  outdated: boolean;
  /** 整个文件的字符数。Grok 到 10000 会截断。 */
  chars: number;
};

export type InjectOutcome = {
  target: CliTarget;
  label: string;
  ok: boolean;
  path: string | null;
  error: string | null;
};

/* ---------------------------------------------------------------------------
   视觉辅助 MCP。

   模型这一项以前只活在 ModelsPage 的 useState 里，切走再回来就回到占位符，
   看起来像「选了没保存上」。真值一直写在各 CLI 的配置文件里，后端读回来。
--------------------------------------------------------------------------- */

export type VisionTargetState = {
  target: CliTarget;
  label: string;
  installed: boolean;
  /** 已装的话，它现在用哪个模型看图。 */
  model: string | null;
  /** 装了，但里面存的内核地址/令牌已经过期 —— 每次看图都会 401。 */
  stale: boolean;
};

/* ---------------------------------------------------------------------------
   生图 MCP。

   走哪条路是**能力差别**不是口味差别：`images` 是标准 OpenAI 生图端点，
   `chat` 是 `/v1/chat/completions` + `modalities:["image"]`。改图只有 chat
   能做 —— images 的请求体里没有放输入图的位置。

   默认是 `auto`：按模型名挑一条先试，上游要是回「这个模型不在这个端点上」
   就换另一条。钉死一条的代价是装的时候选错一次、之后每次生图都失败，而错误
   信息说的是上游的话，没人会想到要回客户端改一个下拉框。
--------------------------------------------------------------------------- */

export type ImageApi = "auto" | "chat" | "images";

export type ImageTargetState = {
  target: CliTarget;
  label: string;
  installed: boolean;
  /** 已装的话，它现在用哪个模型生图。 */
  model: string | null;
  /** 已装的话，它走的是哪条路。 */
  api: ImageApi | null;
  /** 已装的话，图往哪写。null = 后端默认目录。 */
  out_dir: string | null;
  /** 装了，但里面存的内核地址/令牌已经过期 —— 每次生图都会 401。 */
  stale: boolean;
};

/* ---------------------------------------------------------------------------
   自带 MCP 工具的调用统计。

   口径只覆盖客户端自带的两个服务器（`ccload-vision` / `ccload-image`）：
   别家 MCP 服务器是独立进程，既不经内核也不经我们，客户端没有任何位置能看见
   它们的调用。
--------------------------------------------------------------------------- */

export type McpToolStat = {
  tool: string;
  calls: number;
  failed: number;
  /** 毫秒，只统计成功的调用。 */
  avg_ms: number;
  max_ms: number;
  /** 累计耗时毫秒，含失败。 */
  total_ms: number;
  /** unix 秒 */
  last_at: number;
};

export type McpUsage = {
  tools: McpToolStat[];
  calls: number;
  failed: number;
  total_ms: number;
  /** unix 秒，流水里最早一条；0 表示还没有数据。 */
  since: number;
  /** 老记录被丢过，统计口径不是「有史以来」。 */
  truncated: boolean;
};

/* ---------------------------------------------------------------------------
   订阅额度。内核把每个 OAuth 渠道的上游额度采样存进凭证，`GET /admin/channels`
   原样带出来；`POST /admin/channels/:id/oauth-usage` 触发一次重新采样。

   字段对照 vendor/ccLoad/internal/app/admin_oauth_usage.go 的
   `oauthUsageSummary` / `oauthUsageWindow`。
--------------------------------------------------------------------------- */

/** 一个上游额度窗口。身份是 `limit_name|kind`，**不是**窗口时长。 */
export type OAuthUsageWindow = {
  limit_name: string;
  kind: string;
  /** 0–100 */
  used_percent: number;
  remaining_percent: number;
  /** 窗口时长秒；5 小时窗 = 18000，周窗 = 604800 */
  limit_window_seconds: number;
  /** unix 秒，下次重置 */
  reset_at: number;
  /** 该窗口累计的标准成本（微美元），内核按日志实时累加 */
  standard_cost_microusd?: number;
};

export type OAuthUsageSummary = {
  provider: string;
  plan_type?: string;
  subscription_tier?: string;
  entitlement_status?: string;
  windows: OAuthUsageWindow[];
  warnings?: string[];
};

/** `GET /admin/channels` 里我们用得上的字段。 */
export type UsageChannel = {
  id: number;
  name: string;
  enabled?: boolean;
  auth_type?: string;
  oauth_usage?: OAuthUsageSummary | null;
};

/* ---------------------------------------------------------------------------
   扩展管理：MCP / Skill / Agent / Hook 在 5 个 CLI 之间的统一管理。

   下面每一个字符串取值都逐个对照过 src-tauri/src/services/cli_extensions.rs
   的 `#[serde(...)]` 属性，没有靠猜。三个容易踩的坑写在这里：
     · `ExtensionKind` 是 `rename_all = "kebab-case"`，但四个变体都是单词，
       结果与 lowercase 相同 —— 以后加 `SlashCommand` 这种多词变体会变成
       `slash-command`，别按 lowercase 的印象去扩展
     · `HookEvent` **没有** rename_all，序列化就是变体名原样（PascalCase）
     · `McpTransport` 只有 `stdio` / `http` 两个变体，**没有 sse**
--------------------------------------------------------------------------- */

export type ExtensionKind = "mcp" | "skill" | "agent" | "hook";

/** 后端 enum 只有 Stdio / Http；SSE 类型的服务器一律按 http 填 url。 */
export type McpTransport = "stdio" | "http";

/** 规范化事件名。各 CLI 的原生名（Gemini 的 BeforeTool 等）由后端翻译。 */
export type HookEvent =
  | "PreToolUse"
  | "PostToolUse"
  | "UserPromptSubmit"
  | "SessionStart"
  | "SessionEnd"
  | "Stop"
  | "SubagentStop"
  | "PreCompact"
  | "Notification";

/** 支持矩阵的一行。`supported=false` 的组合前端要提前置灰。 */
export type ExtensionSupport = {
  target: CliTarget;
  label: string;
  kind: ExtensionKind;
  supported: boolean;
  /** 支持时写到哪（相对 home）；不支持为 null。 */
  path: string | null;
};

export type ExtensionItem = {
  target: CliTarget;
  kind: ExtensionKind;
  /** MCP 服务器名 / skill 目录名 / agent 文件名 / hook 的 `事件-哈希`。 */
  id: string;
  label: string;
  description: string | null;
  /** 该条目所在文件或目录的绝对路径。 */
  source: string;
  enabled: boolean;
  /** 配置文件里的原始片段，原样展示即可。 */
  detail: unknown;
};

/** sync 的逐目标结果 —— 一个目标失败不影响其他目标。 */
export type SyncOutcome = {
  target: CliTarget;
  label: string;
  ok: boolean;
  written: string[];
  error: string | null;
};

/**
 * 一个扩展的规范化描述。Rust 侧是 `rename_all = "camelCase"` + `default`，
 * 所以字段名用 camelCase（注意 `hookCommand`，不是 hook_command），且可以只
 * 传该 kind 用得到的字段，缺的走 Default。
 */
export type ExtensionSpec = {
  id: string;
  description?: string | null;

  /* ---- MCP ---- */
  transport?: McpTransport | null;
  command?: string | null;
  args?: string[];
  env?: Record<string, string>;
  url?: string | null;
  headers?: Record<string, string>;
  /** 只有显式 false 才会写进配置（OpenCode 例外，它总写 enabled）。 */
  enabled?: boolean | null;

  /* ---- Skill / Agent ---- */
  /** 完整 markdown。带 `---` frontmatter 就原样写入，否则后端合成最小一份。 */
  body?: string | null;

  /* ---- Hook ---- */
  event?: HookEvent | null;
  /** 匹配哪些工具，如 `Bash|Write`；留空等于 `*`。 */
  matcher?: string | null;
  hookCommand?: string | null;
  timeout?: number | null;
};

/* ---------------------------------------------------------------------------
   Admin 统计 / 日志 API。

   字段名逐个对照过内核源码（vendor/ccLoad/internal/model/stats.go、log.go）并用
   真实响应验证过（2026-08-18）。两个容易踩的坑写在这里，别再猜：
     · stats 行里没有 `cost`，只有 `total_cost`（标准）和 `effective_cost`（倍率后）
     · 信封的 `count` 对 /channels 恒为 0，但对 /logs 是真实总数
--------------------------------------------------------------------------- */

/** GET /admin/metrics?range=&bucket_min= —— 已按桶补齐，空桶也在数组里（success/error 为 0）。 */
export type MetricPoint = {
  /** RFC3339，桶的起始时刻 */
  ts: string;
  success: number;
  error: number;
  avg_first_byte_time_seconds?: number;
  avg_duration_seconds?: number;
  total_cost?: number;
  effective_cost?: number;
  input_tokens?: number;
  output_tokens?: number;
  cache_read_tokens?: number;
  cache_creation_tokens?: number;
};

/** stats 响应里 channel_health 的一个点。固定 48 个点（本日=最近 4 小时 × 5 分钟桶）。 */
export type HealthPoint = {
  ts: string;
  /** 成功率 0–1；**-1 表示这一桶没有样本**，不是 0% */
  rate: number;
  success: number;
  error: number;
  rate_limited: number;
  avg_duration?: number;
};

/** stats 数组的一行：粒度是「渠道 × 模型」，同一模型会出现多次。 */
export type StatsEntry = {
  channel_id?: number;
  channel_name?: string;
  model?: string;
  success?: number;
  error?: number;
  total?: number;
  avg_first_byte_time_seconds?: number;
  avg_duration_seconds?: number;
  /* ---- 下面这三个是**渠道口径**，不是本行的渠道 × 模型口径 ----------------
     内核 storage/sql/metrics.go:fillStatsLastRequests 只有在查询带了 model /
     model_like 筛选时才按「渠道 × 模型」填，否则把该渠道最近一次请求的结果
     复制给这个渠道的每一行。实测同一渠道下 5 个模型的 last_request_* 完全相同。
     所以不带 model 筛选时，别把它当成「这个模型最后一次失败的原因」展示。 */
  last_request_at?: number;
  last_request_status?: number;
  last_request_message?: string;
  total_input_tokens?: number;
  total_output_tokens?: number;
  total_cache_read_input_tokens?: number;
  total_cache_creation_input_tokens?: number;
  total_cost?: number;
  effective_cost?: number;
};

export type RpmStats = {
  peak_rpm: number;
  peak_qps: number;
  avg_rpm: number;
  avg_qps: number;
  /** 仅本日有效 */
  recent_rpm: number;
  recent_qps: number;
};

export type StatsResponse = {
  stats: StatsEntry[];
  /** key 是 channel_id 的字符串形式 */
  channel_health?: Record<string, HealthPoint[]>;
  rpm_stats?: RpmStats;
  duration_seconds?: number;
  is_today?: boolean;
};

/** GET /admin/logs —— 按 time 倒序（最新在前）。 */
export type LogEntry = {
  id: number;
  /** unix 秒 */
  time: number;
  model?: string;
  actual_model?: string;
  channel_id?: number;
  channel_name?: string;
  status_code?: number;
  message?: string;
  is_streaming?: boolean;
  /** 秒（浮点） */
  duration?: number;
  first_byte_time?: number;
  input_tokens?: number;
  output_tokens?: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
  reasoning_tokens?: number;
  cost?: number;
  /**
   * 渠道倍率快照。注意 `cost` 是**标准成本**、没乘倍率（内核
   * model/log.go:98），实付 = cost × cost_multiplier；总览的
   * `effective_cost` 就是内核替我们乘好的那一个。
   */
  cost_multiplier?: number;
  auth_token_id?: number;
  thinking_effort?: string;
  client_protocol?: string;
  upstream_protocol?: string;
  base_url?: string;
  api_key_used?: string;
  auth_token_description?: string;
  client_ip?: string;
  log_source?: string;
};

/**
 * GET /admin/active-requests —— 进行中的请求，读的是内核内存态（`activeRequests.List()`），
 * 不查数据库。这是整个 Admin API 里唯一真正「实时」的东西：日志表只有请求**结束**后
 * 才落库，所以正在跑的长流式请求在 /admin/logs 里是看不见的。
 *
 * 字段对照 vendor/ccLoad/internal/app/active_requests.go 的 ActiveRequest，
 * 并用真实响应验证过（2026-08-18）。
 */
export type ActiveRequest = {
  id: number;
  model?: string;
  client_ip?: string;
  /** unix **毫秒** —— 注意和 LogEntry.time 的「秒」不是一个单位 */
  start_time: number;
  is_streaming?: boolean;
  channel_id?: number;
  channel_name?: string;
  client_protocol?: string;
  upstream_protocol?: string;
  /** 已脱敏 */
  api_key_used?: string;
  token_id?: number;
  base_url?: string;
  /** 上游已回传字节数的快照，流式请求靠它判断「在动还是卡住」 */
  bytes_received?: number;
  /** 秒；流式请求才有 */
  client_first_byte_time?: number;
  cost_multiplier?: number;
  upstream_websocket?: boolean;
  debug_log_available?: boolean;
  thinking_effort?: string;
  /** 上游阶段文案，实测见过 "receiving" */
  upstream_status?: string;
};

/** GET /admin/logs/bootstrap —— 筛选下拉的真实取值来源，省得前端自己从日志里凑。 */
export type LogsBootstrap = {
  models?: string[];
  channels?: { id: number; name: string }[];
  status_codes?: number[];
};

/** 统计范围。内核 ParsePaginationParams 支持的取值子集。 */
export type StatsRange = "today" | "yesterday" | "this_week";

export type SettingItem = {
  key: string;
  value: string;
  value_type: "string" | "int" | "bool" | "duration" | string;
  description: string;
  default_value: string;
  updated_at: number;
  editable: boolean;
};

/** 导入前的预览，让用户看清会覆盖什么再决定。 */
export type ImportPreview = {
  format_version: number;
  client_kernel_version: string;
  includes_secrets: boolean;
  kernel_mode: string;
  kernel_endpoint: string;
  chain_aliases: string[];
  overwritten_aliases: string[];
};

/* ---------------------------------------------------------------------------
   渠道模型清单刷新。`POST /admin/channels/models/refresh-batch`

   上游改了模型清单之后，渠道里存的还是**旧的那份** —— 内核不会自己去发现
   「某个模型消失了」。`replace` 模式才会把已经不存在的条目删掉；默认的
   `merge` 只增不删，所以退役的模型会一直挂在候选里，点了就失败。

   字段对照 vendor/ccLoad/internal/app/admin_models.go 的
   `BatchRefreshModelsRequest` / `BatchRefreshModelsItem`。
--------------------------------------------------------------------------- */

export type RefreshMode = "merge" | "replace";

export type RefreshItem = {
  channel_id: number;
  channel_name?: string;
  status: "updated" | "unchanged" | "failed";
  error?: string;
  /** 这次从上游拉到几个 */
  fetched: number;
  /** merge 模式：新增几个 */
  added?: number;
  /** replace 模式：删掉几个 */
  removed?: number;
  /** 刷新后渠道一共有几个模型 */
  total: number;
};

export type RefreshResult = {
  mode: RefreshMode;
  total: number;
  updated: number;
  unchanged: number;
  failed: number;
  results: RefreshItem[];
};

/* ---------------------------------------------------------------------------
   渠道自报用量。

   内核只给 5 家 OAuth 渠道采样额度；别的渠道（api_key 型）没有任何位置拿得到
   订阅用量。但有些上游自己知道 —— 比如 cursor2Oauth 手里有 Cursor 的凭证。
   所以定一个自报契约：上游在根地址提供 `GET /usage`，`windows[]` 形状对齐
   内核的 `oauth_usage.windows[]`，这一页就能把它并排画出来。
--------------------------------------------------------------------------- */

export type SelfReportedUsage = {
  channel_id: number;
  channel_name: string;
  provider: string;
  plan_type: string;
  windows: OAuthUsageWindow[];
  /** 上游给的原话。我们算的百分比要和它对得上。 */
  display_message: string;
};

export type UsageProbeReport = {
  found: SelfReportedUsage[];
  /** 只有**真的出错**的才在这里；「上游没实现」不算错。 */
  errors: string[];
};

/* ---------------------------------------------------------------------------
   会话救援。

   Claude Code 按**模型声明的窗口**决定何时自动压缩，而走 ccLoad 时真正拦你的
   是**中转那一家的上限**。两个数对不上时（模型名挂 `[1m]`、中转其实只给
   500k），阈值被算在一个不存在的分母上，等触发时已经越过真实天花板 —— 之后
   `/compact` 自己也发不出去，因为它同样要把整段 transcript 发上去。

   字段对照 src-tauri/src/services/session_rescue.rs。
--------------------------------------------------------------------------- */

export type SessionInfo = {
  /** 会话 uuid，也是文件名。`claude --resume` 认它。 */
  id: string;
  path: string;
  cwd: string;
  /** Claude Code 起的短名，比 uuid 好认。 */
  slug: string;
  entries: number;
  bytes: number;
  /**
   * **真实**上下文，来自上游回报的 usage（input + 两个 cache 字段之和）——
   * 不是估的。只看 `input_tokens` 会小一个数量级：长会话里它常年是个位数。
   */
  last_context: number;
  peak_context: number;
  /** unix 秒 */
  modified_at: number;
  /** 有 claude 进程正拿着它 —— 这时候改是白改，进程落盘会盖回去。 */
  live: boolean;
  /** 已成功压缩过几次。>0 说明最后一个边界之前的内容本来就不进上下文。 */
  compactions: number;
};

export type SlimReport = {
  images_stripped: number;
  texts_truncated: number;
  bytes_before: number;
  bytes_after: number;
  context_before: number;
  context_after: number;
  backup: string;
};

export type CompactReport = {
  chunks: number;
  kept_tail: number;
  context_before: number;
  summary_tokens: number;
  backup: string;
};

/* ---------------------------------------------------------------------------
   会话预设 / 破禁：按 CLI 各写各的 resume 文件。
--------------------------------------------------------------------------- */

export type SessionTurn = {
  role: "user" | "assistant";
  text: string;
};

export type SessionPreset = {
  id: string;
  title: string;
  summary: string;
  builtin: boolean;
  turns: SessionTurn[];
};

export type SpawnItem = {
  target: CliTarget;
  session_id: string;
  path: string;
  command: string;
  launched: boolean;
  launch_error?: string;
  /** 写成了，但有件事你该知道。`launch_error` 是「没做成」，这个是「做了，
   *  而你可能不想要这个后果」—— 比如 Gemini 按目录基名存会话，撞了名字。 */
  note?: string;
};

export type SpawnResult = {
  cwd: string;
  items: SpawnItem[];
};

export type PresetPrefs = {
  last_cwd: string;
  last_targets: CliTarget[];
};

/* ---------------------------------------------------------------------------
   壳体自更新检查。

   `available` 为 false 时其余字段照样填着 —— 「已是最新」也要显示当前版本。
   查不到（断网、GitHub 限流）走的是 reject，界面静默吞掉：这是锦上添花的功能，
   不该在侧栏弹错误。
--------------------------------------------------------------------------- */

export type UpdateInfo = {
  /** 正在跑的这一版。beta 流水线会把完整 tag 戳进 tauri.conf.json。 */
  current: string;
  /** 上游最新一版的 tag；一个 release 都没有时是空串。 */
  latest: string;
  /** 那一版的 release 页面，拿不到就是 Releases 列表页。 */
  url: string;
  /** 最新那一版是不是预发布。 */
  prerelease: boolean;
  available: boolean;
};

/**
 * 一次检查的失败原因。分类是给重试用的，不是给展示用的。
 *
 * - `transport` —— 根本没连上（断网、DNS、TLS 握手失败、超时）。**只有它值得重试。**
 * - `http` —— GitHub 答了话但不是 2xx，最常见的是限流 403。重试只会烧得更快。
 * - `malformed` —— 连上了也是 2xx，但回来的东西读不成我们要的形状。重试无用。
 */
export type CheckError = {
  kind: "transport" | "http" | "malformed";
  message: string;
};
