import { useT } from "../i18n";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { api } from "../lib/api";
import type { ImportPreview, KernelConfig, KernelMode, SettingItem } from "../types";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import { TextInput } from "../components/ui/Input";
import { CopyButton } from "../components/ui/CopyButton";
import { Download, Upload } from "lucide-react";


/// 托管模式下内核绑的回环端口。
const DEFAULT_PORT = 15722;

export function SettingsPage() {
  const t = useT();
  const qc = useQueryClient();
  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  // 打包版本来自构建期注入，不再手写常量 —— 手写的那个曾经停在 "v1.2.0"，
  // 而实际打进去的是 v4.6.x，于是这块永远在报一个假的版本不一致。
  const bundled = useQuery({
    queryKey: ["bundled-kernel-version"],
    queryFn: api.kernelBundledVersion,
    staleTime: Infinity,
  });
  const settings = useQuery({ queryKey: ["app-settings"], queryFn: api.settingsGet });
  const running = kernel.data?.state === "running";
  const items = useQuery({
    queryKey: ["sys-settings"],
    queryFn: () => api.admin<SettingItem[]>("GET", "settings"),
    enabled: running,
  });
  const sandbox = useMutation({
    mutationFn: (v: boolean) => api.settingsSetSandbox(v),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["app-settings"] }),
  });

  const list = items.data?.data ?? [];
  const remoteVersion =
    kernel.data?.state === "running" ? kernel.data.version : null;
  const versionMismatch =
    remoteVersion !== null &&
    !!bundled.data &&
    remoteVersion !== bundled.data &&
    // dev kernels report the pseudo-version "dev"; nothing to compare.
    remoteVersion !== "dev";

  return (
    <div>
      <h1 className="t-display">{t("设置")}</h1>
      {settings.data && <ConnectionForm current={settings.data.kernel} />}
      {running && (
        <section className="mt-6 card p-4">
          <h2 className="t-title">{t("内核版本")}</h2>
          <div className="mt-3 flex items-center justify-between text-sm">
            <span className="text-muted">{t("壳体打包内核")}</span>
            <code className="font-mono text-content">{bundled.data ?? "—"}</code>
          </div>
          <div className="mt-2 flex items-center justify-between text-sm">
            <span className="text-muted">
              {settings.data?.kernel.mode === "managed" ? t("当前运行内核") : t("远端内核")}
            </span>
            <code className="font-mono text-content">{remoteVersion ?? "—"}</code>
          </div>
          {versionMismatch && (
            <p className="mt-3 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs">
              {t("远端内核版本（")}{remoteVersion}{t("）与壳体打包版本（")}{bundled.data}{t("）不一致。 Admin API 的字段与校验规则随版本变化，建议切回本机内核或把远端升级到同版本， 否则设置、渠道编辑等操作可能拿到意料外的响应。")}
            </p>
          )}
        </section>
      )}
      {running && kernel.data?.state === "running" && (
        <EndpointsCard
          baseUrl={kernel.data.base_url}
          token={settings.data?.client_api_token ?? null}
        />
      )}

      <MigrationCard />

      <section className="mt-6 card p-4">
        <h2 className="t-title">{t("CLI 写入")}</h2>
        <label className="mt-3 flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={!!settings.data?.sandbox_cli_writes}
            onChange={(e) => sandbox.mutate(e.target.checked)}
          />
          {t("走沙箱（~/.ccload-client/sandbox），不改真实 CLI 配置")}
        </label>
      </section>
      <section className="mt-6">
        <h2 className="t-title">{t("内核运行时配置")}</h2>
        <p className="mt-1 text-xs text-muted">
          {t("字段来自 GET /admin/settings，新增项会自动出现。改任何一项都会写库并让 内核约 2 秒后自动重启，在途请求会被打断，请避开使用中修改。")}
        </p>
        <div className="mt-3 space-y-2">
          {list.map((it) => (
            <SettingRow key={it.key} item={it} disabled={!it.editable} />
          ))}
        </div>
      </section>
    </div>
  );
}

