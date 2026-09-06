import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { RotateCcw, ShieldAlert, Trash2 } from "lucide-react";
import { api } from "../lib/api";
import { errText } from "../lib/err";
import { useT } from "../i18n";
import { displayModel } from "../lib/pins";
import { ALL_TARGETS, TARGET_LABELS } from "../lib/targets";
import { fmtClock } from "../components/formatters";
import { Select, TextArea, TextInput } from "../components/ui/Input";
import type { CliTarget, InjectConfig, InjectRule, RescueConfig } from "../types";

/// 自动插件：壳体不依赖内核、由本地代理在转发路上自己完成的自动化能力。
/// tab 往下加，每条一个开关；第一条是自动破甲（拒绝接管），机制见
/// src-tauri/src/services/rescue.rs 的模块注释。

const KIND_TABS = [
  {
    id: "rescue",
    label: "自动破甲",
    hint: "检测到开场拒绝就丢弃原回答，追加破甲提示词重发。",
  },
  {
    id: "inject",
    label: "动态注入",
    hint: "命中条件就把一段文本追加进请求的 system 层 —— 对已经开着的会话也生效。",
  },
] as const;

type Kind = (typeof KIND_TABS)[number]["id"];

export function AutomationPage() {
  const t = useT();
  const [kind, setKind] = useState<Kind>("rescue");

  return (
    <div>
      <div>
        <h1 className="t-display">{t("自动插件")}</h1>
        <p className="mt-1 max-w-3xl text-sm text-muted">
          {t("壳体不依赖内核就能完成的自动化能力，逐个开关。")}
        </p>
      </div>

      <div className="mt-5 flex flex-wrap items-center gap-2">
        {KIND_TABS.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setKind(tab.id)}
            aria-current={kind === tab.id ? "page" : undefined}
            title={t(tab.hint)}
            className={
              kind === tab.id
                ? "rounded-lg bg-accent/10 px-3.5 py-1.5 text-sm font-medium text-accent"
                : "rounded-lg border border-border bg-surface-raised px-3.5 py-1.5 text-sm text-muted hover:bg-surface-2"
            }
          >
            {t(tab.label)}
          </button>
        ))}
      </div>
      <p className="mt-2 text-xs text-muted">
        {t(KIND_TABS.find((x) => x.id === kind)?.hint ?? "")}
      </p>

      <div className="mt-4">
        {kind === "rescue" && (
          <div className="space-y-4">
            <RescuePanel />
            <RescueHits />
          </div>
        )}
        {kind === "inject" && <InjectPanel />}
      </div>
    </div>
  );
}

function RescuePanel() {
  const t = useT();
  const qc = useQueryClient();
  const [draft, setDraft] = useState<RescueConfig | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const cfg = useQuery({ queryKey: ["rescue-cfg"], queryFn: api.rescueGet });

  useEffect(() => {
    if (cfg.data) setDraft(cfg.data);
  }, [cfg.data]);

  const save = useMutation({
    mutationFn: (c: RescueConfig) => api.rescueSet(c),
    onSuccess: (saved) => {
      qc.setQueryData(["rescue-cfg"], saved);
      setDraft(saved);
      setMessage(t("已保存"));
    },
    onError: (e) => setMessage(errText(e)),
  });

  // 恢复默认只回填编辑框，不落盘 —— 用户还得自己按保存，反悔有得反悔。
  const restoreDefault = async () => {
    if (!draft) return;
    try {
      const prompt = await api.rescueDefaultPrompt();
      setDraft({ ...draft, armor_prompt: prompt });
    } catch (e) {
      setMessage(errText(e));
    }
  };

  if (!draft) {
    return cfg.isLoading ? (
      <p className="text-sm text-muted">{t("读取中…")}</p>
    ) : (
      <p className="text-sm text-muted">{errText(cfg.error)}</p>
    );
  }

  return (
    <div className="card max-w-3xl space-y-4 p-4">
      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={draft.enabled}
          onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })}
        />
        {t("开启自动破甲")}
      </label>
      <p className="-mt-2 pl-6 text-xs leading-relaxed text-muted">
        {draft.enabled
          ? t(
              "聊天响应会被整体缓冲下来判断是不是拒绝，不再流式 —— 首字延迟变大，长回答尤其明显。命中就丢弃原回答，按下面这条提示词续写一轮重发。",
            )
          : t("关着时一切原样直通，代理不读响应内容。")}
      </p>

      <div className="flex items-center gap-3 text-xs">
        <span className="text-muted">{t("单次请求最多重发")}</span>
        <Select
          small
          className="w-20"
          value={String(draft.max_retries)}
          onChange={(e) =>
            setDraft({ ...draft, max_retries: Number(e.target.value) })
          }
        >
          {[1, 2, 3, 4, 5].map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </Select>
      </div>

      <label className="block text-xs">
        <span className="mb-1 block text-muted">
          {t("自定义拒绝标记（一行一个，命中即一票判为拒绝）")}
        </span>
        <TextArea
          rows={4}
          value={draft.markers.join("\n")}
          onChange={(e) =>
            setDraft({
              ...draft,
              markers: e.target.value.split("\n"),
            })
          }
          placeholder={t("每行一条，例如：我无法协助")}
        />
      </label>

      <label className="block text-xs">
        <span className="mb-1 flex items-center justify-between">
          <span className="text-muted">{t("破甲提示词（留空用内置默认）")}</span>
          <button
            type="button"
            onClick={() => void restoreDefault()}
            title={t("把内置默认提示词填进来")}
            className="flex items-center gap-1 text-muted hover:text-accent"
          >
            <RotateCcw className="h-3 w-3" /> {t("恢复默认")}
          </button>
        </span>
        <TextArea
          rows={10}
          value={draft.armor_prompt}
          onChange={(e) => setDraft({ ...draft, armor_prompt: e.target.value })}
        />
      </label>

      <div className="flex items-center gap-3">
        <button
          onClick={() => save.mutate(draft)}
          disabled={save.isPending}
          className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
        >
          {save.isPending ? t("保存中…") : t("保存")}
        </button>
        {message && <p className="text-sm text-accent">{message}</p>}
      </div>
    </div>
  );
}

