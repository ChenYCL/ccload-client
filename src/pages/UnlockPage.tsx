import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Copy, FolderOpen, Plus, Trash2, Unlock } from "lucide-react";
import { api } from "../lib/api";
import { errText } from "../lib/err";
import { useT } from "../i18n";
import { Modal } from "../components/Modal";
import { CopyButton } from "../components/ui/CopyButton";
import { Select, TextArea, TextInput } from "../components/ui/Input";
import { ALL_TARGETS, TARGET_LABELS } from "../lib/targets";
import type { CliTarget, SessionPreset, SessionTurn, SpawnResult } from "../types";

/// 会话预设 / 破禁。
///
/// 对白共用，落盘按 CLI 各写各的，然后用那一家自己的 resume 接着干。
/// 拦不拦得住取决于对面那家模型。

function blankPreset(): SessionPreset {
  return {
    id: "",
    title: "",
    summary: "",
    builtin: false,
    turns: [
      { role: "user", text: "" },
      { role: "assistant", text: "" },
    ],
  };
}

export function UnlockPage() {
  const t = useT();
  const qc = useQueryClient();
  const [cwd, setCwd] = useState("");
  const [extra, setExtra] = useState("");
  const [launch, setLaunch] = useState(true);
  // 默认锁住。不锁的那一档会写出一个开局就没有任何文件访问关卡的会话，而
  // Claude Code 在 cwd 位于某个 git 仓库子目录时项目根会落到仓库根上 ——
  // 两件事一叠，选了 repo/src 实际摸得到的是整个 repo。默认值得是安全的那个。
  const [confine, setConfine] = useState(true);
  const [targets, setTargets] = useState<CliTarget[]>([...ALL_TARGETS]);
  const [editing, setEditing] = useState<SessionPreset | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [result, setResult] = useState<SpawnResult | null>(null);

  const presets = useQuery({ queryKey: ["presets"], queryFn: api.presetList });
  const prefs = useQuery({ queryKey: ["presets-prefs"], queryFn: api.presetPrefs });

  useEffect(() => {
    if (!cwd && prefs.data?.last_cwd) setCwd(prefs.data.last_cwd);
    if (prefs.data?.last_targets?.length) setTargets(prefs.data.last_targets);
  }, [cwd, prefs.data]);

  const save = useMutation({
    mutationFn: api.presetSave,
    onSuccess: (list) => {
      qc.setQueryData(["presets"], list);
      setEditing(null);
      setMessage(t("已保存"));
    },
    onError: (e) => setMessage(errText(e)),
  });

  const remove = useMutation({
    mutationFn: api.presetDelete,
    onSuccess: (list) => {
      qc.setQueryData(["presets"], list);
      setMessage(t("已删除"));
    },
    onError: (e) => setMessage(errText(e)),
  });

  const spawn = useMutation({
    mutationFn: (id: string) => api.presetSpawn(id, cwd, extra, launch, targets, confine),
    onSuccess: (r) => {
      setResult(r);
      setMessage(null);
      qc.invalidateQueries({ queryKey: ["sessions"] });
      qc.invalidateQueries({ queryKey: ["presets-prefs"] });
    },
    onError: (e) => {
      setResult(null);
      setMessage(errText(e));
    },
  });

  const pick = async () => {
    const p = await api.pickFolder();
    if (p) setCwd(p);
  };

  const clone = (p: SessionPreset) => {
    setEditing({
      ...p,
      id: "",
      title: t("{title}（副本）", { title: p.title }),
      builtin: false,
    });
  };

  return (
    <div>
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="t-display">{t("破禁")}</h1>
          <p className="mt-1 max-w-3xl text-sm text-muted">
            {t(
              "点一下按勾选的 CLI 各写一份已经带好背景的新会话，然后用那一家自己的 resume 接着干。内置几份公开的破禁预设；你也可以自己写一轮对白。拦不拦得住取决于对面那家模型，壳体只负责把历史写进文件。",
            )}
          </p>
        </div>
        <button
          onClick={() => setEditing(blankPreset())}
          className="flex shrink-0 items-center gap-1 rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90"
        >
          <Plus className="h-4 w-4" /> {t("新建预设")}
        </button>
      </div>

      <div className="mt-5 card space-y-3 p-4">
        <div className="text-xs">
          <span className="mb-1 block text-muted">{t("工作目录")}</span>
          <span className="flex gap-2">
            <TextInput
              mono
              className="flex-1"
              value={cwd}
              onChange={(e) => setCwd(e.target.value)}
              placeholder={t("要打开的那个仓库路径")}
              aria-label={t("工作目录")}
            />
            <button
              type="button"
              onClick={() => void pick()}
              className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2"
            >
              <FolderOpen className="h-4 w-4" /> {t("选目录")}
            </button>
          </span>
        </div>
        <label className="block text-xs">
          <span className="mb-1 block text-muted">{t("写在末尾的第一条任务（可选）")}</span>
          <TextArea
            rows={3}
            value={extra}
            onChange={(e) => setExtra(e.target.value)}
            placeholder={t("会作为最后一条用户消息追加。留空就只写入预设对白。")}
          />
        </label>
        <div className="text-xs">
          <span className="mb-1 block text-muted">{t("写给哪些 CLI")}</span>
          <span className="flex flex-wrap gap-1.5">
            {ALL_TARGETS.map((id) => {
              const on = targets.includes(id);
              return (
                <button
                  key={id}
                  type="button"
                  onClick={() =>
                    setTargets((cur) =>
                      on ? cur.filter((x) => x !== id) : [...cur, id],
                    )
                  }
                  className={
                    on
                      ? "rounded-lg border border-accent bg-accent/10 px-2 py-1 text-xs text-accent"
                      : "rounded-lg border border-border px-2 py-1 text-xs text-muted hover:bg-surface-2"
                  }
                >
                  {TARGET_LABELS[id]}
                </button>
              );
            })}
          </span>
        </div>
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={confine}
            onChange={(e) => setConfine(e.target.checked)}
          />
          {t("锁定在这个目录")}
        </label>
        <p className="-mt-1 pl-6 text-xs leading-relaxed text-muted">
          {confine
            ? t(
                "会话不会被预先解除文件访问的关卡：越出上面这个目录的读写要你当场点头。Codex 还会额外钉死工作根、只给工作区写权限。",
              )
            : t(
                "关掉之后写出的会话开局就没有任何文件访问关卡，读写整块磁盘都不会问你一声。而 CLI 的项目根在目录处于某个 git 仓库子目录时会落到仓库根上 —— 选了 repo/src，实际摸得到的是整个 repo。",
              )}
        </p>
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={launch}
            onChange={(e) => setLaunch(e.target.checked)}
          />
          {t("写完后拉起终端跑 resume")}
        </label>
      </div>

      <ul className="mt-4 space-y-3">
        {(presets.data ?? []).map((preset) => (
          <li key={preset.id} className="card p-4">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-medium">{preset.title}</span>
                  {preset.builtin && (
                    <span className="rounded bg-surface-2 px-1.5 py-0.5 text-[10px] text-muted">
                      {t("内置")}
                    </span>
                  )}
                  <span className="text-[11px] text-muted">
                    {t("{n} 轮", { n: preset.turns.length })}
                  </span>
                </div>
                <p className="mt-1 text-xs leading-relaxed text-muted">{preset.summary}</p>
              </div>
              <div className="flex shrink-0 flex-wrap items-center gap-1.5">
                <button
                  onClick={() => spawn.mutate(preset.id)}
                  disabled={spawn.isPending || !cwd.trim() || targets.length === 0}
                  title={
                    !cwd.trim()
                      ? t("先选一个工作目录")
                      : targets.length === 0
                        ? t("至少勾选一个 CLI")
                        : undefined
                  }
                  className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
                >
                  <Unlock className="h-3 w-3" />
                  {spawn.isPending ? t("写入中…") : t("开一个新会话")}
                </button>
                <button
                  onClick={() => clone(preset)}
                  className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
                >
                  <Copy className="h-3 w-3" /> {t("复制一份")}
                </button>
                {!preset.builtin && (
                  <>
                    <button
                      onClick={() => setEditing(preset)}
                      className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
                    >
                      {t("编辑")}
                    </button>
                    <button
                      onClick={() => remove.mutate(preset.id)}
                      className="rounded-lg border border-border px-2 py-1 text-xs text-red-600 hover:bg-red-50"
                    >
                      <Trash2 className="h-3 w-3" />
                    </button>
                  </>
                )}
              </div>
            </div>
          </li>
        ))}
      </ul>

      {result && (
        <div className="mt-4 card space-y-3 p-4">
          <div className="text-sm font-medium">{t("已经写好")}</div>
          {result.items.map((it) => (
            <div key={it.target} className="space-y-1">
              <div className="text-xs font-medium">{TARGET_LABELS[it.target]}</div>
              <p className="font-mono text-[11px] text-muted">{it.path}</p>
              <div className="flex items-start gap-2">
                <code className="min-w-0 flex-1 whitespace-pre-wrap break-all rounded-lg bg-surface-2 px-2 py-1.5 font-mono text-[11px]">
                  {it.command}
                </code>
                <CopyButton value={it.command} />
              </div>
              <p className="text-xs text-muted">
                {it.launched
                  ? t("已经在终端里拉起。先退出正在跑的同目录窗口，免得两份抢同一份文件。")
                  : it.launch_error
                    ? t("文件写好了，但终端没拉起来：{err}。把上面这条命令自己跑。", {
                        err: it.launch_error,
                      })
                    : t("把上面这条命令丢进终端。")}
              </p>
              {/* 写成了、但有后果的那一类。和上面那行分开：那是「没做成」，
                  这是「做了，而你可能不想要这个后果」。 */}
              {it.note && (
                <p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-2 py-1.5 text-xs">
                  {it.note}
                </p>
              )}
            </div>
          ))}
        </div>
      )}

      {message && <p className="mt-4 text-sm text-accent">{message}</p>}

      {editing && (
        <PresetEditor
          preset={editing}
          busy={save.isPending}
          onClose={() => setEditing(null)}
          onSave={(p) => save.mutate(p)}
        />
      )}
    </div>
  );
}