/// Managed vs remote. Switching mode only persists config; the caller still has
/// to restart the kernel, so we say so instead of silently doing it.
function ConnectionForm({ current }: { current: KernelConfig }) {
  const t = useT();
  const qc = useQueryClient();
  const [draft, setDraft] = useState<KernelConfig>(current);
  const [showPassword, setShowPassword] = useState(false);
  const dirty = JSON.stringify(draft) !== JSON.stringify(current);

  const save = useMutation({
    mutationFn: (cfg: KernelConfig) => api.settingsSetKernel(cfg),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["app-settings"] });
      qc.invalidateQueries({ queryKey: ["kernel"] });
    },
  });
  const restart = useMutation({
    mutationFn: async () => {
      await api.kernelStop();
      return api.kernelStart();
    },
    onSuccess: () => qc.invalidateQueries({ queryKey: ["kernel"] }),
  });

  const set = (patch: Partial<KernelConfig>) =>
    setDraft((d) => ({ ...d, ...patch }));

  return (
    <section className="mt-6 card p-4">
      <h2 className="t-title">{t("连接方式")}</h2>
      <div className="mt-3 flex gap-2">
        {(["managed", "remote"] as KernelMode[]).map((m) => (
          <button
            key={m}
            onClick={() => set({ mode: m })}
            className={cn(
              "flex-1 rounded-md border px-3 py-2 text-left text-sm",
              draft.mode === m
                ? "border-accent bg-accent/10"
                : "border-border hover:bg-surface-2",
            )}
          >
            <div className="font-medium">
              {m === "managed" ? t("本机内核") : t("远端 ccLoad")}
            </div>
            <div className="mt-0.5 text-[11px] text-muted">
              {m === "managed"
                ? t("客户端自己拉起内核进程，数据留在本机")
                : t("连到已有实例（VPS / HF Space）")}
            </div>
          </button>
        ))}
      </div>

      {draft.mode === "managed" ? (
        <Field label={t("端口")} hint={t("内核在本机监听的回环端口")}>
          <TextInput
            type="number"
            value={draft.port}
            // 清空输入框时别落成 0：0 会让内核绑到随机端口，壳体却按配置里的
            // 端口去探活，一直探不到，最后以启动超时收场。
            onChange={(e) => set({ port: Number(e.target.value) || DEFAULT_PORT })}
            className="tabular-nums"
          />
        </Field>
      ) : (
        <Field label={t("远端地址")} hint={t("完整 origin，例如 https://xxx.hf.space")}>
          <TextInput
            mono
            value={draft.remote_url ?? ""}
            placeholder="https://your-ccload.example.com"
            onChange={(e) => set({ remote_url: e.target.value || null })}
          />
        </Field>
      )}

      <Field label={t("管理密码")} hint={t("远端模式填该实例的 CCLOAD_PASS")}>
        <div className="flex items-center gap-2">
          <TextInput
            type={showPassword ? "text" : "password"}
            value={draft.admin_password}
            onChange={(e) => set({ admin_password: e.target.value })}
            className="min-w-0 flex-1"
          />
          {/* 内核后台会自动登录，但会话过期或密码填错时还是要手输一次；
              密码框看不见内容，粘贴完也没法核对，所以这两个都得有。 */}
          <button
            type="button"
            onClick={() => setShowPassword((v) => !v)}
            className="shrink-0 rounded-md border border-border px-2 py-1 text-[11px] text-muted hover:bg-surface-2"
          >
            {showPassword ? t("隐藏") : t("显示")}
          </button>
          <CopyButton value={draft.admin_password} />
        </div>
      </Field>

      <div className="mt-3 flex items-center gap-2">
        <button
          disabled={!dirty || save.isPending}
          onClick={() => save.mutate(draft)}
          className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
        >
          {save.isPending ? t("保存中…") : t("保存")}
        </button>
        <button
          disabled={restart.isPending}
          onClick={() => restart.mutate()}
          className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
        >
          {restart.isPending ? t("重启中…") : t("重启内核")}
        </button>
        {dirty && (
          <span className="text-xs text-amber-700">{t("改动未保存，保存后需重启生效")}</span>
        )}
      </div>
      {save.isError && (
        <p className="mt-2 text-xs text-red-600">{errText(save.error)}</p>
      )}
      {restart.isError && (
        <p className="mt-2 text-xs text-red-600">{errText(restart.error)}</p>
      )}
    </section>
  );
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint: string;
  children: React.ReactNode;
}) {
  return (
    <label className="mt-3 block rounded-md border border-border px-3 py-2 text-sm">
      <div className="text-xs text-muted">
        {label} · {hint}
      </div>
      <div className="mt-1">{children}</div>
    </label>
  );
}

