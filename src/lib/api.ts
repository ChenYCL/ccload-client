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
  KernelConfig,
  KernelStatus,
  SyncOutcome,
  TakeoverOptions,
  TakeoverPreview,
  TakeoverResult,
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

  /** Open a URL in the system browser via the opener plugin. */
  openExternal: (url: string) => openUrl(url),
};
