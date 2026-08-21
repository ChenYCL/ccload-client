//! Thin typed wrappers around the Tauri commands. The renderer never
//! constructs invoke strings itself — a missing command then fails at
//! compile time instead of at click time.

import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type {
  AppSettings,
  BackupEntry,
  CliTarget,
  ConfigFileView,
  EnvKeyInfo,
  Envelope,
  ExtensionItem,
  ExtensionKind,
  ExtensionSpec,
  ExtensionSupport,
  FallbackChain,
  GraphDoc,
  GraphValidation,
  ImportEntry,
  ImportPreview,
  ImportResult,
  CompactReport,
  InjectOutcome,
  InjectSpec,
  SessionInfo,
  SlimReport,
  InjectState,
  KernelConfig,
  KernelStatus,
  McpUsage,
  RefreshMode,
  UsageProbeReport,
  RefreshResult,
  SyncOutcome,
  TakeoverOptions,
  TakeoverPreview,
  TakeoverResult,
  VisionTargetState,
} from "../types";

export const api = {
  kernelStatus: () => invoke<KernelStatus>("kernel_status"),
  kernelStart: () => invoke<KernelStatus>("kernel_start"),
  kernelStop: () => invoke<KernelStatus>("kernel_stop"),
  kernelConfig: () => invoke<KernelConfig>("kernel_config"),
  embedProxyUrl: () => invoke<string | null>("embed_proxy_url"),
  /** Open (or focus) the standalone admin window on a web page. */
  openAdminWindow: (page?: string) =>
    invoke<void>("open_admin_window", { page: page ?? null }),

  settingsGet: () => invoke<AppSettings>("settings_get"),
  settingsSetKernel: (kernel: KernelConfig) =>
    invoke<AppSettings>("settings_set_kernel", { kernel }),
  settingsSetSandbox: (sandbox: boolean) =>
    invoke<AppSettings>("settings_set_sandbox", { sandbox }),
  settingsSetClientToken: (token: string) =>
    invoke<void>("settings_set_client_token", { token }),

  /** The only admin-API surface. Path is relative to `/admin`. */
  admin: <T>(
    method: string,
    path: string,
    opts?: { query?: string; body?: unknown },
  ) =>
    invoke<Envelope<T>>("admin_request", {
      method,
      path,
      query: opts?.query ?? null,
      body: opts?.body ?? null,
    }),

  cliPreviewAll: () => invoke<TakeoverPreview[]>("cli_preview_all"),
  cliApply: (target: CliTarget, options?: TakeoverOptions) =>
    invoke<TakeoverResult>("cli_apply", { target, options: options ?? null }),
  cliBackups: (target?: CliTarget) =>
    invoke<BackupEntry[]>("cli_backups", { target: target ?? null }),
  cliRestore: (backupId: string) =>
    invoke<string[]>("cli_restore", { backupId }),
  cliReadFiles: (target: CliTarget) =>
    invoke<ConfigFileView[]>("cli_read_files", { target }),
  cliWriteFile: (target: CliTarget, rel: string, body: string) =>
    invoke<string>("cli_write_file", { target, rel, body }),
  cliEnvKeys: (target: CliTarget) =>
    invoke<EnvKeyInfo[]>("cli_env_keys", { target }),

  fallbackList: () => invoke<FallbackChain[]>("fallback_list"),
  fallbackSave: (chain: FallbackChain) =>
    invoke<FallbackChain[]>("fallback_save", { chain }),
  fallbackDelete: (alias: string) =>
    invoke<FallbackChain[]>("fallback_delete", { alias }),
  fallbackApply: (alias: string) =>
    invoke<string[]>("fallback_apply", { alias }),

  /* --- 调度图 ------------------------------------------------------------ */

  graphList: () => invoke<GraphDoc[]>("graph_list"),
  graphSave: (doc: GraphDoc) => invoke<GraphDoc[]>("graph_save", { doc }),
  /** 纯计算，不落盘；UI 每次改动都调它做即时校验。 */
  graphValidate: (doc: GraphDoc) => invoke<GraphValidation>("graph_validate", { doc }),
  /** 校验不过时后端一个字都不写。 */
  graphApply: (id: string) => invoke<string[]>("graph_apply", { id }),

  /* --- 配置迁移 ---------------------------------------------------------- */

  /** 导出到 path。include_secrets=false 时密码与令牌留空。 */
  configExport: (path: string, includeSecrets: boolean) =>
    invoke<string>("config_export", { path, includeSecrets }),
  configImportPreview: (path: string) =>
    invoke<ImportPreview>("config_import_preview", { path }),
  /** 模型链按别名合并；applyKernel=true 时才动内核连接设置。 */
  configImport: (path: string, applyKernel: boolean) =>
    invoke<string[]>("config_import", { path, applyKernel }),

  /** Rust 侧原生对话框。JS 侧 plugin-dialog 在部分机器上静默失败，见命令注释。 */
  pickSavePath: (defaultName: string) =>
    invoke<string | null>("pick_save_path", { defaultName }),
  pickOpenPath: () => invoke<string | null>("pick_open_path"),

  /** 打包进壳体的 ccLoad 版本（构建期从 vendor/ccLoad 的 git tag 注入）。 */
  kernelBundledVersion: () => invoke<string>("kernel_bundled_version"),

  modelImport: (target: CliTarget, entries: ImportEntry[]) =>
    invoke<ImportResult>("model_import", { target, entries }),
  visionMcpSet: (target: CliTarget, enabled: boolean, model?: string) =>
    invoke<string[]>("vision_mcp_set", {
      target,
      enabled,
      model: model ?? null,
    }),
  /**
   * 五个 CLI 上视觉 MCP 的真实状态（装没装、用哪个模型、凭证还对不对）。
   * 模型选择读的是磁盘，不是按钮的记忆 —— 切走再回来仍能看到自己选的那个。
   */
  visionMcpState: () => invoke<VisionTargetState[]>("vision_mcp_state"),

  /**
   * 让内核去问上游要模型清单并写回渠道。
   *
   * `replace` 才会删掉上游已经没有的模型 —— `merge`（内核默认）只增不删，
   * 退役的模型会一直留在渠道里。上游改过清单之后要用 replace。
   */
  channelsRefreshModels: (channelIds: number[], mode: RefreshMode) =>
    invoke<Envelope<RefreshResult>>("admin_request", {
      method: "POST",
      path: "channels/models/refresh-batch",
      query: null,
      body: { channel_ids: channelIds, mode },
    }),

  /**
   * 问这些渠道的上游有没有自报用量（根地址下的 `GET /usage`）。
   * 没实现的渠道会被静默跳过 —— 那是绝大多数渠道的正常状态。
   */
  channelUsageProbe: (channelIds: number[]) =>
    invoke<UsageProbeReport>("channel_usage_probe", { channelIds }),

  /** 自带 MCP 工具（ccload-vision）的调用次数与耗时。别家 MCP 不在口径内。 */
  mcpUsageStats: () => invoke<McpUsage>("mcp_usage_stats"),
  mcpUsageClear: () => invoke<void>("mcp_usage_clear"),

  /* --- 系统注入：写进各 CLI 的全局指令文件 ------------------------------- */

  injectState: () => invoke<InjectState[]>("inject_state"),
  /** 预览块内容。视觉那段的工具名由后端生成，前端不另抄一份（抄了必漂）。 */
  injectPreview: (spec: InjectSpec) => invoke<string>("inject_preview", { spec }),
  /** spec 全空即移除。逐目标独立成败。 */
  injectApply: (targets: CliTarget[], spec: InjectSpec) =>
    invoke<InjectOutcome[]>("inject_apply", { targets, spec }),

  /* --- 扩展管理：MCP / Skill / Agent / Hook 跨 5 个 CLI ------------------ */

  /** 5 CLI × 4 类的支持矩阵。前端拿它决定哪些目标可点、哪些要置灰。 */
  extensionsSupport: () => invoke<ExtensionSupport[]>("extensions_support"),
  /** kind 省略时返回四类全集；该 CLI 不支持的类型不会出现（不报错）。 */
  extensionsList: (target: CliTarget, kind?: ExtensionKind) =>
    invoke<ExtensionItem[]>("extensions_list", { target, kind: kind ?? null }),
  /** 同 id 存在即覆盖；skill/agent 被换下的旧版本会以 `…（已归档）` 出现在返回值里。 */
  extensionInstall: (target: CliTarget, kind: ExtensionKind, spec: ExtensionSpec) =>
    invoke<string[]>("extension_install", { target, kind, spec }),
  extensionRemove: (target: CliTarget, kind: ExtensionKind, id: string) =>
    invoke<string[]>("extension_remove", { target, kind, id }),
  /** 读回规范化描述，供编辑框回填。 */
  extensionRead: (target: CliTarget, kind: ExtensionKind, id: string) =>
    invoke<ExtensionSpec>("extension_read", { target, kind, id }),
  /**
   * 一处配置推到多个 CLI，各自转成原生格式。source 省略时后端按固定顺序挑
   * 第一个装了它的 CLI 当来源。逐目标独立成败，看返回数组的每一行。
   */
  extensionSync: (
    kind: ExtensionKind,
    id: string,
    targets: CliTarget[],
    source?: CliTarget,
  ) =>
    invoke<SyncOutcome[]>("extension_sync", {
      kind,
      id,
      targets,
      source: source ?? null,
    }),

  /** 扫本机所有 Claude Code 会话。读几十 MB，后端跑在 blocking 线程池上。 */
  sessionList: () => invoke<SessionInfo[]>("session_list"),
  /**
   * 瘦身：砍图 + 截长工具结果。纯本地、不花 token，但信息是真丢了。
   * `target` 是**真实**口径的目标上下文。
   */
  sessionSlim: (path: string, target: number, textLimit: number) =>
    invoke<SlimReport>("session_slim", { path, target, textLimit }),
  /**
   * 分块总结：把活动链切成小段各自总结，再追加原生的压缩边界 + 摘要。
   * 旧内容一个字节不动，所以出问题还能回去。
   */
  sessionCompact: (path: string, model: string, keepTail: number, chunkTokens: number) =>
    invoke<CompactReport>("session_compact", { path, model, keepTail, chunkTokens }),

  /** Open a URL in the system browser via the opener plugin. */
  openExternal: (url: string) => openUrl(url),
};
