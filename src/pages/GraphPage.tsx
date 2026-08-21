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
import { ComboBox } from "../components/ui/ComboBox";
import { upstreamModelsOf } from "../lib/modelOptions";
import type { GraphDoc, GraphProvider, GraphTier } from "../types";

/// 调度图：把「档位别名 → 哪家的哪个模型」编成内核已经认识的渠道配置。
///
/// 这页只做编辑和校验，真正的语义在后端 `services/graph.rs` 的注释里写全了：
/// 内核不动，所以 graph 是**静态编译**成 `models[].redirect_model`。全局顺序
/// 只排各档队列，存在图上；应用到内核**不改**渠道绑定，也不改渠道优先级 ——
/// 两家共用一个渠道时写两个优先级会互相覆盖。没钉顺序时仍从各档队列做拓扑，
/// 折不出来就拒绝应用，并指出是哪两档打架。

/// 渠道列表里我们用得上的字段。`models` 是候选模型的来源。
type GraphChannel = {
  id?: number;
  name?: string;
  enabled?: boolean;
  models?: { model?: string; redirect_model?: string; disabled?: boolean }[];
};

export function GraphPage() {
  const t = useT();
  const qc = useQueryClient();
  const graphs = useQuery({ queryKey: ["graphs"], queryFn: api.graphList });
  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  const running = kernel.data?.state === "running";
  const channels = useQuery({
    queryKey: ["channels"],
    queryFn: () =>
      api.admin<GraphChannel[]>(
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
    // 拖动顺序时 current 每变一次就重跑校验；没有上一份结果的话面板会闪成
    // 「校验未通过」。本地 invoke 很快，但闪一下仍然扎眼。
    placeholderData: (prev) => prev,
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
            {t("把「哪种活用哪家的哪个模型」配成一张表，应用后写进内核渠道：档位别名落成")} <code className="font-mono text-xs">redirect_model</code>{t("。全局顺序只排各档队列，不会改渠道绑定，也不会改渠道优先级。之后 CLI 只认档位别名，换家、重试、冷却全部由内核原有的选择器完成。")}
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
            {t("内核未运行，读不到渠道列表")}
          </div>
          <p className="mt-1">
            {t("调度图要把 provider 绑到内核里")}<strong className="font-medium">{t("已有的")}</strong>{t("渠道上，所以得先从左下角「启动内核」。下面的下拉框在那之前是空的，与配置本身无关。")}
          </p>
        </div>
      ) : channelList.length === 0 ? (
        <div className="rounded-xl border border-amber-300/70 bg-amber-50/70 px-3 py-2.5 text-xs text-amber-900">
          {t("内核里还没有任何渠道。先去「内核后台」把各家的渠道建好（客户端不替你发明凭据），再回来绑定。")}
        </div>
      ) : null}

      <ValidationPanel
        problems={v?.problems ?? []}
        ok={!!v?.ok}
        order={chipOrder(current, v?.globalOrder ?? [])}
        labelOf={(pid) => current.providers.find((p) => p.id === pid)?.label ?? pid}
        onReorder={(next) => patch(withProviderOrder(current, next))}
      />

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

      <TierTable
        doc={current}
        order={chipOrder(current, v?.globalOrder ?? [])}
        onChange={(tiers) => patch({ tiers })}
        onTierProviders={(i, providers) => {
          const global = chipOrder(current, v?.globalOrder ?? []);
          const old = current.tiers[i].providers;
          const sameSet =
            old.length === providers.length && old.every((id) => providers.includes(id));
          const extra = providers.filter((id) => !global.includes(id));
          const nextOrder = sameSet
            ? mergeSubsequence(global, providers)
            : extra.length
              ? [...global, ...extra]
              : global;
          const tiers = current.tiers.map((tier, j) =>
            j === i ? { ...tier, providers } : tier,
          );
          patch(withProviderOrder({ ...current, tiers }, nextOrder));
        }}
      />

      <RolePanel doc={current} />

      {message && (
        <pre className="card whitespace-pre-wrap break-all p-3 text-xs text-muted">{message}</pre>
      )}
      {save.isError && <p className="text-sm text-red-600">{errText(save.error)}</p>}
    </div>
  );
}

