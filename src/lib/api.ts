//! Thin typed wrappers around the Tauri commands. The renderer never
//! constructs invoke strings itself — a missing command then fails at
//! compile time instead of at click time.

import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import type {
  AppSettings,
  BackupDiff,
  BackupEntry,
  CliTarget,
  ConfigFileView,
  ContextPolicy,
  Pin,
  PinOutcome,
  RouteHit,
  TierRow,
  WindowPreview,
  DiffBase,
  EnvKeyInfo,
  Envelope,
  ExtensionItem,
  ExtensionKind,
  ExtensionSpec,
  ExtensionSupport,
  FallbackChain,
  ForcedRoute,
  GraphDoc,
  GraphValidation,
  ImageApi,
  ImageTargetState,
  ImportEntry,
  ImportPreview,
  ImportResult,
  CompactReport,
  DeleteReport,
  SessionPreset,
  PresetPrefs,
  SpawnResult,
  InjectOutcome,
  InjectSpec,
  SessionInfo,
  SlimReport,
  InjectState,
  KernelConfig,
  KernelStatus,
  McpUsage,
  NodeService,
  NodeServiceStatus,
  CliUsageReport,
  ProxyRecord,
  RefreshMode,
  UsageProbeReport,
  RefreshResult,
  SessionRef,
  SyncOutcome,
  TakeoverOptions,
  TakeoverPreview,
  TakeoverResult,
  UpdateInfo,
  VisionTargetState,
} from "../types";

