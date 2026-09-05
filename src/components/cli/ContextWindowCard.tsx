import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useT, type Translate } from "../../i18n";
import { api } from "../../lib/api";
import { formatWindow } from "../../lib/modelMeta";
import { TARGET_LABELS, TARGET_SHORT } from "../../lib/targets";
import { Select, TextInput } from "../ui/Input";
import type {
  ContextMode,
  ContextPolicy,
  TierRow,
  WindowCandidate,
  WindowSource,
  WindowVia,
} from "../../types";

/// 上下文窗口**总控** + 分档表。
///
/// 为什么要有这么一个东西：窗口在五家 CLI 里是五个不同的键（Claude 的
/// `CLAUDE_CODE_MAX_CONTEXT_TOKENS`、Codex 的 `model_context_window`、OpenCode
/// 的 `limit.context`、Grok 目录项的 context…），而 CLI 只在启动时读它们。内核把
/// 一条请求从 1M 的 claude 分流到 500k 的 grok 时，CLI 不知道、也没有键能中途改。
/// 所以这里按「这个别名所有可能落到的上游」取最窄写进去，压缩阈值按百分比跟着
/// 窗口走 —— 1M 时 900k 压缩、落到 grok 的 500k 时 450k 压缩，都是同一个 90%。
///
/// 名字推断不准的（本地 qwen、中转自起的别名）在分档表里手填，手填盖过一切。
/// 下面那张「每家会写成」表和真正写入走同一条解析路径，不是另抄一份规则。
const WINDOW_CHOICES: [string, number][] = [
  ["2M", 2_000_000],
  ["1M", 1_000_000],
  ["500k", 500_000],
  ["400k", 400_000],
  ["256k", 256_000],
  ["200k", 200_000],
  ["128k", 128_000],
  ["64k", 64_000],
  ["32k", 32_000],
];

function sourceLabel(t: Translate, s: WindowSource): string {
  switch (s) {
    case "manual":
      return t("手填");
    case "suffix":
      return t("模型名声明");
    case "catalog":
      return t("models.dev");
    default:
      return t("内置表");
  }
}

function viaLabel(t: Translate, v: WindowVia): string {
  switch (v) {
    case "pinned":
      return t("首选渠道");
    case "chain":
      return t("模型链");
    case "forced_route":
      return t("强制路由");
    case "kernel":
      return t("内核渠道");
    default:
      return t("模型本身");
  }
}

/// 「grok-4.6 · xAI-l · 模型链」这种一眼能认出来源的短描述。
function describeCandidate(t: Translate, c: WindowCandidate): string {
  const parts = [c.model];
  if (c.channel_name) parts.push(c.channel_name);
  if (c.via !== "model") parts.push(viaLabel(t, c.via));
  return parts.join(" · ");
}

/// 分档表的键：和后端 `tier_key` 一样去后缀、去厂商前缀、小写。用户填 `qwen3.8-27b`
/// 要能覆盖 `local/Qwen3.8-27B[1m]` 那一行，前端删旧键时也得按同一套认。
function tierKey(name: string): string {
  let s = name.trim();
  const slash = s.lastIndexOf("/");
  if (slash >= 0) s = s.slice(slash + 1);
  const open = s.lastIndexOf("[");
  if (open >= 0 && s.endsWith("]")) s = s.slice(0, open);
  return s.trim().toLowerCase();
}

