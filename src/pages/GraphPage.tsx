import { useT } from "../i18n";
import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Check, GripVertical, Play, Save, X, Zap } from "lucide-react";
import { api } from "../lib/api";
import { suggestChannels } from "../lib/providerMatch";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import { useReorder } from "../lib/useReorder";
import { Select, TextInput } from "../components/ui/Input";
import type { GraphDoc, GraphProvider, GraphTier } from "../types";

/// 调度图：把「档位别名 → 哪家的哪个模型」编成内核已经认识的渠道配置。
///
/// 这页只做编辑和校验，真正的语义在后端 `services/graph.rs` 的注释里写全了：
/// 内核不动，所以 graph 是**静态编译**成 `models[].redirect_model` + 渠道优先级，
/// 而不是请求时改写。因此逐档不同的 provider 顺序必须能折成一个全局顺序 ——
/// 折不出来就拒绝保存，并指出是哪两档打架。

export function GraphPage() {
  const t = useT();
  const qc = useQueryClient();
  const graphs = useQuery({ queryKey: ["graphs"], queryFn: api.graphList });
  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  const running = kernel.data?.state === "running";
  const channels = useQuery({
    queryKey: ["channels"],
    queryFn: () =>
      api.admin<{ id?: number; name?: string; url?: string; urls?: unknown }[]>(
        "GET",
        "channels",
      ),
    // 内核没跑就别发这个请求：它必然失败，而失败的表现是「下拉框空的」，
    // 用户会以为是调度图有问题（实际发生过）。
    enabled: running,
  });
  const channelList = channels.data?.data ?? [];

  const [activeId, setActiveId] = useState<string | null>(null);
  const [draft, setDraft] = useState<GraphDoc | null>(null);

  const list = graphs.data ?? [];
  const current = draft ?? list.find((g) => g.id === (activeId ?? list[0]?.id)) ?? null;

  // 切 tab 时丢掉未保存的草稿：两张图的字段结构一样，留着会把 A 的改动写到 B 上。
  const switchTo = (id: string) => {
    setActiveId(id);
    setDraft(null);
  };

  const validation = useQuery({
    queryKey: ["graph-validate", current],
    queryFn: () => api.graphValidate(current!),
    enabled: !!current,
  });

  const save = useMutation({
    mutationFn: (d: GraphDoc) => api.graphSave(d),
    onSuccess: () => {
      setDraft(null);
      qc.invalidateQueries({ queryKey: ["graphs"] });
    },
  });
  const apply = useMutation({
    mutationFn: (id: string) => api.graphApply(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["channels"] }),
  });

  const [message, setMessage] = useState<string | null>(null);

  if (!current) {
    return (
      <div>
        <h1 className="t-display">{t("调度图")}</h1>
        <p className="mt-2 text-sm text-muted">
          {graphs.isPending ? t("读取中…") : t("还没有调度图。")}
        </p>
      </div>
    );
  }

  const patch = (p: Partial<GraphDoc>) => setDraft({ ...current, ...p });
  const v = validation.data;
  const dirty = draft !== null;

  return (
    <div className="space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h1 className="t-display">{t("调度图")}</h1>
          <p className="mt-0.5 max-w-3xl text-sm text-muted">
            把「哪种活用哪家的哪个模型」配成一张表，应用后写进内核渠道：档位别名
            落成 <code className="font-mono text-xs">redirect_model</code>，队列顺序落成渠道优先级。
            之后 CLI 只认四个档位别名，换家、重试、冷却全部由内核原有的选择器完成。
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            onClick={() => save.mutate(current)}
            disabled={!dirty || save.isPending}
            className="flex items-center gap-1.5 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
          >
            <Save className="h-4 w-4" /> {dirty ? t("保存改动") : t("已保存")}
          </button>
          <button
            onClick={() => {
              setMessage(null);
              apply.mutate(current.id, {
                onSuccess: (lines) => setMessage(lines.join("\n")),
                onError: (e) => setMessage(errText(e)),
              });
            }}
            disabled={apply.isPending || dirty || !v?.ok}
            title={
              dirty
                ? t("先保存再应用")
                : !v?.ok
                  ? t("校验未通过，不能应用")
                  : t("写入内核渠道")
            }
            className="flex items-center gap-1.5 rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
          >
            <Zap className="h-4 w-4" /> {apply.isPending ? t("写入中…") : t("应用到内核")}
          </button>
        </div>
      </header>

      {/* 两张图一人一个 tab，PRD §11.2。 */}
      <div role="tablist" className="flex gap-1 border-b border-border">
        {list.map((g) => (
          <button
            key={g.id}
            role="tab"
            aria-selected={g.id === current.id}
            onClick={() => switchTo(g.id)}
            className={cn(
              "-mb-px border-b-2 px-3 py-2 text-sm",
              g.id === current.id
                ? "border-accent font-medium text-accent"
                : "border-transparent text-muted hover:text-content",
            )}
          >
            {g.label}
          </button>
        ))}
      </div>

      {/* 渠道列表来自内核。内核没起来时先说这件事 —— 否则下面清一色的
          「还没有绑定渠道」会把人引到错误的方向。 */}
      {!running ? (
        <div className="rounded-xl border border-amber-300/70 bg-amber-50/70 px-3 py-2.5 text-xs text-amber-900">
          <div className="flex items-center gap-2 font-medium">
            <AlertTriangle className="h-4 w-4 shrink-0" />
            内核未运行，读不到渠道列表
          </div>
          <p className="mt-1">
            调度图要把 provider 绑到内核里<strong className="font-medium">{t("已有的")}</strong>渠道上，
            所以得先从左下角「启动内核」。下面的下拉框在那之前是空的，与配置本身无关。
          </p>
        </div>
      ) : channelList.length === 0 ? (
        <div className="rounded-xl border border-amber-300/70 bg-amber-50/70 px-3 py-2.5 text-xs text-amber-900">
          内核里还没有任何渠道。先去「内核后台」把各家的渠道建好（客户端不替你发明凭据），
          再回来绑定。
        </div>
      ) : (
        <ValidationPanel
          problems={v?.problems ?? []}
          ok={!!v?.ok}
          order={v?.globalOrder ?? []}
        />
      )}

      <ProviderTable
        doc={current}
        channels={channelList}
        onChange={(providers) => patch({ providers })}
        onAutoBind={() => {
          const guess = suggestChannels(
            current.providers.map((p) => p.id),
            channelList,
          );
          const hit = current.providers.filter((p) => guess[p.id] != null).length;
          patch({
            providers: current.providers.map((p) =>
              // 只填没绑过的，不覆盖用户已经选好的。
              p.channelId == null && guess[p.id] != null
                ? { ...p, channelId: guess[p.id] }
                : p,
            ),
          });
          setMessage(
            hit === 0
              ? t("按名称和 URL 没猜出任何一家，需要手动选。")
              : `按名称和 URL 猜中 ${hit} 家，已填进下拉框 —— 请自己核对一遍再保存。`,
          );
        }}
      />

      <TierTable doc={current} onChange={(tiers) => patch({ tiers })} priorities={v?.priorities} />

      <RolePanel doc={current} />

      {message && (
        <pre className="card whitespace-pre-wrap break-all p-3 text-xs text-muted">{message}</pre>
      )}
      {save.isError && <p className="text-sm text-red-600">{errText(save.error)}</p>}
    </div>
  );
}