function SettingRow({ item, disabled }: { item: SettingItem; disabled: boolean }) {
  const qc = useQueryClient();
  const save = useMutation({
    mutationFn: (value: string) =>
      api.admin("PUT", `settings/${item.key}`, { body: { value } }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["sys-settings"] }),
  });
  if (item.value_type === "bool") {
    return (
      <label className="flex items-center justify-between rounded-md border border-border px-3 py-2 text-sm">
        <span>
          {item.key}
          <span className="ml-2 text-xs text-muted">{item.description}</span>
        </span>
        <input
          type="checkbox"
          disabled={disabled}
          checked={item.value === "true"}
          onChange={(e) => save.mutate(e.target.checked ? "true" : "false")}
        />
      </label>
    );
  }
  return (
    <label className="block rounded-md border border-border px-3 py-2 text-sm">
      <div className="text-xs text-muted">
        {item.key} · {item.description}
      </div>
      <TextInput
        mono
        disabled={disabled}
        defaultValue={item.value}
        onBlur={(e) => {
          if (e.target.value !== item.value) save.mutate(e.target.value);
        }}
        className="mt-1"
      />
    </label>
  );
}

/// 接入地址。
///
/// 内核本身就是这台机器上的出口代理：`/v1/*` 和 `/v1beta/*` 全量走
/// `HandleProxyRequest`，协议注册表按请求路径/内容自动在 Anthropic、OpenAI、
/// Gemini、Codex 之间双向翻译。也就是说任何认这几种规范的程序都能直接指过来，
/// 不需要客户端再自己起一层代理 —— 这里只负责把地址和令牌摆出来能一键复制。
function EndpointsCard({ baseUrl, token }: { baseUrl: string; token: string | null }) {
  const t = useT();
  const rows: { label: string; hint: string; value: string }[] = [
    {
      label: t("Anthropic 规范"),
      hint: "ANTHROPIC_BASE_URL · /v1/messages",
      value: baseUrl,
    },
    {
      label: t("OpenAI 规范"),
      hint: "OPENAI_BASE_URL · /v1/chat/completions",
      value: `${baseUrl}/v1`,
    },
    {
      label: t("Gemini 规范"),
      hint: "/v1beta/models/{model}:generateContent",
      value: `${baseUrl}/v1beta`,
    },
  ];
  return (
    <section className="mt-6 card p-4">
      <h2 className="t-title">{t("接入地址")}</h2>
      <p className="mt-1 text-sm text-muted">
        {t("内核同时提供这几套规范的入口，协议转换在内核里完成。第三方工具直接填下面的 地址和令牌即可，不必经过 CLI 接管。")}
      </p>
      <div className="mt-3 space-y-1.5">
        {rows.map((r) => (
          <CopyRow key={r.label} label={r.label} hint={r.hint} value={r.value} />
        ))}
        <CopyRow
          label={t("API 令牌")}
          hint={t("客户端为自己申请的那把，可在内核后台吊销")}
          value={token ?? ""}
          secret
        />
      </div>
    </section>
  );
}

function CopyRow({
  label,
  hint,
  value,
  secret,
}: {
  label: string;
  hint: string;
  value: string;
  secret?: boolean;
}) {
  const t = useT();
  const [shown, setShown] = useState(false);
  // 令牌默认打码：这一页可能出现在截图和录屏里。
  const display = !value ? "—" : secret && !shown ? "•".repeat(24) : value;

  return (
    <div className="flex items-center gap-3 rounded-lg border border-border px-3 py-2">
      <div className="w-32 shrink-0">
        <div className="text-sm">{label}</div>
        <div className="text-[10px] text-muted">{hint}</div>
      </div>
      <code className="min-w-0 flex-1 truncate font-mono text-xs" title={secret ? undefined : value}>
        {display}
      </code>
      {secret && value && (
        <button
          onClick={() => setShown((v) => !v)}
          className="shrink-0 rounded-md border border-border px-2 py-1 text-[11px] text-muted hover:bg-surface-2"
        >
          {shown ? t("隐藏") : t("显示")}
        </button>
      )}
      <CopyButton value={value} />
    </div>
  );
}