/// 窗口选择：预设档位 + 「自定义」。当前值不在预设里时作为一项列出来，免得下拉
/// 显示成空白。
function WindowSelect({
  value,
  onChange,
  disabled,
}: {
  value: number;
  onChange: (n: number) => void;
  disabled?: boolean;
}) {
  const t = useT();
  const [custom, setCustom] = useState(false);
  const [draft, setDraft] = useState("");
  const known = WINDOW_CHOICES.some(([, n]) => n === value);
  if (custom) {
    return (
      <span className="inline-flex items-center gap-1">
        <TextInput
          small
          mono
          type="number"
          min={1}
          className="w-20"
          placeholder="200"
          value={draft}
          autoFocus
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              const n = Number(draft);
              if (n > 0) onChange(Math.round(n * 1000));
              setCustom(false);
            }
            if (e.key === "Escape") setCustom(false);
          }}
          onBlur={() => {
            const n = Number(draft);
            if (n > 0) onChange(Math.round(n * 1000));
            setCustom(false);
          }}
        />
        <span className="text-[10px] text-muted">k</span>
      </span>
    );
  }
  return (
    <Select
      small
      className="w-24"
      value={known ? value : value > 0 ? "current" : ""}
      disabled={disabled}
      onChange={(e) => {
        if (e.target.value === "custom") {
          setDraft(value > 0 ? String(Math.round(value / 1000)) : "");
          setCustom(true);
          return;
        }
        if (e.target.value === "current") return;
        onChange(Number(e.target.value));
      }}
    >
      {!known && value > 0 && <option value="current">{formatWindow(value)}</option>}
      {WINDOW_CHOICES.map(([label, n]) => (
        <option key={n} value={n}>
          {label}
        </option>
      ))}
      <option value="custom">{t("自定义…")}</option>
    </Select>
  );
}

