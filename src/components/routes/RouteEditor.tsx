import { useMemo, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useT, type Translate } from "../../i18n";
import { api } from "../../lib/api";
import { cn } from "../../lib/cn";
import { errText } from "../../lib/err";
import { upstreamModelsOf, type ChannelModels } from "../../lib/modelOptions";
import { useReorder } from "../../lib/useReorder";
import type { FallbackChain, ForcedRoute, RouteHit } from "../../types";
import { ComboBox } from "../ui/ComboBox";
import { Select } from "../ui/Input";
import { AlertTriangle, CheckCircle2, GripVertical, Plus, Radar, Trash2, X, XCircle } from "lucide-react";

/// 「这个别名应该落到哪」—— 本地编排，点应用才写内核。
///
/// # 为什么两种模式是一张表
///
/// 内核里只有一种事实：某个渠道的 `models[]` 里有没有这个别名、指向哪个上游，
/// 以及那个渠道的优先级。退让和独占的差别**只在优先级怎么算**：
///
///   * 退让：固定阶梯 100 / 90 / 80……，排在后面的只有当高优先级渠道冷却 / 限流 /
///     5xx 时才会被内核选中 —— 这就是「模型链」；
///   * 独占：应用时先问一次内核、算出正在服务这个别名的**其他**渠道的最高优先级，
///     再把目标排到它上面 —— 谁也别想平分，这就是「强制路由」。
///
/// 输入形状（别名 → 有序的「渠道 + 上游模型」）、写入路径（`patch_channel`）、
/// 最终写进内核的东西完全一样，所以这里是一张表加一个模式开关，而不是两个页面。
///
/// 一个别名同时只保留一种模式：保存时会删掉另一种（见 save）。两者同时存在的话，
/// 谁后应用谁赢，落点会随应用顺序漂移 —— 那不是能靠界面解释清楚的状态。

export type RouteTarget = {
  channel_id: number | null;
  channel_name: string | null;
  model: string;
};

export type RouteMode = "fallback" | "exclusive";

/// 与后端 `services::fallback::hop_priority` 同一个公式。写在这里是为了让用户在
/// 保存**之前**就看见每一层会被写成什么优先级 —— 这个值会改到渠道上，不该等
/// 应用完了再从日志里发现。两边任何一处改了，另一处必须跟着改。
const hopPriority = (i: number) => 100 - i * 10;

type Probe = { models: string[]; err: string };
type HopVerdict = { level: "error" | "warn" | "ok" | "idle"; text: string };

function verdictOf(
  target: RouteTarget,
  channel: ChannelModels | undefined,
  probe: Probe | undefined,
  t: Translate,
): HopVerdict {
  if (target.channel_id == null) {
    return { level: "warn", text: t("没绑渠道 · 应用时这一条会被跳过") };
  }
  if (!channel) {
    return { level: "error", text: t("渠道 #{id} 已不存在", { id: target.channel_id }) };
  }
  if (channel.enabled === false) {
    return { level: "error", text: t("渠道已禁用 · 这一条永远不会被选中") };
  }
  if (!target.model.trim()) return { level: "idle", text: "" };
  if (!probe) return { level: "idle", text: "" };
  if (probe.err) {
    return { level: "warn", text: t("上游清单拉不到，无法校验：{err}", { err: probe.err }) };
  }
  return probe.models.includes(target.model.trim())
    ? { level: "ok", text: t("上游清单里有这个模型") }
    : {
        level: "error",
        text: t("上游清单里没有 {m} · 请求打到这一条会直接失败", { m: target.model.trim() }),
      };
}

/// 某个模式下**已存盘**的那张表。
///
/// 必须按模式取，不能「有哪个取哪个」：两种编排在磁盘上是两份文件，一个别名两边
/// 都有过记录时，用另一边的表去比「改没改」会让「未保存」标记和应用按钮全反过来。
function savedTargets(
  mode: RouteMode,
  chain: FallbackChain | undefined,
  route: ForcedRoute | undefined,
): RouteTarget[] {
  if (mode === "exclusive") {
    return (route?.targets ?? []).map((t) => ({
      channel_id: t.channel_id,
      channel_name: t.channel_name,
      model: t.model,
    }));
  }
  return (chain?.hops ?? []).map((h) => ({
    channel_id: h.channel_id,
    channel_name: h.channel_name,
    model: h.upstream,
  }));
}

