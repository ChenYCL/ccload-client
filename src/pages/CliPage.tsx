import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../lib/api";
import type { BackupEntry, CliTarget, ConfigFileView, TakeoverOptions, TakeoverPreview } from "../types";
import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { errText } from "../lib/err";
import { Modal } from "../components/Modal";
import { TextArea, TextInput } from "../components/ui/Input";
import { cn } from "../lib/cn";

export function CliPage() {
  const qc = useQueryClient();
  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  const settings = useQuery({ queryKey: ["app-settings"], queryFn: api.settingsGet });
  const preview = useQuery({ queryKey: ["cli-preview"], queryFn: api.cliPreviewAll });
  const backups = useQuery({ queryKey: ["cli-backups"], queryFn: () => api.cliBackups() });
  const apply = useMutation({
    mutationFn: ({ target, options }: { target: CliTarget; options: TakeoverOptions }) =>
      api.cliApply(target, options),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["cli-preview"] });
      qc.invalidateQueries({ queryKey: ["cli-backups"] });
    },
  });
  const restore = useMutation({
    mutationFn: (id: string) => api.cliRestore(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["cli-preview"] });
      qc.invalidateQueries({ queryKey: ["cli-backups"] });
    },
  });

  const [showBackups, setShowBackups] = useState(false);
  // Per-target model tier / extra env inputs, keyed by target.
  const [opts, setOpts] = useState<Record<CliTarget, TakeoverOptions>>({
    "claude-code": {},
    codex: {},
    "gemini-cli": {},
    "grok-build": {},
    opencode: {},
  });
  const [editing, setEditing] = useState<CliTarget | null>(null);

  const running = kernel.data?.state === "running";

  return (
    <div>
      <div className="flex items-center justify-between">
        <div>
          <h1 className="t-display">CLI 接管</h1>
          <p className="mt-1 text-sm text-muted">
            把各 CLI 的配置指到内核。写入前自动快照，可在「快照历史」回滚；
            不确定时先在设置里打开「沙箱写入」，改动只落到
            ~/.ccload-client/sandbox/，不碰真实配置。
          </p>
        </div>
        <button
          onClick={() => setShowBackups(!showBackups)}
          className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2"
        >
          {showBackups ? "← 返回接管" : "快照历史"}
        </button>
      </div>
      {settings.data?.sandbox_cli_writes && (
        <p className="mt-3 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs">
          沙箱已开：写入 ~/.ccload-client/sandbox，真实 CLI 配置不会被改。
        </p>
      )}
      {!settings.data?.client_api_token && (
        <p className="mt-3 text-sm text-amber-700">
          还没有客户端令牌。先到「令牌」页新建一个，创建时会自动记下。
        </p>
      )}

      {!showBackups && (
        <ul className="mt-6 space-y-3">
          {(preview.data ?? []).map((p) => (
            <CliCard
              key={p.target}
              p={p}
              disabled={!running || apply.isPending}
              pending={apply.isPending && apply.variables?.target === p.target}
              // Completion/error feedback belongs ON the card that triggered
              // it. Rendering it at the page bottom meant a successful write
              // to an already-active target looked like nothing happened —
              // the preview is identical before and after.
              result={
                apply.variables?.target === p.target
                  ? apply.isSuccess
                    ? { ok: true, text: `已写入 ${apply.data?.written.length ?? 0} 个文件` }
                    : apply.isError
                      ? { ok: false, text: errText(apply.error) }
                      : null
                  : null
              }
              options={opts[p.target]}
              onOptionsChange={(o) => setOpts({ ...opts, [p.target]: o })}
              onApply={() => apply.mutate({ target: p.target, options: opts[p.target] })}
              onEdit={() => setEditing(p.target)}
            />
          ))}
        </ul>
      )}

      {showBackups && (
        <div className="mt-6">
          <h2 className="t-title">快照历史</h2>
          <p className="mt-1 text-xs text-muted">
            每次接管前会自动快照。标记「原始」的是首次接管前的用户配置。
          </p>
          {backups.data && backups.data.length === 0 && (
            <p className="mt-4 text-sm text-muted">还没有任何快照。</p>
          )}
          <ul className="mt-4 space-y-2">
            {(backups.data ?? []).map((b) => (
              <BackupCard
                key={b.id}
                b={b}
                disabled={restore.isPending}
                onRestore={() => restore.mutate(b.id)}
              />
            ))}
          </ul>
        </div>
      )}

      {restore.isError && (
        <p className="mt-3 text-sm text-red-600">{errText(restore.error)}</p>
      )}

      {editing && (
        <ConfigEditor
          target={editing}
          onClose={() => setEditing(null)}
          onSaved={() => qc.invalidateQueries({ queryKey: ["cli-preview"] })}
        />
      )}
    </div>
  );
}

