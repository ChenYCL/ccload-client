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
  | "web-admin"
  | "cli"
  | "fallback"
  | "models"
  | "extensions"
  | "graph"
  | "settings";

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