/// 校验面板。通过时把算出来的全局顺序摊出来 —— 这是整套静态实现的核心结论，
/// 用户得能一眼看到「最终谁排前面」。
function ValidationPanel({
  ok,
  problems,
  order,
}: {
  ok: boolean;
  problems: string[];
  order: string[];
}) {
  if (ok) {
    return (
      <div className="flex items-center gap-2 rounded-xl border border-emerald-200 bg-emerald-50/60 px-3 py-2 text-xs text-emerald-800">
        <Check className="h-4 w-4 shrink-0" />
        校验通过。全局优先级顺序：{order.join(" → ")}
      </div>
    );
  }
  return (
    <div className="rounded-xl border border-amber-300/70 bg-amber-50/70 px-3 py-2.5 text-xs text-amber-900">
      <div className="flex items-center gap-2 font-medium">
        <AlertTriangle className="h-4 w-4 shrink-0" />
        校验未通过，无法应用（不会写入任何东西）
      </div>
      <ul className="mt-1.5 list-disc space-y-0.5 pl-5">
        {problems.map((p, i) => (
          <li key={i}>{p}</li>
        ))}
      </ul>
    </div>
  );
}

function ProviderTable({
  doc,
  channels,
  onChange,
  onAutoBind,
}: {
  doc: GraphDoc;
  channels: { id?: number; name?: string; url?: string; urls?: unknown }[];
  onChange: (p: GraphProvider[]) => void;
  onAutoBind: () => void;
}) {
  const t = useT();
  const set = (i: number, patch: Partial<GraphProvider>) => {
    const next = [...doc.providers];
    next[i] = { ...next[i], ...patch };
    onChange(next);
  };

  return (
    <section className="card overflow-hidden">
      <div className="flex flex-wrap items-start justify-between gap-3 border-b border-border px-4 py-3">
        <div className="min-w-0">
        <h2 className="t-title">Provider</h2>
        <p className="mt-0.5 text-xs text-muted">
          每家绑一个内核里已有的渠道（客户端不替你建渠道、不发明凭据），
          再填它在各档的<strong className="font-medium text-content">{t("真实上游模型名")}</strong>
          —— 这些名字必须是该渠道上游认识的。
        </p>
        </div>
        <button
          onClick={onAutoBind}
          disabled={channels.length === 0}
          title={t("按渠道名称和 URL 里的关键词猜，填完仍需自己核对")}
          className="shrink-0 rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 text-xs hover:bg-surface-2 disabled:opacity-40"
        >
          按名称自动匹配
        </button>
      </div>
      <table className="w-full table-fixed text-sm">
        <thead>
          <tr className="border-b border-border bg-surface-2 text-left text-xs text-muted">
            <th className="w-16 px-3 py-2">{t("启用")}</th>
            <th className="w-24 px-2 py-2">Provider</th>
            <th className="w-52 px-2 py-2">{t("渠道")}</th>
            {doc.tiers.map((tier) => (
              <th key={tier.id} className="px-2 py-2">
                {tier.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {doc.providers.map((p, i) => (
            <tr key={p.id} className={cn("border-b border-border/50", !p.enabled && "opacity-50")}>
              <td className="px-3 py-2">
                <input
                  type="checkbox"
                  aria-label={`启用 ${p.label}`}
                  checked={p.enabled}
                  onChange={(e) => set(i, { enabled: e.target.checked })}
                  className="h-4 w-4"
                />
              </td>
              <td className="px-2 py-2 font-medium">{p.label}</td>
              <td className="px-2 py-2">
                <Select
                  small
                  className="w-full"
                  aria-label={`${p.label} 绑定的渠道`}
                  value={p.channelId ?? ""}
                  onChange={(e) =>
                    set(i, { channelId: e.target.value ? Number(e.target.value) : null })
                  }
                >
                  <option value="">
                    {channels.length === 0 ? t("（内核未运行 / 无渠道）") : t("未绑定")}
                  </option>
                  {channels
                    .filter((c) => c.id !== undefined)
                    .map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name} (#{c.id})
                      </option>
                    ))}
                </Select>
              </td>
              {doc.tiers.map((tier) => (
                <td key={tier.id} className="px-2 py-2">
                  <TextInput
                    small
                    mono
                    aria-label={`${p.label} 在 ${tier.label} 档的模型`}
                    value={p.models[tier.id] ?? ""}
                    placeholder={t("上游模型名")}
                    onChange={(e) =>
                      set(i, { models: { ...p.models, [tier.id]: e.target.value } })
                    }
                  />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

function TierTable({
  doc,
  onChange,
  priorities,
}: {
  doc: GraphDoc;
  onChange: (t: GraphTier[]) => void;
  priorities?: Record<string, number>;
}) {
  const t = useT();
  const setTier = (i: number, patch: Partial<GraphTier>) => {
    const next = [...doc.tiers];
    next[i] = { ...next[i], ...patch };
    onChange(next);
  };
  const labelOf = (pid: string) =>
    doc.providers.find((p) => p.id === pid)?.label ?? pid;

  return (
    <section className="card p-4">
      <h2 className="t-title">{t("档位与队列")}</h2>
      <p className="mt-0.5 text-xs text-muted">
        别名是 CLI 侧实际请求的模型名。队列从上到下依次尝试 —— 但内核只有<strong className="font-medium text-content">{t("渠道级")}</strong>
        优先级，所以所有档的顺序必须能折成一个全局顺序，折不出来上面会报冲突。
      </p>

      <div className="mt-3 space-y-3">
        {doc.tiers.map((t, i) => (
          <div key={t.id} className="rounded-xl border border-border p-3">
            <div className="flex flex-wrap items-center gap-2">
              <span className="w-20 shrink-0 text-sm font-medium">{t.label}</span>
              <label className="flex items-center gap-1.5 text-xs text-muted">
                别名
                <TextInput
                  small
                  mono
                  className="w-40"
                  value={t.alias}
                  onChange={(e) => setTier(i, { alias: e.target.value })}
                />
              </label>
            </div>
            <ProviderQueue
              tier={t}
              labelOf={labelOf}
              all={doc.providers}
              priorities={priorities}
              onChange={(providers) => setTier(i, { providers })}
            />
          </div>
        ))}
      </div>
    </section>
  );
}

/// 一档的 provider 队列。拖拽换序用的是和模型链同一个 hook。
function ProviderQueue({
  tier,
  all,
  labelOf,
  priorities,
  onChange,
}: {
  tier: GraphTier;
  all: GraphProvider[];
  labelOf: (id: string) => string;
  priorities?: Record<string, number>;
  onChange: (p: string[]) => void;
}) {
  const t = useT();
  const reorder = useReorder(tier.providers, onChange);
  const unused = all.filter((p) => !tier.providers.includes(p.id));

  return (
    <>
      <ol ref={reorder.listRef} className="mt-2 space-y-1.5">
        {tier.providers.map((pid, i) => {
          const dragging = reorder.drag?.from === i;
          return (
            <li
              key={pid}
              style={{ transform: `translateY(${reorder.offsetOf(i)}px)` }}
              className={cn(
                "flex items-center gap-2 rounded-lg border px-2 py-1.5 text-xs",
                dragging
                  ? "z-10 border-accent/50 bg-surface-raised shadow-[var(--shadow-raised)]"
                  : "border-border bg-surface-raised transition-transform duration-[180ms] ease-[cubic-bezier(0.32,0.72,0,1)]",
              )}
            >
              <button
                onPointerDown={reorder.start(i)}
                onKeyDown={reorder.onKeyDown(i)}
                aria-label={`${labelOf(pid)}，第 ${i + 1} 位，可拖动或按上下键调整`}
                className="cursor-grab touch-none rounded p-0.5 text-muted hover:bg-surface-2 active:cursor-grabbing"
              >
                <GripVertical className="h-3.5 w-3.5" />
              </button>
              <span className="flex h-5 w-5 items-center justify-center rounded-full bg-accent/10 text-[10px] font-medium text-accent">
                {i + 1}
              </span>
              <span className="font-medium">{labelOf(pid)}</span>
              <span className="font-mono text-[10px] text-muted">
                {all.find((p) => p.id === pid)?.models[tier.id] || t("（未填模型）")}
              </span>
              {priorities?.[pid] != null && (
                <span className="text-[10px] text-muted">优先级 {priorities[pid]}</span>
              )}
              <button
                onClick={() => onChange(tier.providers.filter((x) => x !== pid))}
                aria-label={`从 ${tier.label} 档移除 ${labelOf(pid)}`}
                className="ml-auto rounded p-0.5 text-muted hover:text-red-600"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </li>
          );
        })}
      </ol>

      {unused.length > 0 && (
        <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
          <span className="text-[10px] text-muted">{t("加入：")}</span>
          {unused.map((p) => (
            <button
              key={p.id}
              onClick={() => onChange([...tier.providers, p.id])}
              className="rounded-md border border-dashed border-border px-1.5 py-0.5 text-[10px] text-muted hover:bg-surface-2"
            >
              + {p.label}
            </button>
          ))}
        </div>
      )}
    </>
  );
}

/// 角色 → 档位。这一段只是说明：真正让角色生效要在扩展管理里装对应的 agent
/// 文件，把 `model:` 写成这里的别名。客户端不越俎代庖去猜用户想装哪些 agent。
function RolePanel({ doc }: { doc: GraphDoc }) {
  const t = useT();
  const aliasOf = (tierId: string) =>
    doc.tiers.find((t) => t.id === tierId)?.alias ?? "?";
  const rows = useMemo(() => doc.roles ?? [], [doc.roles]);
  if (rows.length === 0) return null;

  return (
    <section className="card p-4">
      <h2 className="t-title">{t("角色映射")}</h2>
      <p className="mt-0.5 text-xs text-muted">
        角色靠 CLI 侧表达：在「扩展管理」里建一个同名 agent，把它的{" "}
        <code className="font-mono">model</code> 写成下面这个别名，请求就会落到对应档。
      </p>
      <ul className="mt-2.5 grid gap-1.5 sm:grid-cols-2">
        {rows.map((r) => (
          <li
            key={r.id}
            className="flex items-center gap-2 rounded-lg border border-border px-2.5 py-1.5 text-xs"
          >
            <Play className="h-3 w-3 shrink-0 text-muted" />
            <span className="font-medium">{r.label}</span>
            <span className="ml-auto font-mono text-[11px] text-accent">
              {aliasOf(r.tier)}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}