export const api = {
  kernelStatus: () => invoke<KernelStatus>("kernel_status"),
  kernelStart: () => invoke<KernelStatus>("kernel_start"),
  kernelStop: () => invoke<KernelStatus>("kernel_stop"),
  kernelConfig: () => invoke<KernelConfig>("kernel_config"),
  embedProxyUrl: () => invoke<string | null>("embed_proxy_url"),
  /** CLI 该指向哪儿：代理起着就是代理地址，否则是内核地址。 */
  cliProxyUrl: () => invoke<string | null>("cli_proxy_url"),
  /** 最近的转发记录，最新的在前。会话归因全靠它。 */
  cliProxyRecords: () => invoke<ProxyRecord[]>("cli_proxy_records"),
  /** 缓存窗口是否升到 1h。默认关 —— 交互式会话开着更贵（写入价 2×）。 */
  cliProxyLongCache: () => invoke<boolean>("cli_proxy_long_cache"),
  cliProxySetLongCache: (enabled: boolean) =>
    invoke<boolean>("cli_proxy_set_long_cache", { enabled }),
  /** 把会话 id 解析成标题和磁盘路径。 */
  cliProxySession: (sessionId: string) =>
    invoke<SessionRef>("cli_proxy_session", { sessionId }),

  /** 受管的 Node 常驻服务：MCP over http/sse、自定义后端。 */
  nodeServiceList: () => invoke<NodeService[]>("node_service_list"),
  nodeServiceSave: (service: NodeService) =>
    invoke<NodeService[]>("node_service_save", { service }),
  nodeServiceDelete: (id: string) =>
    invoke<NodeService[]>("node_service_delete", { id }),
  nodeServiceStart: (id: string) =>
    invoke<NodeServiceStatus>("node_service_start", { id }),
  nodeServiceStop: (id: string) => invoke<void>("node_service_stop", { id }),
  nodeServiceStatus: () => invoke<NodeServiceStatus[]>("node_service_status"),
  /** 把模板脚本写到默认位置，返回落盘路径（给 entry 用）。 */
  nodeServiceWriteScript: (suggestedName: string, body: string) =>
    invoke<string>("node_service_write_script", { suggestedName, body }),

  /** 今日按 CLI / 会话的消耗聚合（代理记录 × 内核日志配对）。 */
  cliProxyUsage: () => invoke<CliUsageReport>("cli_proxy_usage"),
  /** Open (or focus) the standalone admin window on a web page. */
  openAdminWindow: (page?: string) =>
    invoke<void>("open_admin_window", { page: page ?? null }),

  /**
   * 把内核管理页面停进主窗口内容区的占位元素处（docked 子 webview）。
   * `rect` 是占位元素相对**窗口内容区**的逻辑坐标。
   */
  adminDockShow: (
    file: string,
    rect: { x: number; y: number; width: number; height: number },
  ) => invoke<void>("admin_dock_show", { file, ...rect }),
  /** 布局变化后的坐标修正。返回「面板是否真的挪了」—— 面板还没挂出来时是
      false，调用方必须靠它决定要不要重试，否则挂载瞬间那个过期坐标会永久留在
      屏幕上（表现：面板盖住页面标题）。 */
  adminDockBounds: (rect: { x: number; y: number; width: number; height: number }) =>
    invoke<boolean>("admin_dock_bounds", rect),
  /** 离开页面 / 收起时藏起来（不销毁 —— 保留会话与页面状态）。 */
  adminDockHide: () => invoke<void>("admin_dock_hide"),

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
  /** 切「CLI 走本地代理」。只改设置，返回是否还需要点「写入」才生效。 */
  cliSetProxyRouting: (enabled: boolean) =>
    invoke<boolean>("cli_set_proxy_routing", { enabled }),

  /** 上下文窗口总控。五家 CLI 各写各的键，但用同一个策略。 */
  contextPolicyGet: () => invoke<ContextPolicy>("context_policy_get"),
  /** 只存策略不写 CLI；返回还有几家已接管的 CLI 需要点「写入」才会跟上。 */
  contextPolicySet: (policy: ContextPolicy) =>
    invoke<number>("context_policy_set", { policy }),
  /** 每家 CLI 按当前策略会写成多少 —— 和真正写入走同一条解析路径。 */
  contextWindowPreview: () => invoke<WindowPreview[]>("context_window_preview"),
  /** 分档表：每个可能用到的模型一行，含「自动会算成多少」。 */
  contextTiers: () => invoke<TierRow[]>("context_tiers"),
  /** 某个别名在内核里会落到哪些渠道，按优先级从高到低。内核没连上会报错。 */
  aliasRoutes: (alias: string) => invoke<RouteHit[]>("alias_routes", { alias }),
  /** 首选渠道钉住：`pin_save` 会顺手把私有别名写进内核并刷代理，`pin_delete` 反之。 */
  pinList: () => invoke<Pin[]>("pin_list"),
  pinSave: (pin: Pin) => invoke<PinOutcome>("pin_save", { pin }),
  pinDelete: (alias: string) => invoke<PinOutcome>("pin_delete", { alias }),
  cliApply: (target: CliTarget, options?: TakeoverOptions) =>
    invoke<TakeoverResult>("cli_apply", { target, options: options ?? null }),
  cliBackups: (target?: CliTarget) =>
    invoke<BackupEntry[]>("cli_backups", { target: target ?? null }),
  cliRestore: (backupId: string) =>
    invoke<string[]>("cli_restore", { backupId }),
  /**
   * 一份快照相对某个基准改了什么。
   *
   * 基准默认 `current`（磁盘现状）—— 决定「要不要点恢复」时唯一相关的问题是
   * 「它会把我现在的配置改成什么样」。也可以比上一份快照或原始配置。
   */
  cliBackupDiff: (backupId: string, base: DiffBase = "current") =>
    invoke<BackupDiff>("cli_backup_diff", { backupId, base }),
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

  /* --- 强制路由 --------------------------------------------------------- */

  /** ai-go 那种「请求某模型 → 钉到渠道+上游模型」。存这里，apply 才写渠道。 */
  forcedRouteList: () => invoke<ForcedRoute[]>("forced_route_list"),
  forcedRouteSave: (route: ForcedRoute) =>
    invoke<ForcedRoute[]>("forced_route_save", { route }),
  forcedRouteDelete: (from: string) =>
    invoke<ForcedRoute[]>("forced_route_delete", { from }),
  /** 把 from 别名以递减优先级写进每个目标渠道，redirect 到目标上游模型。 */
  forcedRouteApply: (from: string) =>
    invoke<string[]>("forced_route_apply", { from }),

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

  /**
   * 有没有新版壳体。只读一次 GitHub Releases，不下载也不替换。
   *
   * `current` 必须传 `getVersion()` 读到的那个值 —— Rust 侧的 CARGO_PKG_VERSION
   * 是编译期基座（0.1.0），拿它去比会让所有 beta 用户永远看到「有新版」。
   */
  checkClientUpdate: (current: string) => invoke<UpdateInfo>("check_client_update", { current }),

  /** `prune` 会把本次清单以外的旧别名从 OpenCode 目录里清掉 —— 那些正是内核
      已经不认、选中即 503 的条目。默认关闭，只增不删。 */
  modelImport: (target: CliTarget, entries: ImportEntry[], prune?: boolean) =>
    invoke<ImportResult>("model_import", { target, entries, prune: prune ?? false }),
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
   * 装 / 卸生图 MCP。
   *
   * `api` 是能力开关不是口味：`images` 是标准 OpenAI 生图端点，只能生成；
   * `chat` 走 `/v1/chat/completions` + `modalities:["image"]`，**改图只有它能做**。
   * 省略即 `auto` —— 按模型挑一条先试，端点挑错了当场换另一条。
   */
  imageMcpSet: (
    target: CliTarget,
    enabled: boolean,
    model?: string,
    imageApi?: ImageApi,
    outDir?: string,
  ) =>
    invoke<string[]>("image_mcp_set", {
      target,
      enabled,
      model: model ?? null,
      api: imageApi ?? null,
      outDir: outDir ?? null,
    }),
  /** 五个 CLI 上生图 MCP 的真实状态。同样读磁盘，不靠按钮的记忆。 */
  imageMcpState: () => invoke<ImageTargetState[]>("image_mcp_state"),

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

  /** 自带 MCP 工具（ccload-vision / ccload-image）的调用次数与耗时。别家 MCP 不在口径内。 */
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
  /**
   * 删掉选中的会话。不可恢复，调用方必须先弹确认。
   * 活着的会话后端会跳过；一条失败不拖累其余。
   */
  sessionDelete: (paths: string[]) => invoke<DeleteReport>("session_delete", { paths }),

  pickFolder: () => invoke<string | null>("pick_folder"),

  presetList: () => invoke<SessionPreset[]>("preset_list"),
  presetPrefs: () => invoke<PresetPrefs>("preset_prefs"),
  /** 藏 / 显内置预设（视图偏好，不动二进制里的内置）。返回更新后的列表。 */
  presetSetHideBuiltins: (hide: boolean) =>
    invoke<SessionPreset[]>("preset_set_hide_builtins", { hide }),
  presetSave: (preset: SessionPreset) => invoke<SessionPreset[]>("preset_save", { preset }),
  presetDelete: (id: string) => invoke<SessionPreset[]>("preset_delete", { id }),
  /**
   * 写出会话并（可选）拉起终端。
   *
   * `confine` 是「这个会话只在 cwd 里干活」：不预先关掉权限询问，Codex 还会钉住
   * 工作根。默认 true —— 不锁才是危险的那一档，默认值必须是安全的那个。
   */
  presetSpawn: (
    id: string,
    cwd: string,
    extraUser: string,
    launch: boolean,
    targets: CliTarget[],
    confine = true,
  ) =>
    invoke<SpawnResult>("preset_spawn", {
      id,
      cwd,
      extraUser: extraUser || null,
      launch,
      confine,
      targets,
    }),

  /** Open a URL in the system browser via the opener plugin. */
  openExternal: (url: string) => openUrl(url),
};