/// 校验面板。全局顺序可拖 —— 这是用户钉在图上的排列，不会改渠道绑定。
function ValidationPanel({
  ok,
  problems,
  order,
  labelOf,
  onReorder,
}: {
  ok: boolean;
  problems: string[];
  order: string[];
  labelOf: (id: string) => string;
  onReorder: (next: string[]) => void;
}) {
  const t = useT();
  const reorder = useReorder(order, onReorder, "x");
  return (
    <div
      className={
        ok
          ? "rounded-xl border border-emerald-200 bg-emerald-50/60 px-3 py-2.5 text-xs text-emerald-800"
          : "rounded-xl border border-amber-300/70 bg-amber-50/70 px-3 py-2.5 text-xs text-amber-900"
      }
    >
      {ok ? (
        <div className="flex items-center gap-2 font-medium">
          <Check className="h-4 w-4 shrink-0" />
          {t("校验通过。")}
        </div>
      ) : (
        <>
          <div className="flex items-center gap-2 font-medium">
            <AlertTriangle className="h-4 w-4 shrink-0" />
            {t("校验未通过，无法应用（不会写入任何东西）")}
          </div>
          <ul className="mt-1.5 list-disc space-y-0.5 pl-5">
            {problems.map((msg, i) => (
              <li key={i}>{msg}</li>
            ))}
          </ul>
        </>
      )}
      {order.length > 0 && (
        <div className="mt-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="shrink-0 font-medium">{t("全局顺序")}</span>
            <ol
              ref={reorder.listRef}
              className="flex min-w-0 flex-nowrap items-center gap-1 overflow-x-auto"
            >
              {order.map((pid, i) => {
                const dragging = reorder.drag?.from === i;
                return (
                  <li
                    key={pid}
                    style={{ transform: `translateX(${reorder.offsetOf(i)}px)` }}
                    className={cn(
                      "flex shrink-0 items-center gap-1 rounded-lg border px-1.5 py-1",
                      dragging
                        ? "z-10 border-accent/50 bg-surface-raised shadow-[var(--shadow-raised)]"
                        : "border-border/80 bg-surface-raised/80 transition-transform duration-[180ms] ease-[cubic-bezier(0.32,0.72,0,1)]",
                    )}
                  >
                    <button
                      onPointerDown={reorder.start(i)}
                      onKeyDown={reorder.onKeyDown(i)}
                      aria-label={t("{name}，第 {n} 位，可拖动或按左右键调整", {
                        name: labelOf(pid),
                        n: i + 1,
                      })}
                      className="cursor-grab touch-none rounded p-0.5 text-muted hover:bg-surface-2 active:cursor-grabbing"
                    >
                      <GripVertical className="h-3.5 w-3.5" />
                    </button>
                    <span className="font-medium">{labelOf(pid)}</span>
                    {i < order.length - 1 && (
                      <span className="pl-0.5 text-[10px] text-muted" aria-hidden>
                        →
                      </span>
                    )}
                  </li>
                );
              })}
            </ol>
          </div>
          <p className="mt-1.5 text-[11px] opacity-80">
            {t("拖动调整各档队列的排列。应用到内核时只写别名映射，不改渠道绑定，也不改渠道优先级。")}
          </p>
        </div>
      )}
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
  channels: GraphChannel[];
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
          {t("每家绑一个内核里已有的渠道（客户端不替你建渠道、不发明凭据），再填它在各档的")}<strong className="font-medium text-content">{t("真实上游模型名")}</strong>
          {t("—— 这些名字必须是该渠道上游认识的。")}
        </p>
        </div>
        <button
          onClick={onAutoBind}
          disabled={channels.length === 0}
          title={t("按渠道名称和 URL 里的关键词猜，填完仍需自己核对")}
          className="shrink-0 rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 text-xs hover:bg-surface-2 disabled:opacity-40"
        >
          {t("按名称自动匹配")}
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
                  {/* 这一格填的是**上游真实模型名**，所以候选取该渠道的
                      `redirect_model || model`；CLI 接管那边填的是别名，取的是
                      另一份清单。两者的区别见 lib/modelOptions.ts。 */}
                  <ComboBox
                    aria-label={t("{p} 在 {tier} 档的模型", { p: p.label, tier: tier.label })}
                    value={p.models[tier.id] ?? ""}
                    placeholder={t("上游模型名")}
                    onChange={(v) => set(i, { models: { ...p.models, [tier.id]: v } })}
                    options={upstreamModelsOf(
                      channels.find((c) => c.id === p.channelId),
                    )}
                    emptyHint={
                      p.channelId == null
                        ? t("先在左边给它绑一个渠道")
                        : t("这个渠道还没配模型")
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
  order,
  onChange,
  onTierProviders,
}: {
  doc: GraphDoc;
  order: string[];
  onChange: (tiers: GraphTier[]) => void;
  onTierProviders: (i: number, providers: string[]) => void;
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
        {t("别名是 CLI 侧实际请求的模型名。队列从上到下是全局顺序在这一档的投影；加入或移除只改变谁参与，不会改渠道绑定。")}
      </p>

      <div className="mt-3 space-y-3">
        {doc.tiers.map((tier, i) => (
          <div key={tier.id} className="rounded-xl border border-border p-3">
            <div className="flex flex-wrap items-center gap-2">
              <span className="w-20 shrink-0 text-sm font-medium">{tier.label}</span>
              <label className="flex items-center gap-1.5 text-xs text-muted">
                {t("别名")}
                <TextInput
                  small
                  mono
                  className="w-40"
                  value={tier.alias}
                  onChange={(e) => setTier(i, { alias: e.target.value })}
                />
              </label>
            </div>
            <ProviderQueue
              tier={tier}
              labelOf={labelOf}
              all={doc.providers}
              order={order}
              onChange={(providers) => onTierProviders(i, providers)}
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
  order,
  onChange,
}: {
  tier: GraphTier;
  all: GraphProvider[];
  labelOf: (id: string) => string;
  order: string[];
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
                aria-label={t("{name}，第 {n} 位，可拖动或按上下键调整", { name: labelOf(pid), n: i + 1 })}
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
              onClick={() => onChange(sortByOrder([...tier.providers, p.id], order))}
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
        {t("角色靠 CLI 侧表达：在「扩展管理」里建一个同名 agent，把它的")}{" "}
        <code className="font-mono">model</code> {t("写成下面这个别名，请求就会落到对应档。")}
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

function usedProviderIds(doc: GraphDoc): string[] {
  const ids: string[] = [];
  for (const tier of doc.tiers) {
    for (const pid of tier.providers) {
      if (!ids.includes(pid)) ids.push(pid);
    }
  }
  return ids;
}

function sortByOrder(ids: string[], order: string[]): string[] {
  return [...ids].sort((a, b) => {
    const ra = order.indexOf(a);
    const rb = order.indexOf(b);
    const ia = ra === -1 ? Number.MAX_SAFE_INTEGER : ra;
    const ib = rb === -1 ? Number.MAX_SAFE_INTEGER : rb;
    if (ia !== ib) return ia - ib;
    return ids.indexOf(a) - ids.indexOf(b);
  });
}

function chipOrder(doc: GraphDoc, computed: string[]): string[] {
  const used = usedProviderIds(doc);
  const base =
    doc.providerOrder && doc.providerOrder.length > 0
      ? doc.providerOrder
      : computed.length > 0
        ? computed
        : used;
  const seen = new Set<string>();
  const out: string[] = [];
  for (const id of [...base, ...used]) {
    if (used.includes(id) && !seen.has(id)) {
      seen.add(id);
      out.push(id);
    }
  }
  return out;
}

function mergeSubsequence(global: string[], sub: string[]): string[] {
  const set = new Set(sub);
  const extra = sub.filter((id) => !global.includes(id));
  let k = 0;
  const mapped = global.map((id) => (set.has(id) ? sub[k++] : id));
  return [...mapped, ...extra];
}

function withProviderOrder(
  doc: GraphDoc,
  order: string[],
): Pick<GraphDoc, "providerOrder" | "tiers"> {
  return {
    providerOrder: order,
    tiers: doc.tiers.map((tier) => ({
      ...tier,
      providers: sortByOrder(tier.providers, order),
    })),
  };
}