export function ContextWindowCard() {
  const t = useT();
  const qc = useQueryClient();
  const policy = useQuery({ queryKey: ["context-policy"], queryFn: api.contextPolicyGet });
  const windows = useQuery({
    queryKey: ["context-window-preview"],
    queryFn: api.contextWindowPreview,
  });
  const tiers = useQuery({ queryKey: ["context-tiers"], queryFn: api.contextTiers });
  const [notice, setNotice] = useState<string | null>(null);
  const [showTiers, setShowTiers] = useState(false);
  const [newName, setNewName] = useState("");
  const [newWindow, setNewWindow] = useState(200_000);
  const save = useMutation({
    mutationFn: api.contextPolicySet,
    onSuccess: (stale) => {
      qc.invalidateQueries({ queryKey: ["context-policy"] });
      qc.invalidateQueries({ queryKey: ["context-window-preview"] });
      qc.invalidateQueries({ queryKey: ["context-tiers"] });
      // 存策略 ≠ 写 CLI。不说清楚的话用户改完开关就走了，配置纹丝未动。
      setNotice(
        stale > 0
          ? t("策略已保存。已接管的 CLI 要再点一次下面的「写入」才会跟上新窗口。")
          : t("策略已保存。"),
      );
    },
  });

  const p = policy.data;
  // 百分比输入用本地草稿：每敲一个数字就存一次会在 9 和 90 之间存出一个 9%。
  const [pct, setPct] = useState("");
  useEffect(() => {
    if (p) setPct(String(p.compact_percent));
  }, [p]);
  if (!p) return null;

  const set = (next: Partial<ContextPolicy>) => save.mutate({ ...p, ...next });
  const commitPct = () => {
    const n = Math.round(Number(pct));
    if (!(n >= 50 && n <= 95)) {
      setPct(String(p.compact_percent));
      return;
    }
    if (n !== p.compact_percent) set({ compact_percent: n });
  };
  const setOverride = (model: string, tokens: number | null) => {
    const key = tierKey(model);
    const overrides: Record<string, number> = {};
    for (const [k, v] of Object.entries(p.overrides)) {
      if (tierKey(k) !== key) overrides[k] = v;
    }
    if (tokens && tokens > 0) overrides[model.trim()] = tokens;
    set({ overrides });
  };

  const manualCount = Object.keys(p.overrides).length;
  const kernelMissing = p.mode === "auto" && windows.data?.some((w) => !w.kernel_checked);

  return (
    <div className="mt-4 rounded-xl border border-border bg-surface-2/40 px-3.5 py-3">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <span className="text-sm font-medium">{t("上下文窗口总控")}</span>
        <div className="flex items-center gap-3 text-xs">
          {(
            [
              ["auto", t("按落点自动")],
              ["fixed", t("固定")],
              ["off", t("不写入")],
            ] as [ContextMode, string][]
          ).map(([mode, label]) => (
            <label key={mode} className="flex cursor-pointer items-center gap-1.5">
              <input
                type="radio"
                name="ctx-mode"
                checked={p.mode === mode}
                disabled={save.isPending}
                onChange={() => set({ mode })}
              />
              {label}
            </label>
          ))}
        </div>
        {p.mode === "fixed" && (
          <WindowSelect
            value={p.fixed_tokens}
            disabled={save.isPending}
            onChange={(n) => set({ fixed_tokens: n })}
          />
        )}
        {p.mode === "auto" && (
          <label className="flex items-center gap-1.5 text-xs text-muted">
            {t("上限")}
            <Select
              small
              className="w-24"
              value={p.cap_tokens}
              disabled={save.isPending}
              onChange={(e) => set({ cap_tokens: Number(e.target.value) })}
            >
              <option value={0}>{t("不夹")}</option>
              {WINDOW_CHOICES.map(([label, n]) => (
                <option key={n} value={n}>
                  {label}
                </option>
              ))}
            </Select>
          </label>
        )}
        {p.mode !== "off" && (
          <label className="flex items-center gap-1.5 text-xs text-muted">
            {t("压缩触发")}
            <TextInput
              small
              mono
              type="number"
              min={50}
              max={95}
              className="w-14"
              value={pct}
              disabled={save.isPending}
              onChange={(e) => setPct(e.target.value)}
              onBlur={commitPct}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitPct();
              }}
            />
            <span>%</span>
          </label>
        )}
      </div>
      <p className="mt-1.5 text-xs text-muted">
        {p.mode === "auto"
          ? t("按这个 CLI 的模型在内核里所有可能落到的上游取最窄（模型本身、模型链、强制路由、内核渠道里服务它的每一条），压缩阈值按百分比跟着窗口走。CLI 只在启动时读窗口，分流到窄模型那一刻没法再改，所以事先按最窄写：1M 的 claude 链上挂着 500k 的 grok，就按 500k 跑、450k 压缩。名字推不准的模型在下面的分档表里手填。")
          : p.mode === "fixed"
            ? t("不管选了什么模型，五家 CLI 一律写这个数，压缩阈值按百分比算。")
            : t("一个字都不写，各 CLI 保持现状。")}
      </p>
      {kernelMissing && (
        <p className="mt-1.5 text-xs text-amber-700">
          {t("内核没连上，下面只按本地的模型链和强制路由算，可能偏宽；内核起来后会自动重算。")}
        </p>
      )}
      {windows.data && p.mode !== "off" && (
        <div className="mt-2 space-y-1 text-[11px] text-muted">
          {windows.data.map((w) => {
            // 磁盘上的数和将要写的不一致 = 还没写入，或者就是那个存量旧值。
            // 直接标出来，省得用户对着两个数字猜哪个是真的。
            const drift = w.tokens !== null && w.on_disk !== null && w.on_disk !== w.tokens;
            const others = w.candidates.filter((c) => c.via !== "model");
            return (
              <div key={w.target}>
                <div className="flex flex-wrap items-baseline gap-x-1.5">
                  <span className="min-w-[5.5rem]">{TARGET_LABELS[w.target]}</span>
                  <span className={w.tokens ? "font-mono text-accent" : "font-mono"}>
                    {w.tokens ? formatWindow(w.tokens) : t("不写")}
                  </span>
                  {w.compact_tokens !== null && w.compact_tokens > 0 && (
                    <span className="opacity-70">
                      {t("压缩 {n}", { n: formatWindow(w.compact_tokens) })}
                    </span>
                  )}
                  {w.narrowest && w.narrowest.via !== "model" && (
                    <span className="opacity-70">
                      {t("最窄：{what}", { what: describeCandidate(t, w.narrowest) })}
                    </span>
                  )}
                  {w.narrowest && w.narrowest.via === "model" && (
                    <span className="opacity-60">{sourceLabel(t, w.narrowest.source)}</span>
                  )}
                  {w.capped && <span className="opacity-60">{t("已按上限夹")}</span>}
                  {w.model && <span className="opacity-50">· {w.model}</span>}
                  {drift && (
                    <span className="text-amber-700">
                      {t("磁盘上还是 {n}，点「写入」更新", { n: formatWindow(w.on_disk ?? 0) })}
                    </span>
                  )}
                </div>
                {others.length > 0 && (
                  <div className="ml-[5.5rem] flex flex-wrap gap-x-2 opacity-60">
                    <span>{t("落点")}</span>
                    {others.map((c) => (
                      <span key={`${c.model}:${c.channel_name ?? ""}`} className="font-mono">
                        {c.model}
                        {c.channel_name ? `@${c.channel_name}` : ""} {formatWindow(c.window)}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
      {notice && <p className="mt-1.5 text-xs text-amber-700">{notice}</p>}

      {p.mode !== "off" && (
        <div className="mt-2 border-t border-border/60 pt-2">
          <button
            type="button"
            onClick={() => setShowTiers(!showTiers)}
            className="flex items-center gap-1 text-xs text-muted hover:text-content"
          >
            {showTiers ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
            {t("分档表")}
            {manualCount > 0 && (
              <span className="opacity-70">{t("（{n} 条手填）", { n: manualCount })}</span>
            )}
          </button>
          {showTiers && (
            <div className="mt-2">
              <p className="text-[11px] text-muted">
                {t("每个可能用到的模型一行。改窗口就是手填，盖过模型名后缀、models.dev 和内置表；本地模型、中转自起的别名靠这里给真实上限。压缩触发按上面的百分比算。")}
              </p>
              <table className="mt-1.5 w-full text-[11px]">
                <thead className="text-left text-muted">
                  <tr>
                    <th className="py-1 pr-2 font-normal">{t("模型")}</th>
                    <th className="py-1 pr-2 font-normal">{t("窗口")}</th>
                    <th className="py-1 pr-2 font-normal">{t("来源")}</th>
                    <th className="py-1 pr-2 font-normal">{t("压缩触发")}</th>
                    <th className="py-1 pr-2 font-normal">{t("用到它的 CLI")}</th>
                    <th className="py-1 font-normal" />
                  </tr>
                </thead>
                <tbody>
                  {(tiers.data ?? []).map((row) => (
                    <TierLine
                      key={row.model}
                      row={row}
                      disabled={save.isPending}
                      onWindow={(n) => setOverride(row.model, n)}
                      onReset={() => setOverride(row.model, null)}
                    />
                  ))}
                  <tr className="border-t border-border/60">
                    <td className="py-1.5 pr-2">
                      <TextInput
                        small
                        mono
                        className="w-48"
                        placeholder={t("模型名，如 qwen3.8-27b")}
                        value={newName}
                        onChange={(e) => setNewName(e.target.value)}
                      />
                    </td>
                    <td className="py-1.5 pr-2">
                      <WindowSelect value={newWindow} onChange={setNewWindow} />
                    </td>
                    <td className="py-1.5 pr-2 text-muted" colSpan={3}>
                      {t("手填")}
                    </td>
                    <td className="py-1.5">
                      <button
                        type="button"
                        disabled={save.isPending || !newName.trim() || newWindow <= 0}
                        onClick={() => {
                          setOverride(newName, newWindow);
                          setNewName("");
                        }}
                        className="rounded border border-border px-2 py-0.5 text-[11px] hover:bg-surface-2 disabled:opacity-40"
                      >
                        {t("添加")}
                      </button>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function TierLine({
  row,
  disabled,
  onWindow,
  onReset,
}: {
  row: TierRow;
  disabled: boolean;
  onWindow: (n: number) => void;
  onReset: () => void;
}) {
  const t = useT();
  return (
    <tr className="border-t border-border/40">
      <td className="py-1 pr-2 font-mono">{row.model}</td>
      <td className="py-1 pr-2">
        <WindowSelect value={row.window} disabled={disabled} onChange={onWindow} />
      </td>
      <td className="py-1 pr-2 text-muted">
        {sourceLabel(t, row.source)}
        {row.source === "manual" && row.auto_window !== row.window && (
          <span className="opacity-60">
            {" "}
            {t("（自动 {n}）", { n: formatWindow(row.auto_window) })}
          </span>
        )}
      </td>
      <td className="py-1 pr-2 font-mono text-muted">{formatWindow(row.compact_tokens)}</td>
      <td className="py-1 pr-2 text-muted">
        {row.used_by.length === 0 ? "—" : row.used_by.map((x) => TARGET_SHORT[x]).join(" · ")}
      </td>
      <td className="py-1 text-right">
        {row.source === "manual" && (
          <button
            type="button"
            disabled={disabled}
            onClick={onReset}
            className="rounded border border-border px-1.5 py-0 text-[10px] text-muted hover:bg-surface-2 hover:text-content disabled:opacity-40"
          >
            {t("恢复自动")}
          </button>
        )}
      </td>
    </tr>
  );
}
