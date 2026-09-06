import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { RotateCcw } from "lucide-react";
import { api } from "../lib/api";
import { errText } from "../lib/err";
import { useT } from "../i18n";
import { Select, TextArea } from "../components/ui/Input";
import type { RescueConfig } from "../types";

/// 自动插件：壳体不依赖内核、由本地代理在转发路上自己完成的自动化能力。
/// tab 往下加，每条一个开关；第一条是自动破甲（拒绝接管），机制见
/// src-tauri/src/services/rescue.rs 的模块注释。

const KIND_TABS = [
  {
    id: "rescue",
    label: "自动破甲",
    hint: "检测到开场拒绝就丢弃原回答，追加破甲提示词重发。",
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

      <div className="mt-4">{kind === "rescue" && <RescuePanel />}</div>
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