/// 命中记录：代理记录里 rescued > 0 的那部分。代理侧是环形缓冲（全量滚动、
/// 上限 MAX_RECORDS），命中条目会随流量被挤掉 —— 这里只显示「当前还留着」的。
function RescueHits() {  const t = useT();
  const hits = useQuery({
    queryKey: ["rescue-hits"],
    queryFn: api.cliProxyRecords,
    select: (rs) => rs.filter((r) => r.rescued > 0),
    refetchInterval: 8000,
  });

  return (
    <div className="card max-w-3xl p-4">
      <div className="flex items-center gap-2 text-sm font-medium">
        <ShieldAlert className="h-4 w-4 text-accent" />
        {t("命中记录")}
      </div>
      <p className="mt-1 text-xs leading-relaxed text-muted">
        {t(
          "原回答被判成拒绝丢弃、追加破甲提示词重发的请求。记录随代理日志环形滚动，只显示还留着的。",
        )}
      </p>
      <ul className="mt-3 space-y-1.5">
        {hits.data && hits.data.length === 0 && (
          <li className="rounded-lg border border-dashed border-border px-3 py-3 text-center text-xs text-muted">
            {t("还没有命中记录。")}
          </li>
        )}
        {(hits.data ?? []).map((r, i) => (
          <li
            key={`${r.time}-${i}`}
            className="flex flex-wrap items-center gap-x-2 gap-y-1 rounded-lg border border-border px-2.5 py-1.5 text-xs"
          >
            <span className="font-mono text-muted">{fmtClock(r.time)}</span>
            <span className="font-medium">{TARGET_LABELS[r.cli as CliTarget] ?? r.cli}</span>
            <span className="font-mono">{displayModel(r.model ?? r.sent_model) ?? "—"}</span>
            {r.sent_model && r.sent_model !== r.model && (
              <span className="text-muted">→ {displayModel(r.sent_model)}</span>
            )}
            <span
              className={
                r.status >= 400
                  ? "rounded bg-red-500/10 px-1.5 py-0.5 font-mono text-red-600"
                  : "rounded bg-surface-2 px-1.5 py-0.5 font-mono text-muted"
              }
            >
              {r.status}
            </span>
            <span className="ml-auto rounded bg-accent/10 px-1.5 py-0.5 text-accent">
              {t("{n} 次重发", { n: r.rescued })}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/// 动态注入面板：规则列表整体编辑、保存时一把交给后端归一化。
function InjectPanel() {
  const t = useT();
  const qc = useQueryClient();
  const [draft, setDraft] = useState<InjectConfig | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const cfg = useQuery({
    queryKey: ["dynamic-inject"],
    queryFn: api.dynamicInjectGet,
  });
  useEffect(() => {
    if (cfg.data) setDraft(cfg.data);
  }, [cfg.data]);

  const save = useMutation({
    mutationFn: (c: InjectConfig) => api.dynamicInjectSet(c),
    onSuccess: (saved) => {
      qc.setQueryData(["dynamic-inject"], saved);
      setDraft(saved);
      setMessage(t("已保存"));
    },
    onError: (e) => setMessage(errText(e)),
  });

  if (!draft) {
    return cfg.isLoading ? (
      <p className="text-sm text-muted">{t("读取中…")}</p>
    ) : (
      <p className="text-sm text-muted">{errText(cfg.error)}</p>
    );
  }

  const setRule = (i: number, patch: Partial<InjectRule>) => {
    const rules = [...draft.rules];
    rules[i] = { ...rules[i], ...patch };
    setDraft({ ...draft, rules });
  };

  return (
    <div className="max-w-3xl space-y-4">
      <div className="card space-y-2 p-4">
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={draft.enabled}
            onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })}
          />
          {t("开启动态注入")}
        </label>
        <p className="pl-6 text-xs leading-relaxed text-muted">
          {draft.enabled
            ? t(
                "命中的聊天请求在转发前会把文本追加进 system 层末尾。内容和位置逐请求一致，prompt cache 前缀不会被打散。",
              )
            : t("关着时请求原样直通。")}
        </p>
      </div>

      {draft.rules.length === 0 && (
        <p className="rounded-lg border border-dashed border-border px-3 py-4 text-center text-xs text-muted">
          {t("还没有规则。点「+ 加一条规则」写第一条。")}
        </p>
      )}

      {draft.rules.map((rule, i) => (
        <div key={rule.id || i} className="card space-y-3 p-4">
          <div className="flex items-center gap-2">
            <TextInput
              className="flex-1"
              value={rule.name}
              onChange={(e) => setRule(i, { name: e.target.value })}
              placeholder={t("规则名称")}
              aria-label={t("规则名称")}
            />
            <label className="flex shrink-0 items-center gap-1.5 text-xs text-muted">
              <input
                type="checkbox"
                checked={rule.enabled}
                onChange={(e) => setRule(i, { enabled: e.target.checked })}
              />
              {t("启用")}
            </label>
            <button
              type="button"
              onClick={() =>
                setDraft({ ...draft, rules: draft.rules.filter((_, j) => j !== i) })
              }
              aria-label={t("删这条规则")}
              title={t("删这条规则")}
              className="shrink-0 text-muted hover:text-red-600"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          </div>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <label className="block text-xs">
              <span className="mb-1 block text-muted">
                {t("仅对以下 CLI 生效（留空 = 任意）")}
              </span>
              <Select
                small
                value={rule.cli}
                onChange={(e) => setRule(i, { cli: e.target.value })}
                aria-label={t("匹配 CLI")}
              >
                <option value="">{t("任意 CLI")}</option>
                {ALL_TARGETS.map((id) => (
                  <option key={id} value={id}>
                    {TARGET_LABELS[id]}
                  </option>
                ))}
              </Select>
            </label>
            <label className="block text-xs">
              <span className="mb-1 block text-muted">
                {t("仅对以下模型生效（留空 = 任意）")}
              </span>
              <TextInput
                mono
                value={rule.model}
                onChange={(e) => setRule(i, { model: e.target.value })}
                placeholder={t("例如 claude-opus-5")}
                aria-label={t("匹配模型")}
              />
            </label>
          </div>
          <label className="block text-xs">
            <span className="mb-1 block text-muted">
              {t("追加进 system 层的文本")}
            </span>
            <TextArea
              rows={5}
              value={rule.text}
              onChange={(e) => setRule(i, { text: e.target.value })}
            />
          </label>
        </div>
      ))}

      <button
        type="button"
        onClick={() =>
          setDraft({
            ...draft,
            rules: [
              ...draft.rules,
              {
                id: crypto.randomUUID(),
                name: "",
                enabled: true,
                cli: "",
                model: "",
                text: "",
              },
            ],
          })
        }
        className="text-xs text-accent hover:underline"
      >
        {t("+ 加一条规则")}
      </button>

      <div className="flex items-center gap-3">
        <button
          onClick={() => save.mutate(draft)}
          disabled={save.isPending}
          className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
        >
          {save.isPending ? t("保存中…") : t("保存")}
        </button>
        {message && <p className="text-sm text-accent">{message}</p>}
      </div>
    </div>
  );
}