function CliCard({
  p,
  disabled,
  pending,
  result,
  options,
  onOptionsChange,
  onApply,
  onEdit,
}: {
  p: TakeoverPreview;
  disabled: boolean;
  pending: boolean;
  result: { ok: boolean; text: string } | null;
  options: TakeoverOptions;
  onOptionsChange: (o: TakeoverOptions) => void;
  onApply: () => void;
  onEdit: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const isClaude = p.target === "claude-code";
  const isCodex = p.target === "codex";

  return (
    <li className="card">
      <div className="flex items-center justify-between p-4">
        <div>
          <div className="font-medium">{p.label}</div>
          <div className="mt-1 font-mono text-[11px] text-muted">{p.path}</div>
          <div className="mt-1 text-xs text-muted">
            当前 {p.current_endpoint ?? "（未配置）"} → {p.next_endpoint}
          </div>
          {p.token_stale && (
            <div className="mt-1 text-xs text-amber-700">
              地址已指向内核，但令牌与当前内核不匹配 —— 调用会 401，请重新写入。
            </div>
          )}
          {result && (
            <div
              role="status"
              className={`mt-2 rounded-lg px-2.5 py-1.5 text-xs ${
                result.ok
                  ? "bg-emerald-50 text-emerald-700"
                  : "bg-red-50 text-red-700"
              }`}
            >
              {result.ok ? "✓ " : ""}
              {result.text}
            </div>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={onEdit}
            className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-xs hover:bg-surface-2"
          >
            编辑配置
          </button>
          {/* Never latch this off on `already_active`: re-writing is the fix
              for a hand-edited, half-migrated, or stale-token config, and
              locking the button is exactly the "覆盖不了" complaint. */}
          <button
            disabled={disabled}
            onClick={onApply}
            className={
              p.already_active
                ? "rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
                : "rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
            }
          >
            {pending ? "写入中…" : p.already_active ? "重新写入" : "写入"}
          </button>
        </div>
      </div>

      {(isClaude || isCodex) && (
        <div className="border-t border-border">
          <button
            onClick={() => setExpanded(!expanded)}
            className="flex w-full items-center gap-1 px-4 py-2 text-xs text-muted hover:text-content"
          >
            {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
            高级配置
          </button>
          {expanded && (
            <div className="px-4 pb-4">
              {isClaude && (
                <div className="mb-4 grid grid-cols-2 gap-3">
                  <ModelField
                    label="默认模型 (ANTHROPIC_MODEL)"
                    envKey="ANTHROPIC_MODEL"
                    target={p.target}
                    value={options.anthropic_model}
                    onChange={(v) => onOptionsChange({ ...options, anthropic_model: v })}
                  />
                  <ModelField
                    label="Sonnet tier"
                    envKey="ANTHROPIC_DEFAULT_SONNET_MODEL"
                    target={p.target}
                    value={options.sonnet_model}
                    onChange={(v) => onOptionsChange({ ...options, sonnet_model: v })}
                  />
                  <ModelField
                    label="Opus tier"
                    envKey="ANTHROPIC_DEFAULT_OPUS_MODEL"
                    target={p.target}
                    value={options.opus_model}
                    onChange={(v) => onOptionsChange({ ...options, opus_model: v })}
                  />
                  <ModelField
                    label="Haiku tier"
                    envKey="ANTHROPIC_DEFAULT_HAIKU_MODEL"
                    target={p.target}
                    value={options.haiku_model}
                    onChange={(v) => onOptionsChange({ ...options, haiku_model: v })}
                  />
                </div>
              )}
              <KnownKeysEditor
                target={p.target}
                options={options}
                onOptionsChange={onOptionsChange}
              />
            </div>
          )}
        </div>
      )}
    </li>
  );
}

/// The full knob list for a target, pre-filled from what the machine already
/// has (falling back to our suggested default). Replaces the old blank
/// KEY/value pair editor, where every knob had to be typed from memory.
///
/// Only rows the user actually touches are sent: writing every default back
/// would bloat the config with values the CLI already defaults to, and would
/// silently pin knobs the user never asked about.
function KnownKeysEditor({
  target,
  options,
  onOptionsChange,
}: {
  target: CliTarget;
  options: TakeoverOptions;
  onOptionsChange: (o: TakeoverOptions) => void;
}) {
  const keys = useQuery({
    queryKey: ["cli-env-keys", target],
    queryFn: () => api.cliEnvKeys(target),
  });
  const isCodex = target === "codex";
  const edits = options.extra_env ?? {};

  // Codex knobs are dedicated TakeoverOptions fields, not free-form env.
  const codexValue = (key: string): string | undefined => {
    if (key === "model") return options.codex_model;
    if (key === "model_reasoning_effort") return options.codex_reasoning_effort;
    if (key === "model_context_window") return options.codex_context_window?.toString();
    return edits[key];
  };
  const setCodex = (key: string, v: string) => {
    if (key === "model") return onOptionsChange({ ...options, codex_model: v });
    if (key === "model_reasoning_effort")
      return onOptionsChange({ ...options, codex_reasoning_effort: v });
    if (key === "model_context_window")
      return onOptionsChange({
        ...options,
        codex_context_window: v ? Number(v) : undefined,
      });
    onOptionsChange({ ...options, extra_env: { ...edits, [key]: v } });
  };

  const reset = (key: string) => {
    if (isCodex) return setCodex(key, "");
    const next = { ...edits };
    delete next[key];
    onOptionsChange({ ...options, extra_env: next });
  };

  if (!keys.data?.length) return null;
  // Tier keys render as dedicated inputs above; skip them here.
  const rows = keys.data.filter((k) => !TIER_KEYS.includes(k.key));

  return (
    <div>
      <div className="mb-2 text-xs text-muted">
        {isCodex ? "config.toml 配置项" : "环境变量"} · 已读取本机现值，留空的显示建议默认值；
        只有你改过的项才会写入
      </div>
      <div className="space-y-1">
        {rows.map((k) => {
          const edited = isCodex ? codexValue(k.key) : edits[k.key];
          const shown = edited ?? k.current ?? "";
          const isDirty = edited !== undefined && edited !== (k.current ?? "");
          return (
            <div key={k.key} className="flex items-center gap-2">
              <div className="w-[46%] shrink-0">
                <div className="font-mono text-[11px] text-content">{k.key}</div>
                <div className="text-[10px] text-muted">{k.description}</div>
              </div>
              <TextInput
                mono
                value={shown}
                placeholder={k.default || "未设置"}
                onChange={(e) =>
                  isCodex
                    ? setCodex(k.key, e.target.value)
                    : onOptionsChange({
                        ...options,
                        extra_env: { ...edits, [k.key]: e.target.value },
                      })
                }
                className={cn("flex-1", isDirty && "!border-accent")}
              />
              <button
                onClick={() => reset(k.key)}
                title={k.current ? "恢复为本机现值" : "清空"}
                className="rounded-md border border-border px-2 py-1 text-[10px] text-muted hover:bg-surface-2"
              >
                复原
              </button>
            </div>
          );
        })}
      </div>
      <CustomEnvRows
        known={keys.data.map((k) => k.key)}
        value={edits}
        onChange={(v) => onOptionsChange({ ...options, extra_env: v })}
      />
    </div>
  );
}

/// Anything outside the built-in list. The catalog covers the knobs we know
/// about, but CLIs ship new env vars faster than we can curate, so a free-form
/// escape hatch stays — it just no longer carries the whole burden.
function CustomEnvRows({
  known,
  value,
  onChange,
}: {
  known: string[];
  value: Record<string, string>;
  onChange: (v: Record<string, string>) => void;
}) {
  // Only keys the catalog does not already render, or they would show twice.
  const custom = Object.entries(value).filter(([k]) => !known.includes(k));

  const rename = (oldKey: string, newKey: string) => {
    const next: Record<string, string> = {};
    for (const [k, v] of Object.entries(value)) next[k === oldKey ? newKey : k] = v;
    onChange(next);
  };

  return (
    <div className="mt-3 border-t border-border pt-3">
      <div className="mb-2 text-xs text-muted">自定义环境变量（清单之外的任意 KEY）</div>
      <div className="space-y-1">
        {custom.map(([k, v], i) => (
          <div key={i} className="flex items-center gap-2">
            <TextInput
              mono
              value={k}
              onChange={(e) => rename(k, e.target.value)}
              placeholder="KEY"
              className="w-[46%] shrink-0"
            />
            <TextInput
              mono
              value={v}
              onChange={(e) => onChange({ ...value, [k]: e.target.value })}
              placeholder="value"
              className="flex-1"
            />
            <button
              onClick={() => {
                const next = { ...value };
                delete next[k];
                onChange(next);
              }}
              className="rounded-md border border-border px-2 py-1 text-[10px] text-red-600 hover:bg-surface-2"
            >
              删除
            </button>
          </div>
        ))}
      </div>
      <button
        onClick={() => onChange({ ...value, "": "" })}
        disabled={Object.keys(value).includes("")}
        className="mt-2 rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
      >
        + 添加变量
      </button>
    </div>
  );
}

/// The four Claude tier keys. They live in the backend catalog so their
/// on-disk values come back with everything else, but render as dedicated
/// inputs here — so the generic row list must skip them or they show twice.
const TIER_KEYS = [
  "ANTHROPIC_MODEL",
  "ANTHROPIC_DEFAULT_SONNET_MODEL",
  "ANTHROPIC_DEFAULT_OPUS_MODEL",
  "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

/// A tier input that echoes what the machine already has. `value` is the
/// pending edit (undefined = untouched); the current on-disk value shows
/// through when untouched, so the field is a view of reality rather than an
/// empty box the user has to retype from memory.
function ModelField({
  label,
  envKey,
  target,
  value,
  onChange,
}: {
  label: string;
  envKey: string;
  target: CliTarget;
  value: string | undefined;
  onChange: (v: string) => void;
}) {
  const keys = useQuery({
    queryKey: ["cli-env-keys", target],
    queryFn: () => api.cliEnvKeys(target),
  });
  const current = keys.data?.find((k) => k.key === envKey)?.current ?? null;
  const shown = value ?? current ?? "";
  const dirty = value !== undefined && value !== (current ?? "");

  return (
    <label className="block text-xs">
      <div className="text-muted">{label}</div>
      <TextInput
        value={shown}
        onChange={(e) => onChange(e.target.value)}
        placeholder="留空则不写入"
        className={cn("mt-1", dirty && "!border-accent")}
      />
    </label>
  );
}

function BackupCard({
  b,
  disabled,
  onRestore,
}: {
  b: BackupEntry;
  disabled: boolean;
  onRestore: () => void;
}) {
  const date = new Date(b.created_at * 1000).toLocaleString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
  const targetLabel =
    {
      "claude-code": "Claude Code",
      codex: "Codex",
      "gemini-cli": "Gemini CLI",
      "grok-build": "Grok Build",
      opencode: "OpenCode",
    }[b.target] ?? b.target;

  return (
    <li className="flex items-center justify-between card p-3">
      <div className="flex-1">
        <div className="flex items-center gap-2">
          <span className="font-medium text-sm">{targetLabel}</span>
          {b.pristine && (
            <span className="rounded-full bg-emerald-500/20 px-2 py-0.5 text-[10px] text-emerald-700">
              原始
            </span>
          )}
        </div>
        <div className="mt-1 text-xs text-muted">{date}</div>
        <div className="mt-1 text-xs text-muted">
          {b.files.filter((f) => f.existed).length} 个文件
        </div>
      </div>
      <button
        disabled={disabled}
        onClick={onRestore}
        className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-xs hover:bg-surface-2 disabled:opacity-40"
      >
        恢复
      </button>
    </li>
  );
}

function ConfigEditor({
  target,
  onClose,
  onSaved,
}: {
  target: CliTarget;
  onClose: () => void;
  onSaved: () => void;
}) {
  const files = useQuery({
    queryKey: ["cli-files", target],
    queryFn: () => api.cliReadFiles(target),
  });
  const save = useMutation({
    mutationFn: ({ rel, body }: { rel: string; body: string }) =>
      api.cliWriteFile(target, rel, body),
    onSuccess: () => {
      onSaved();
      onClose();
    },
  });

  const [selected, setSelected] = useState<ConfigFileView | null>(null);
  const [body, setBody] = useState("");

  useEffect(() => {
    if (files.data && files.data.length > 0 && !selected) {
      setSelected(files.data[0]);
      setBody(files.data[0].body);
    }
  }, [files.data, selected]);

  const targetLabel =
    {
      "claude-code": "Claude Code",
      codex: "Codex",
      "gemini-cli": "Gemini CLI",
      "grok-build": "Grok Build",
      opencode: "OpenCode",
    }[target] ?? target;

  return (
    <Modal onClose={onClose} className="max-w-3xl">
      <>
        <div className="flex items-center justify-between">
          <h2 className="t-title">{targetLabel} 配置编辑</h2>
          <button
            onClick={onClose}
            className="rounded-md border border-border px-2 py-1 text-sm hover:bg-surface-2"
          >
            关闭
          </button>
        </div>

        {files.data && files.data.length > 1 && (
          <div className="mt-3 flex gap-2 border-b border-border pb-2">
            {files.data.map((f) => (
              <button
                key={f.rel}
                onClick={() => {
                  setSelected(f);
                  setBody(f.body);
                }}
                className={`rounded-md px-2 py-1 text-xs ${
                  selected?.rel === f.rel
                    ? "bg-accent/20 text-content"
                    : "text-muted hover:text-content"
                }`}
              >
                {f.rel}
              </button>
            ))}
          </div>
        )}

        {selected && (
          <div className="mt-4">
            <div className="flex items-center justify-between text-xs text-muted">
              <span>{selected.path}</span>
              <span>{selected.format.toUpperCase()}</span>
            </div>
            <TextArea
              mono
              value={body}
              onChange={(e) => setBody(e.target.value)}
              rows={16}
              className="mt-2"
            />
            <div className="mt-3 flex items-center justify-between">
              <div className="text-xs text-muted">
                {selected.exists ? "文件已存在" : "文件将创建"}
              </div>
              <button
                disabled={save.isPending || body === selected.body}
                onClick={() => save.mutate({ rel: selected.rel, body })}
                className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
              >
                {save.isPending ? "保存中…" : "保存"}
              </button>
            </div>
            {save.isError && (
              <p className="mt-2 text-xs text-red-600">{errText(save.error)}</p>
            )}
          </div>
        )}
      </>
    </Modal>
  );
}