export function RouteEditor({
  alias,
  channels,
  chain,
  route,
  hits,
}: {
  alias: string;
  channels: ChannelModels[];
  chain?: FallbackChain;
  route?: ForcedRoute;
  /** 内核**现在**的落点，用来一键把现状固化成编排。 */
  hits: RouteHit[];
}) {
  const t = useT();
  const qc = useQueryClient();
  const initialMode: RouteMode = route ? "exclusive" : "fallback";
  const [mode, setMode] = useState<RouteMode>(initialMode);
  const [targets, setTargets] = useState<RouteTarget[]>(() =>
    savedTargets(initialMode, chain, route),
  );
  const [probes, setProbes] = useState<Record<number, Probe>>({});

  /// 换模式。
  ///
  /// 目标模式**已经有存盘**时装它自己的表 —— 那是一份真实记录，拿当前草稿盖掉它
  /// 等于悄悄改了另一份配置。没有存盘时把草稿带过去：绝大多数「换模式」其实是
  /// 「这几条不变，只想换一种优先级算法」，让人重填一遍纯属折磨。
  const switchMode = (next: RouteMode) => {
    if (next === mode) return;
    const saved = savedTargets(next, chain, route);
    setMode(next);
    if (saved.length > 0) setTargets(saved);
  };

  const channelOf = (id: number | null) => channels.find((c) => c.id === id);
  const candidatesFor = (id: number | null) => {
    const probe = id == null ? undefined : probes[id];
    if (probe?.models.length) return probe.models;
    return upstreamModelsOf(channelOf(id));
  };

  const setAt = (i: number, patch: Partial<RouteTarget>) =>
    setTargets((cur) => cur.map((x, j) => (j === i ? { ...x, ...patch } : x)));
  const removeAt = (i: number) => setTargets((cur) => cur.filter((_, j) => j !== i));
  const addOne = () =>
    setTargets((cur) => [...cur, { channel_id: null, channel_name: null, model: "" }]);
  /// 把内核现状抄成编排。抄完还是本地文件，要点「应用」才写回内核。
  const seedFromKernel = () =>
    setTargets(
      hits.map((h) => ({ channel_id: h.channel_id, channel_name: h.channel_name, model: h.upstream })),
    );

  const reorder = useReorder(targets, setTargets);

  const invalidate = () => {
    qc.invalidateQueries({ queryKey: ["fallback"] });
    qc.invalidateQueries({ queryKey: ["forced-routes"] });
    qc.invalidateQueries({ queryKey: ["channels"] });
    qc.invalidateQueries({ queryKey: ["alias-routes"] });
    qc.invalidateQueries({ queryKey: ["context-window-preview"] });
    qc.invalidateQueries({ queryKey: ["context-tiers"] });
    qc.invalidateQueries({ queryKey: ["cli-preview"] });
  };

  const save = useMutation({
    mutationFn: async () => {
      const lines: string[] = [];
      if (mode === "fallback") {
        await api.fallbackSave({
          alias,
          hops: targets.map((x) => ({
            upstream: x.model.trim(),
            channel_id: x.channel_id,
            channel_name: x.channel_name,
          })),
        });
        lines.push(t("已保存「{alias}」的退让编排（{n} 条）。", { alias, n: targets.length }));
        // 同一别名只留一种模式：另一种会跟着被应用，落点随应用顺序漂移。
        if (route) {
          await api.forcedRouteDelete(alias);
          lines.push(t("原来的独占编排已删除 —— 一个别名同时只保留一种模式。"));
        }
      } else {
        await api.forcedRouteSave({
          from: alias,
          targets: targets.map((x) => ({
            channel_id: x.channel_id,
            channel_name: x.channel_name,
            model: x.model.trim(),
          })),
        });
        lines.push(t("已保存「{alias}」的独占编排（{n} 条）。", { alias, n: targets.length }));
        if (chain) {
          await api.fallbackDelete(alias);
          lines.push(t("原来的退让编排已删除 —— 一个别名同时只保留一种模式。"));
        }
      }
      return lines;
    },
    onSuccess: invalidate,
  });
  const apply = useMutation({
    mutationFn: () => (mode === "fallback" ? api.fallbackApply(alias) : api.forcedRouteApply(alias)),
    onSuccess: invalidate,
  });
  const drop = useMutation({
    mutationFn: async () => {
      if (chain) await api.fallbackDelete(alias);
      if (route) await api.forcedRouteDelete(alias);
    },
    onSuccess: invalidate,
  });
  const check = useMutation({
    mutationFn: async () => {
      const ids = [...new Set(targets.map((x) => x.channel_id).filter((id): id is number => id != null))];
      const pairs = await Promise.all(
        ids.map(async (id) => {
          try {
            const r = await api.admin<{ models?: { model?: string }[] }>(
              "GET",
              `channels/${id}/models/fetch`,
            );
            const models = (r.data?.models ?? []).map((m) => m.model ?? "").filter(Boolean);
            // 内核对拉取失败是返回 200 + success=false 的（上游报错属预期内），
            // 所以「拿到 0 个」要当成「没给出清单」，不能当成空集去判定「都不存在」
            // —— 那会把每一条都误报成红的。
            return [id, models.length ? { models, err: "" } : { models: [], err: t("上游没有返回任何模型") }] as const;
          } catch (e) {
            return [id, { models: [], err: errText(e) }] as const;
          }
        }),
      );
      return Object.fromEntries(pairs) as Record<number, Probe>;
    },
    onSuccess: setProbes,
  });

  // 「有没有存盘」和「改没改」都按**当前模式**算。应用按钮走的是当前模式那条
  // 命令（fallbackApply / forcedRouteApply），拿另一种模式的存在与否去开它，
  // 点下去只会得到一句「没有叫 X 的强制路由」。
  const savedHere = mode === "fallback" ? chain !== undefined : route !== undefined;
  // 另一种模式下也存着一份 —— 保存会把它删掉，得先告诉用户。
  const otherMode = mode === "fallback" ? route !== undefined : chain !== undefined;
  const dirty = useMemo(
    () => JSON.stringify(targets) !== JSON.stringify(savedTargets(mode, chain, route)),
    [targets, mode, chain, route],
  );
  const canSave = targets.length > 0 && targets.every((x) => x.model.trim().length > 0);
  // 存盘的和屏幕上的不一样时不给应用：应用写的是磁盘上那份，而用户看着的是草稿
  // —— 那是「我明明改了，怎么写进去的是旧的」最容易发生的地方。
  const canApply = savedHere && !dirty;

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <div className="flex rounded-lg bg-surface-2 p-0.5" role="tablist">
          {(
            [
              { id: "fallback", label: t("退让"), hint: t("优先级 100/90/80…，主力不可用时内核自动往下走") },
              { id: "exclusive", label: t("独占"), hint: t("应用时排到现有服务者之上，谁也别想平分") },
            ] as const
          ).map((m) => (
            <button
              key={m.id}
              role="tab"
              aria-selected={mode === m.id}
              onClick={() => switchMode(m.id)}
              title={m.hint}
              className={cn(
                "rounded-[6px] px-3 py-1 text-xs font-medium",
                mode === m.id ? "bg-surface-raised text-content shadow-sm" : "text-muted hover:text-content",
              )}
            >
              {m.label}
            </button>
          ))}
        </div>
        <span className="text-[11px] text-muted">
          {mode === "fallback"
            ? t("写进内核时按 100 / 90 / 80… 依次降级")
            : t("写进内核时压过正在服务这个别名的其它渠道")}
        </span>
        {/* 每一条填的是「哪家渠道 + 发给它的上游真实模型名」，候选来自那个渠道
            自己的模型清单 —— 不是左边那种别名。这两种名字看起来一样，要的东西
            正好相反，填反了要等发请求那一刻才炸。 */}
        <span className="basis-full text-[11px] text-muted/70">
          {t("每一条 = 一个渠道 + 发给它的上游真实模型名（候选来自那个渠道自己的模型清单）。")}
        </span>
        {dirty && (
          <span className="rounded-full bg-amber-500/15 px-2 py-0.5 text-[11px] font-medium text-amber-700">
            {t("未保存")}
          </span>
        )}
      </div>

      {/* 另一种模式也存着一份时先说清楚。保存会把它删掉（见 save），而那是一份
          真实配置 —— 事后在日志里补一句「已删除」太晚了。 */}
      {otherMode && (
        <p className="rounded-lg border border-amber-500/40 bg-amber-500/10 px-2.5 py-1.5 text-[11px] leading-relaxed text-amber-900">
          {mode === "fallback"
            ? t("这个别名还存着一条「独占」编排（{n} 条目标）。点保存会把它删掉 —— 一个别名同时只保留一种模式，两份都在的话谁后应用谁赢。", {
                n: route?.targets.length ?? 0,
              })
            : t("这个别名还存着一条「退让」编排（{n} 条目标）。点保存会把它删掉 —— 一个别名同时只保留一种模式，两份都在的话谁后应用谁赢。", {
                n: chain?.hops.length ?? 0,
              })}
        </p>
      )}

      {targets.length === 0 ? (
        <div className="rounded-xl border border-dashed border-border px-3 py-4 text-center">
          <p className="text-xs text-muted">{t("还没有编排。这个别名现在只按内核里已有的落点走。")}</p>
          <div className="mt-2 flex flex-wrap items-center justify-center gap-2">
            <button
              onClick={seedFromKernel}
              disabled={hits.length === 0}
              className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
            >
              {t("从内核落点复制一份（{n} 条）", { n: hits.length })}
            </button>
            <button
              onClick={addOne}
              className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
            >
              <Plus className="h-3.5 w-3.5" /> {t("从空白开始")}
            </button>
          </div>
        </div>
      ) : (
        <ol ref={reorder.listRef} className="space-y-2">
          {targets.map((x, i) => {
            const dragging = reorder.drag?.from === i;
            return (
              <li
                key={i}
                style={{ transform: `translateY(${reorder.offsetOf(i)}px)` }}
                className={cn(
                  "rounded-xl border bg-surface-raised p-2",
                  dragging
                    ? "z-10 border-accent/50 shadow-[var(--shadow-raised)]"
                    : "border-border transition-transform duration-[180ms] ease-[cubic-bezier(0.32,0.72,0,1)]",
                )}
              >
                <div className="flex items-center gap-2">
                  <button
                    onPointerDown={reorder.start(i)}
                    onKeyDown={reorder.onKeyDown(i)}
                    aria-label={t("第 {n} 条，拖动或按上下键调整顺序", { n: i + 1 })}
                    title={t("拖动排序")}
                    className="flex cursor-grab touch-none items-center rounded-md p-1 text-muted hover:bg-surface-2 active:cursor-grabbing"
                  >
                    <GripVertical className="h-4 w-4" />
                  </button>
                  <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-accent/10 text-[11px] font-medium text-accent">
                    {i + 1}
                  </span>
                  <Select
                    className="w-52 shrink-0"
                    aria-label={t("第 {n} 条的渠道", { n: i + 1 })}
                    value={x.channel_id ?? ""}
                    onChange={(e) => {
                      const id = e.target.value ? Number(e.target.value) : null;
                      setAt(i, {
                        channel_id: id,
                        // 名字一并存下来，列表页不必再去查一次渠道表。
                        channel_name: channels.find((c) => c.id === id)?.name ?? null,
                      });
                    }}
                  >
                    <option value="">{t("选择渠道")}</option>
                    {channels.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name ?? t("渠道")} (#{c.id})
                        {c.enabled === false ? t("（已禁用）") : ""}
                      </option>
                    ))}
                  </Select>
                  <ComboBox
                    className="flex-1"
                    aria-label={t("第 {n} 条的上游模型", { n: i + 1 })}
                    value={x.model}
                    onChange={(v) => setAt(i, { model: v })}
                    placeholder={t("上游模型，例如 claude-opus-5")}
                    options={candidatesFor(x.channel_id)}
                    emptyHint={
                      x.channel_id == null
                        ? t("先选左边的渠道，这里会列出它能服务的模型")
                        : t("这个渠道还没配模型；点「校验上游模型」去问一次上游")
                    }
                  />
                  <button
                    onClick={() => removeAt(i)}
                    aria-label={t("删除第 {n} 条", { n: i + 1 })}
                    className="shrink-0 rounded-md border border-border p-1.5 text-muted hover:bg-surface-2 hover:text-red-600"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
                <div className="mt-1 pl-[4.6rem] text-[11px] text-muted">
                  {mode === "fallback" ? (
                    <>
                      {t("应用后会把该渠道的优先级写成")} {hopPriority(i)}
                      <span className="text-muted/70">{t("（影响该渠道服务的所有模型）")}</span>
                    </>
                  ) : (
                    <>{t("应用时算出的优先级会压过正在服务这个别名的其它渠道")}</>
                  )}
                </div>
                <VerdictLine
                  verdict={verdictOf(x, channelOf(x.channel_id), x.channel_id == null ? undefined : probes[x.channel_id], t)}
                />
              </li>
            );
          })}
        </ol>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <button
          onClick={addOne}
          className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 text-xs hover:bg-surface-2"
        >
          <Plus className="h-3.5 w-3.5" /> {t("添加一条")}
        </button>
        {/* 上游校验。内核已经能「按渠道声明的协议去问上游要模型清单」
            （GET /admin/channels/:id/models/fetch），这里只负责把它拉回来、
            和每一条的 redirect 目标对一下。 */}
        <button
          onClick={() => check.mutate()}
          disabled={check.isPending || targets.length === 0}
          title={t("逐个渠道去问上游要真实模型清单，核对每一条的模型名")}
          className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 text-xs hover:bg-surface-2 disabled:opacity-40"
        >
          <Radar className="h-3.5 w-3.5" />
          {check.isPending ? t("校验中…") : t("校验上游模型")}
        </button>
        {targets.length > 0 && (
          <button
            onClick={() => setTargets([])}
            className="rounded-lg border border-border px-2.5 py-1.5 text-xs text-muted hover:bg-surface-2"
          >
            {t("清空")}
          </button>
        )}
        <div className="flex-1" />
        {savedHere && (
          <button
            onClick={() => drop.mutate()}
            disabled={drop.isPending}
            className="flex items-center gap-1 rounded-lg border border-border px-2.5 py-1.5 text-xs text-red-600 hover:bg-red-50 disabled:opacity-40"
          >
            <Trash2 className="h-3.5 w-3.5" /> {t("删除编排")}
          </button>
        )}
        <button
          onClick={() => save.mutate()}
          disabled={!canSave || save.isPending}
          title={canSave ? undefined : t("每条都要填上游模型名")}
          className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
        >
          {save.isPending ? t("保存中…") : t("保存")}
        </button>
        <button
          onClick={() => apply.mutate()}
          disabled={!canApply || apply.isPending}
          title={
            !savedHere
              ? t("先保存，再应用")
              : dirty
                ? t("屏幕上的改动还没保存 —— 先点「保存」，应用写的是磁盘上那份")
                : t("把这张表写进内核渠道")
          }
          className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
        >
          {apply.isPending ? t("应用中…") : t("应用到内核")}
        </button>
      </div>

      {save.isError && <p className="text-xs text-red-600">{errText(save.error)}</p>}
      {drop.isError && <p className="text-xs text-red-600">{errText(drop.error)}</p>}
      {save.data && (
        <ul className="text-[11px] text-accent">
          {save.data.map((line, i) => (
            <li key={i}>✓ {line}</li>
          ))}
        </ul>
      )}
      {apply.isError && <p className="text-xs text-red-600">{errText(apply.error)}</p>}
      {apply.data && (
        <div className="rounded-lg border border-border bg-surface-2/60 p-3 font-mono text-[11px] leading-relaxed text-muted">
          {apply.data.map((line, i) => (
            <div key={i}>{line}</div>
          ))}
        </div>
      )}
    </div>
  );
}

function VerdictLine({ verdict }: { verdict: HopVerdict }) {
  if (verdict.level === "idle" || !verdict.text) return null;
  const Icon =
    verdict.level === "ok" ? CheckCircle2 : verdict.level === "warn" ? AlertTriangle : XCircle;
  return (
    <div
      className={cn(
        "mt-1 flex items-center gap-1 pl-[4.6rem] text-[11px]",
        verdict.level === "ok"
          ? "text-emerald-700"
          : verdict.level === "warn"
            ? "text-amber-700"
            : "text-red-600",
      )}
    >
      <Icon className="h-3 w-3 shrink-0" />
      {verdict.text}
    </div>
  );
}