/// 配置迁移。换机器时把内核连接方式和模型链带走。
///
/// 渠道和令牌不在这里 —— 那是内核的数据，内核后台自带 CSV 导入导出。这边再抄一份
/// 就要永远追着内核的字段跑。
function MigrationCard() {
  const t = useT();
  const qc = useQueryClient();
  const [includeSecrets, setIncludeSecrets] = useState(false);
  const [preview, setPreview] = useState<{ path: string; info: ImportPreview } | null>(null);
  const [applyKernel, setApplyKernel] = useState(true);
  const [note, setNote] = useState<string | null>(null);

  const exportIt = useMutation({
    mutationFn: async () => {
      const path = await api.pickSavePath("ccload-client-config.json");
      if (!path) return null;
      return api.configExport(path, includeSecrets);
    },
    onSuccess: (p) => setNote(p ? `已导出到 ${p}` : null),
    onError: (e) => setNote(errText(e)),
  });

  const pick = useMutation({
    mutationFn: async () => {
      const path = await api.pickOpenPath();
      if (typeof path !== "string") return null;
      return { path, info: await api.configImportPreview(path) };
    },
    onSuccess: (v) => {
      setPreview(v);
      setNote(null);
    },
    onError: (e) => setNote(errText(e)),
  });

  const doImport = useMutation({
    mutationFn: () => api.configImport(preview!.path, applyKernel),
    onSuccess: (lines) => {
      setNote(lines.join("；"));
      setPreview(null);
      qc.invalidateQueries({ queryKey: ["app-settings"] });
      qc.invalidateQueries({ queryKey: ["fallback"] });
    },
    onError: (e) => setNote(errText(e)),
  });

  return (
    <section className="mt-6 card p-4">
      <h2 className="t-title">{t("配置迁移")}</h2>
      <p className="mt-1 text-sm text-muted">
        {t("导出内核连接方式与模型链，换机器时导入即可。渠道和令牌属于内核数据， 在「内核后台」用它自带的导入导出。")}
      </p>

      <label className="mt-3 flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={includeSecrets}
          onChange={(e) => setIncludeSecrets(e.target.checked)}
        />
        <span>
          {t("包含密钥（管理密码与 API 令牌）")}
          <span className="ml-1.5 text-xs text-muted">
            {t("拿到就能直接调内核全部管理接口，别丢进聊天或云盘")}
          </span>
        </span>
      </label>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        <button
          onClick={() => exportIt.mutate()}
          disabled={exportIt.isPending}
          className="flex items-center gap-1.5 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
        >
          <Upload className="h-4 w-4" /> {t("导出配置")}
        </button>
        <button
          onClick={() => pick.mutate()}
          disabled={pick.isPending}
          className="flex items-center gap-1.5 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
        >
          <Download className="h-4 w-4" /> {t("导入配置")}
        </button>
      </div>

      {/* 导入是覆盖性动作，先把「会发生什么」摊开再让用户点确认。 */}
      {preview && (
        <div className="mt-3 rounded-xl border border-border p-3 text-xs">
          <div className="font-medium">{t("即将导入")}</div>
          <ul className="mt-1.5 space-y-1 text-muted">
            <li>{t("来源客户端内核版本：")}{preview.info.client_kernel_version}</li>
            <li>
              {t("内核连接：")}{preview.info.kernel_mode} · {preview.info.kernel_endpoint || t("（未设置）")}
              {!preview.info.includes_secrets && t("（文件不含密钥，本机密码保持不变）")}
            </li>
            <li>
              {t("模型链")} {preview.info.chain_aliases.length} {t("条：")}
              {preview.info.chain_aliases.join("、") || t("无")}
            </li>
            {preview.info.overwritten_aliases.length > 0 && (
              <li className="text-amber-700">
                {t("会覆盖同名的本机链：")}{preview.info.overwritten_aliases.join("、")}
              </li>
            )}
          </ul>
          <label className="mt-2.5 flex items-center gap-2">
            <input
              type="checkbox"
              checked={applyKernel}
              onChange={(e) => setApplyKernel(e.target.checked)}
            />
            {t("一并应用内核连接设置（不勾则只导入模型链）")}
          </label>
          <div className="mt-3 flex justify-end gap-2">
            <button
              onClick={() => setPreview(null)}
              className="rounded-lg border border-border bg-surface-raised px-3 py-1 hover:bg-surface-2"
            >
              {t("取消")}
            </button>
            <button
              onClick={() => doImport.mutate()}
              disabled={doImport.isPending}
              className="rounded-lg bg-accent px-3 py-1 font-medium text-white hover:bg-accent/90 disabled:opacity-40"
            >
              {doImport.isPending ? t("导入中…") : t("确认导入")}
            </button>
          </div>
        </div>
      )}

      {note && <p className="mt-3 break-all text-xs text-muted">{note}</p>}
    </section>
  );
}
