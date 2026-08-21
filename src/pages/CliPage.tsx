import { useT } from "../i18n";
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
  const t = useT();
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
          <h1 className="t-display">{t("CLI 接管")}</h1>
          <p className="mt-1 text-sm text-muted">
            {t("把各 CLI 的配置指到内核。写入前自动快照，可在「快照历史」回滚；不确定时先在设置里打开「沙箱写入」，改动只落到 ~/.ccload-client/sandbox/，不碰真实配置。")}
          </p>
        </div>
        <button
          onClick={() => setShowBackups(!showBackups)}
          className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2"
        >
          {showBackups ? t("← 返回接管") : t("快照历史")}
        </button>
      </div>
      {settings.data?.sandbox_cli_writes && (
        <p className="mt-3 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs">
          {t("沙箱已开：写入 ~/.ccload-client/sandbox，真实 CLI 配置不会被改。")}
        </p>
      )}
      {!settings.data?.client_api_token && (
        <p className="mt-3 text-sm text-amber-700">
          {t("还没有客户端令牌。先到「令牌」页新建一个，创建时会自动记下。")}
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
          <h2 className="t-title">{t("快照历史")}</h2>
          <p className="mt-1 text-xs text-muted">
            {t("每次接管前会自动快照。标记「原始」的是首次接管前的用户配置。")}
          </p>
          {backups.data && backups.data.length === 0 && (
            <p className="mt-4 text-sm text-muted">{t("还没有任何快照。")}</p>
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
  const t = useT();
  const [expanded, setExpanded] = useState(false);
  const isClaude = p.target === "claude-code";

  return (
    <li className="card">
      <div className="flex items-center justify-between p-4">
        <div>
          <div className="font-medium">{p.label}</div>
          <div className="mt-1 font-mono text-[11px] text-muted">{p.path}</div>
          <div className="mt-1 text-xs text-muted">
            {t("当前")} {p.current_endpoint ?? t("（未配置）")} → {p.next_endpoint}
          </div>
          {p.token_stale && (
            <div className="mt-1 text-xs text-amber-700">
              {t("地址已指向内核，但令牌与当前内核不匹配 —— 调用会 401，请重新写入。")}
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
            {t("编辑配置")}
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
            {pending ? t("写入中…") : p.already_active ? t("重新写入") : t("写入")}
          </button>
        </div>
      </div>

      <div className="border-t border-border">
        <button
          onClick={() => setExpanded(!expanded)}
          className="flex w-full items-center gap-1 px-4 py-2 text-xs text-muted hover:text-content"
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
          {t("高级配置")}
        </button>
        {expanded && (
          <div className="px-4 pb-4">
              {isClaude && (
                <div className="mb-4 grid grid-cols-2 gap-3">
                  <ModelField
                    label={t("默认模型 (ANTHROPIC_MODEL)")}
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
                  <ModelField
                    label="Fable tier"
                    envKey="ANTHROPIC_DEFAULT_FABLE_MODEL"
                    target={p.target}
                    value={options.extra_env?.ANTHROPIC_DEFAULT_FABLE_MODEL}
                    onChange={(v) =>
                      onOptionsChange({
                        ...options,
                        extra_env: { ...options.extra_env, ANTHROPIC_DEFAULT_FABLE_MODEL: v },
                      })
                    }
                  />
                </div>
              )}
              {isClaude && (
                <FallbackFields options={options} onOptionsChange={onOptionsChange} />
              )}
            <KnownKeysEditor
              target={p.target}
              options={options}
              onOptionsChange={onOptionsChange}
            />
          </div>
        )}
      </div>
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
  const t = useT();
  const [filter, setFilter] = useState("");
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
  const q = filter.trim().toLowerCase();
  const rows = keys.data.filter((k) => {
    if (TIER_KEYS.includes(k.key)) return false;
    if (!q) return true;
    return (
      k.key.toLowerCase().includes(q) ||
      k.description.toLowerCase().includes(q)
    );
  });

  return (
    <div>
      <div className="mb-2 text-xs text-muted">
        {catalogHint(target, keys.data.filter((k) => !TIER_KEYS.includes(k.key)).length)}
      </div>
      {keys.data.length > 6 && (
        <TextInput
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder={t("筛选变量名或说明…")}
          className="mb-2"
        />
      )}
      <div className="max-h-[28rem] space-y-1 overflow-y-auto pr-1">
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
                placeholder={k.default || t("未设置")}
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
                title={k.current ? t("恢复为本机现值") : t("清空")}
                className="rounded-md border border-border px-2 py-1 text-[10px] text-muted hover:bg-surface-2"
              >
                {t("复原")}
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
  const t = useT();
  // Only keys the catalog does not already render, or they would show twice.
  const custom = Object.entries(value).filter(([k]) => !known.includes(k));

  const rename = (oldKey: string, newKey: string) => {
    const next: Record<string, string> = {};
    for (const [k, v] of Object.entries(value)) next[k === oldKey ? newKey : k] = v;
    onChange(next);
  };

  return (
    <div className="mt-3 border-t border-border pt-3">
      <div className="mb-2 text-xs text-muted">{t("清单之外的项（点路径或自定义 KEY）")}</div>
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
              {t("删除")}
            </button>
          </div>
        ))}
      </div>
      <button
        onClick={() => onChange({ ...value, "": "" })}
        disabled={Object.keys(value).includes("")}
        className="mt-2 rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
      >
        {t("+ 添加变量")}
      </button>
    </div>
  );
}

/// The four Claude tier keys. They live in the backend catalog so their
/// on-disk values come back with everything else, but render as dedicated
/// inputs here — so the generic row list must skip them or they show twice.
function catalogHint(target: CliTarget, n: number): string {
  switch (target) {
    case "claude-code":
      return `Claude Code 官方 env 全量（${n}）· 已读取本机现值；只有你改过的项才会写入`;
    case "codex":
      return `Codex 官方 config.toml 标量项（${n}）· 已读取本机现值；只有你改过的项才会写入`;
    case "gemini-cli":
      return `Gemini CLI 官方 settings（${n}）· 点路径写入 settings.json；只有你改过的项才会写入`;
    case "grok-build":
      return `Grok Build 官方 config.toml（${n}）· 已读取本机现值；只有你改过的项才会写入`;
    case "opencode":
      return `OpenCode 官方配置标量（${n}）· 已读取本机现值；只有你改过的项才会写入`;
  }
}

const TIER_KEYS = [
  "ANTHROPIC_MODEL",
  "ANTHROPIC_DEFAULT_SONNET_MODEL",
  "ANTHROPIC_DEFAULT_OPUS_MODEL",
  "ANTHROPIC_DEFAULT_HAIKU_MODEL",
  "ANTHROPIC_DEFAULT_FABLE_MODEL",
];

/// A tier input that echoes what the machine already has. `value` is the
/// pending edit (undefined = untouched); the current on-disk value shows
/// through when untouched, so the field is a view of reality rather than an
/// empty box the user has to retype from memory.
/// Claude Code 的 fallback 配置。
///
/// 这里要澄清一件几乎所有人都会搞错的事：Claude Code 有**两种**换模型的机制，
/// 长得像，但走的是完全不同的路。
///
///   1. 主力**过载/不可用** → 按 `fallbackModel` 数组依次换人。去重后最多 3 个。
///   2. 请求被**安全分类器标记** → Claude Code 自己跳到写死的 Opus 4.8 / Opus 5。
///      它**根本不看** `fallbackModel`。
///
/// 走 ccLoad 的人全都是「第三方供应商」，上游没有 `claude-opus-4-8` 这个名字，
/// 于是第 2 种每次都跳进一个不存在的模型 —— 这就是「总是自动跳到
/// claude-opus-4-8」的来历。官方文档给的解法只有一个：把
/// `ANTHROPIC_DEFAULT_OPUS_MODEL` 钉成你自己有的模型，所有有 fallback 的分类
/// 都会改跑它。所以这一块和上面的 Opus tier 是一对，必须放在一起说。
const MAX_FALLBACK = 3;

function FallbackFields({
  options,
  onOptionsChange,
}: {
  options: TakeoverOptions;
  onOptionsChange: (o: TakeoverOptions) => void;
}) {
  const t = useT();
  // 读回 settings.json 顶层的现值。`cliEnvKeys` 只覆盖 env，这两个键不在里面。
  const files = useQuery({
    queryKey: ["cli-files", "claude-code"],
    queryFn: () => api.cliReadFiles("claude-code"),
  });
  const current = (() => {
    const body = files.data?.find((f) => f.rel.endsWith("settings.json"))?.body;
    if (!body?.trim()) return { chain: [] as string[], switchOnFlag: undefined };
    try {
      const doc = JSON.parse(body) as {
        fallbackModel?: unknown;
        switchModelsOnFlag?: unknown;
      };
      // 官方是数组，但手写成字符串的配置在野外确实存在，两种都收。
      const raw = doc.fallbackModel;
      const chain = Array.isArray(raw)
        ? raw.filter((x): x is string => typeof x === "string")
        : typeof raw === "string"
          ? [raw]
          : [];
      return {
        chain,
        switchOnFlag:
          typeof doc.switchModelsOnFlag === "boolean" ? doc.switchModelsOnFlag : undefined,
      };
    } catch {
      // 配置编辑器允许写坏，但这一格不该因此变成错误提示 —— 它只是回显。
      return { chain: [] as string[], switchOnFlag: undefined };
    }
  })();

  const draft = options.fallback_models;
  const shown = draft ?? current.chain;
  const setSlot = (i: number, v: string) => {
    const next = [...shown];
    while (next.length <= i) next.push("");
    next[i] = v;
    onOptionsChange({ ...options, fallback_models: next });
  };

  const switchOnFlag = options.switch_models_on_flag ?? current.switchOnFlag ?? true;

  return (
    <div className="mb-4 rounded-xl border border-border bg-surface-2/40 p-3">
      <div className="text-xs font-medium">{t("强制 fallback 模型")}</div>
      <p className="mt-1 text-[11px] leading-relaxed text-muted">
        {t("主力")}<strong>{t("过载或不可用")}</strong>{t("时，按下面的顺序换人（写进 settings.json 顶层的")}{" "}
        <code>fallbackModel</code>{t("）。去重后最多 3 个，多余的 Claude Code 会忽略；填")} <code>default</code> {t("表示默认模型。留空则不写入。")}
      </p>
      <div className="mt-2 grid grid-cols-3 gap-2">
        {Array.from({ length: MAX_FALLBACK }, (_, i) => (
          <label key={i} className="block text-xs">
            <div className="text-muted">{t("第")} {i + 1} {t("顺位")}</div>
            <TextInput
              mono
              value={shown[i] ?? ""}
              onChange={(e) => setSlot(i, e.target.value)}
              placeholder={i === 0 ? "fable-5" : t("留空则不写入")}
              className={cn(
                "mt-1",
                draft !== undefined && (draft[i] ?? "") !== (current.chain[i] ?? "") &&
                  "!border-accent",
              )}
            />
          </label>
        ))}
      </div>

      {/* 用户真正抱怨的那个症状在这里解释。放在 fallbackModel 下面是有意的：
          他们会先在这一格里找，找不到就以为客户端没这个能力。 */}
      <p className="mt-3 rounded-lg border border-amber-500/40 bg-amber-500/10 px-2.5 py-2 text-[11px] leading-relaxed text-amber-900">
        <strong>{t("总是跳到 claude-opus-4-8？")}</strong>{t("那不是上面这条链干的。请求被 Claude Code 的安全分类器标记时，它会跳到")}<strong>{t("写死的")}</strong> {t("Opus 4.8 / Opus 5，完全不看")}{" "}
        <code>fallbackModel</code>{t("。而 ccLoad 上游没有")} <code>claude-opus-4-8</code> {t("这个名字，于是每次都跳进一个不存在的模型。唯一的改法是把上面的")}{" "}
        <strong>Opus tier（ANTHROPIC_DEFAULT_OPUS_MODEL）</strong>
        {t("钉成你自己有的模型 —— 设了它之后，所有有 fallback 的分类都改跑这一个。另外把 Fable tier 填上，Claude Code 才认得出当前模型是 Fable 5。")}
      </p>

      <label className="mt-2 flex items-center gap-2 text-[11px]">
        <input
          type="checkbox"
          checked={!switchOnFlag}
          onChange={(e) =>
            onOptionsChange({ ...options, switch_models_on_flag: !e.target.checked })
          }
          className="h-3.5 w-3.5"
        />
        <span>
          {t("被标记时先问一句，不自动换模型（")}<code>switchModelsOnFlag: false</code>）
        </span>
      </label>
    </div>
  );
}

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
  const t = useT();
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
        placeholder={t("留空则不写入")}
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
  const t = useT();
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
              {t("原始")}
            </span>
          )}
        </div>
        <div className="mt-1 text-xs text-muted">{date}</div>
        <div className="mt-1 text-xs text-muted">
          {b.files.filter((f) => f.existed).length} {t("个文件")}
        </div>
      </div>
      <button
        disabled={disabled}
        onClick={onRestore}
        className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-xs hover:bg-surface-2 disabled:opacity-40"
      >
        {t("恢复")}
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
  const t = useT();
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
          <h2 className="t-title">{targetLabel} {t("配置编辑")}</h2>
          <button
            onClick={onClose}
            className="rounded-md border border-border px-2 py-1 text-sm hover:bg-surface-2"
          >
            {t("关闭")}
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
                {selected.exists ? t("文件已存在") : t("文件将创建")}
              </div>
              <button
                disabled={save.isPending || body === selected.body}
                onClick={() => save.mutate({ rel: selected.rel, body })}
                className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
              >
                {save.isPending ? t("保存中…") : t("保存")}
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