function PresetEditor({
  preset,
  busy,
  onClose,
  onSave,
}: {
  preset: SessionPreset;
  busy: boolean;
  onClose: () => void;
  onSave: (p: SessionPreset) => void;
}) {
  const t = useT();
  const [draft, setDraft] = useState<SessionPreset>(preset);
  const setTurn = (i: number, patch: Partial<SessionTurn>) => {
    const turns = [...draft.turns];
    turns[i] = { ...turns[i], ...patch };
    setDraft({ ...draft, turns });
  };

  return (
    <Modal onClose={onClose} className="max-w-3xl">
      <h2 className="t-title">{draft.id ? t("编辑预设") : t("新建预设")}</h2>
      <div className="mt-4 space-y-3">
        <label className="block text-xs">
          <span className="mb-1 block text-muted">{t("标题")}</span>
          <TextInput
            value={draft.title}
            onChange={(e) => setDraft({ ...draft, title: e.target.value })}
          />
        </label>
        <label className="block text-xs">
          <span className="mb-1 block text-muted">{t("摘要")}</span>
          <TextInput
            value={draft.summary}
            onChange={(e) => setDraft({ ...draft, summary: e.target.value })}
          />
        </label>
        <div className="space-y-3">
          {draft.turns.map((turn, i) => (
            <div key={i} className="rounded-xl border border-border p-3">
              <div className="mb-2 flex items-center justify-between">
                <Select
                  small
                  className="w-32"
                  value={turn.role}
                  onChange={(e) =>
                    setTurn(i, { role: e.target.value as SessionTurn["role"] })
                  }
                >
                  <option value="user">{t("用户")}</option>
                  <option value="assistant">{t("助手")}</option>
                </Select>
                <button
                  type="button"
                  onClick={() =>
                    setDraft({ ...draft, turns: draft.turns.filter((_, j) => j !== i) })
                  }
                  disabled={draft.turns.length <= 1}
                  className="text-xs text-muted hover:text-red-600 disabled:opacity-30"
                >
                  {t("删这轮")}
                </button>
              </div>
              <TextArea
                rows={6}
                value={turn.text}
                onChange={(e) => setTurn(i, { text: e.target.value })}
              />
            </div>
          ))}
        </div>
        <button
          type="button"
          onClick={() =>
            setDraft({
              ...draft,
              turns: [...draft.turns, { role: "user", text: "" }],
            })
          }
          className="text-xs text-accent hover:underline"
        >
          {t("加一轮")}
        </button>
      </div>
      <div className="mt-5 flex justify-end gap-2">
        <button
          onClick={onClose}
          className="rounded-lg border border-border px-3 py-1.5 text-sm hover:bg-surface-2"
        >
          {t("取消")}
        </button>
        <button
          onClick={() => onSave(draft)}
          disabled={busy}
          className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white disabled:opacity-40"
        >
          {busy ? t("写入中…") : t("保存")}
        </button>
      </div>
    </Modal>
  );
}
