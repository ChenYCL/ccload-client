import { useT } from "../i18n";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  Plus,
  Trash2,
  X,
  Zap,
} from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import type { ForcedRoute, ForcedTarget } from "../types";
import { errText } from "../lib/err";
import { Modal } from "../components/Modal";
import { ComboBox } from "../components/ui/ComboBox";
import { Select, TextInput } from "../components/ui/Input";
import {
  kernelAliases,
  upstreamModelsOf,
  type ChannelModels,
} from "../lib/modelOptions";

/// 强制路由 —— ai-go 那种「CLI 请求某个模型，就把它钉到你选的渠道 + 上游模型」。
///
/// 和「模型链」是两个概念：模型链讲优雅降级、要校验上游；这里**不校验上游，照发**
/// —— 级联下拉只是给候选，手填任意模型名照样存、照样发。落到渠道上和模型链走同一
/// 条写渠道路径（后端 `channel_writer::patch_channel`），只是这一页不谈校验。

/// 目标在路由里的角色。第一个是首选，其余是备用（首选冷却/故障时才轮到）。
/// 不再显示具体优先级数字 —— 真实优先级在**应用时**按现有服务该别名的渠道计算
/// （要压过它们才算独占），保存时给不出准数，给了反而误导。
const roleLabel = (t: ReturnType<typeof useT>, i: number) =>
  i === 0 ? t("首选") : t("备用 {n}", { n: i });

/// 编辑器 / 列表页共用的渠道字段。`enabled` 是关键：被禁用的渠道内核根本不会选，
/// 钉在上面的目标等于不存在。
type ChannelLite = ChannelModels & { name?: string };

export function ForcedRoutePage() {
  const t = useT();
  const qc = useQueryClient();
  const routes = useQuery({ queryKey: ["forced-routes"], queryFn: api.forcedRouteList });
  const channels = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.admin<unknown[]>("GET", "channels"),
  });
  const save = useMutation({
    mutationFn: (r: ForcedRoute) => api.forcedRouteSave(r),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["forced-routes"] }),
  });
  const remove = useMutation({
    mutationFn: (from: string) => api.forcedRouteDelete(from),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["forced-routes"] }),
  });
  const apply = useMutation({
    mutationFn: (from: string) => api.forcedRouteApply(from),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["channels"] }),
  });

  const [editing, setEditing] = useState<ForcedRoute | null>(null);
  const channelList = ((channels.data?.data ?? []) as ChannelLite[]).filter(
    (c) => c.id !== undefined,
  );

  return (
    <div>
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="t-display">{t("强制路由")}</h1>
          <p className="mt-1 max-w-3xl text-sm text-muted">
            {t("CLI 请求某个模型名，就强制把它发到你选的渠道 + 上游模型。选一个渠道，联动列出它的模型，勾多个即可 —— 不校验上游，手填任意名字照样发。和「模型链」的区别是心智：那边讲主力冷了往下降级，这里是「我说发去哪就发去哪」。")}
          </p>
        </div>
        <button
          onClick={() => setEditing({ from: "", targets: [] })}
          className="flex shrink-0 items-center gap-1 rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90"
        >
          <Plus className="h-4 w-4" /> {t("新建路由")}
        </button>
      </div>

      {routes.data && routes.data.length === 0 && (
        <p className="mt-6 text-sm text-muted">
          {t("还没有路由。点「新建路由」把第一个别名钉到一个渠道+模型上。")}
        </p>
      )}

      <ul className="mt-6 space-y-3">
        {(routes.data ?? []).map((r) => (
          <li key={r.from} className="card p-4">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <div className="font-mono text-sm font-medium">{r.from}</div>
                <TargetStrip targets={r.targets} channels={channelList} />
              </div>
              <div className="flex shrink-0 items-center gap-2">
                <button
                  onClick={() => apply.mutate(r.from)}
                  disabled={apply.isPending}
                  title={t("把这条路由写进各目标渠道")}
                  className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
                >
                  <Zap className="h-3 w-3" /> {t("应用")}
                </button>
                <button
                  onClick={() => setEditing(r)}
                  className="rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2"
                >
                  {t("编辑")}
                </button>
                <button
                  onClick={() => remove.mutate(r.from)}
                  aria-label={t("删除路由 {from}", { from: r.from })}
                  className="rounded-md border border-border px-2 py-1 text-xs text-red-600 hover:bg-surface-2"
                >
                  <Trash2 className="h-3 w-3" />
                </button>
              </div>
            </div>
          </li>
        ))}
      </ul>

      {apply.isError && <p className="mt-3 text-sm text-red-600">{errText(apply.error)}</p>}
      {apply.data && (
        <div className="mt-3 rounded-lg border border-border bg-surface-2/60 p-3 font-mono text-[11px] leading-relaxed text-muted">
          {apply.data.map((line, i) => (
            <div key={i}>{line}</div>
          ))}
        </div>
      )}
      {save.isError && <p className="mt-3 text-sm text-red-600">{errText(save.error)}</p>}

      {editing && (
        <RouteEditor
          route={editing}
          channels={channelList}
          onClose={() => setEditing(null)}
          onSave={(r) => save.mutate(r)}
        />
      )}
    </div>
  );
}

/// 列表页上一条路由的目标摘要。禁用的渠道直接划掉 —— 那个目标是死的，不主动看
/// 就要等真发请求那天才发现。
function TargetStrip({
  targets,
  channels,
}: {
  targets: ForcedTarget[];
  channels: ChannelLite[];
}) {
  const t = useT();
  if (targets.length === 0) {
    return <div className="mt-1 text-xs text-muted">{t("（还没有目标）")}</div>;
  }
  return (
    <ol className="mt-1.5 flex flex-wrap items-center gap-x-1 gap-y-1.5">
      {targets.map((tg, i) => {
        const ch = tg.channel_id == null ? undefined : channels.find((c) => c.id === tg.channel_id);
        const dead = ch?.enabled === false;
        const unbound = tg.channel_id == null;
        return (
          <li key={i} className="flex items-center gap-1">
            {i > 0 && <span className="text-muted/60">→</span>}
            <span
              className={cn(
                "flex items-center gap-1.5 rounded-lg border px-2 py-0.5",
                dead || unbound
                  ? "border-amber-500/40 bg-amber-500/10"
                  : "border-border bg-surface-2/60",
              )}
              title={
                t("第 {n} 个", { n: i + 1 }) +
                " · " +
                roleLabel(t, i) +
                (dead ? t(" · 渠道已禁用，不会被选中") : "") +
                (unbound ? t(" · 没绑渠道，应用时跳过") : "")
              }
            >
              <span className={cn("font-mono text-[11px]", dead && "text-amber-700 line-through")}>
                {tg.model}
              </span>
              <span className="text-[10px] text-muted">
                {tg.channel_name ?? t("未绑渠道")}
              </span>
            </span>
          </li>
        );
      })}
    </ol>
  );
}

function RouteEditor({
  route,
  channels,
  onClose,
  onSave,
}: {
  route: ForcedRoute;
  channels: ChannelLite[];
  onClose: () => void;
  onSave: (r: ForcedRoute) => void;
}) {
  const t = useT();
  const [draft, setDraft] = useState<ForcedRoute>(route);

  // 级联多选的当前渠道，和这个渠道下勾中的模型 + 手填的一个。
  const [pickChannel, setPickChannel] = useState<number | null>(null);
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [custom, setCustom] = useState("");

  const channelOf = (id: number | null | undefined) =>
    id == null ? undefined : channels.find((c) => c.id === id);
  const pickedChannel = channelOf(pickChannel);
  // 级联出来的候选：该渠道已配的上游模型名。不校验上游，所以这只是候选，不是白名单。
  const cascadeModels = useMemo(() => upstreamModelsOf(pickedChannel), [pickedChannel]);
  // from 的候选：全部启用渠道对外提供的别名（CLI 侧写的名字）。手填也行。
  const fromCandidates = useMemo(() => kernelAliases(channels), [channels]);

  const setTargets = (targets: ForcedTarget[]) => setDraft({ ...draft, targets });
  const removeTarget = (i: number) => setTargets(draft.targets.filter((_, j) => j !== i));
  const moveTarget = (i: number, dir: -1 | 1) => {
    const j = i + dir;
    if (j < 0 || j >= draft.targets.length) return;
    const next = [...draft.targets];
    [next[i], next[j]] = [next[j], next[i]];
    setTargets(next);
  };

  const toggle = (model: string) =>
    setChecked((cur) => {
      const next = new Set(cur);
      if (next.has(model)) next.delete(model);
      else next.add(model);
      return next;
    });

  /// 把当前渠道下勾中的（加上手填的那个）作为目标加入。同渠道+同模型已存在的跳过，
  /// 不重复。加完清空选择，方便切到下一个渠道继续凑。
  const addSelected = () => {
    if (pickChannel == null) return;
    const models = [...checked];
    const typed = custom.trim();
    if (typed) models.push(typed);
    if (models.length === 0) return;
    const name = pickedChannel?.name ?? null;
    const existing = new Set(draft.targets.map((tg) => `${tg.channel_id} ${tg.model}`));
    const additions: ForcedTarget[] = [];
    for (const model of models) {
      const key = `${pickChannel} ${model}`;
      if (existing.has(key)) continue;
      existing.add(key);
      additions.push({ channel_id: pickChannel, channel_name: name, model });
    }
    setTargets([...draft.targets, ...additions]);
    setChecked(new Set());
    setCustom("");
  };

  const canSave = draft.from.trim().length > 0 && draft.targets.length > 0;

  return (
    <Modal onClose={onClose} className="max-w-3xl">
      <>
        <h2 className="t-title">{t("编辑强制路由")}</h2>
        <p className="mt-1 text-xs text-muted">
          {t("命中「请求别名」就强制发到下面的目标。多个目标按序：第一个是首选，命中即用，后面的是备用落点。应用时会把目标排到现有服务该别名的渠道之上，确保独占而不是被平分。")}
        </p>

        <label className="mt-4 block text-xs">
          <div className="text-muted">{t("请求别名（CLI 里写的模型名）")}</div>
          <ComboBox
            className="mt-1"
            aria-label={t("请求别名")}
            value={draft.from}
            onChange={(v) => setDraft({ ...draft, from: v })}
            placeholder="claude-fable-5"
            options={fromCandidates}
            emptyHint={t("内核里还没有渠道；名字可以手填")}
          />
        </label>

        {/* 已选目标，按序。第一个优先级最高。 */}
        <div className="mt-4">
          <div className="text-xs text-muted">{t("目标（按序，第一个优先级最高）")}</div>
          {draft.targets.length === 0 ? (
            <p className="mt-1.5 rounded-lg border border-dashed border-border px-3 py-2 text-xs text-muted">
              {t("还没有目标。在下面选个渠道、勾几个模型，点「加入选中」。")}
            </p>
          ) : (
            <ol className="mt-1.5 space-y-1.5">
              {draft.targets.map((tg, i) => (
                <li
                  key={`${tg.channel_id}-${tg.model}-${i}`}
                  className="flex items-center gap-2 rounded-lg border border-border bg-surface-raised p-2"
                >
                  <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-accent/10 text-[11px] font-medium text-accent">
                    {i + 1}
                  </span>
                  <div className="min-w-0 flex-1">
                    <span className="font-mono text-xs">{tg.model}</span>
                    <span className="ml-2 text-[11px] text-muted">
                      {tg.channel_name ?? t("未绑渠道")}
                    </span>
                  </div>
                  <span className="shrink-0 text-[10px] text-muted/70">
                    {roleLabel(t, i)}
                  </span>
                  <button
                    onClick={() => moveTarget(i, -1)}
                    disabled={i === 0}
                    aria-label={t("上移")}
                    className="shrink-0 rounded-md border border-border p-1 text-muted hover:bg-surface-2 disabled:opacity-30"
                  >
                    <ArrowUp className="h-3 w-3" />
                  </button>
                  <button
                    onClick={() => moveTarget(i, 1)}
                    disabled={i === draft.targets.length - 1}
                    aria-label={t("下移")}
                    className="shrink-0 rounded-md border border-border p-1 text-muted hover:bg-surface-2 disabled:opacity-30"
                  >
                    <ArrowDown className="h-3 w-3" />
                  </button>
                  <button
                    onClick={() => removeTarget(i)}
                    aria-label={t("移除这个目标")}
                    className="shrink-0 rounded-md border border-border p-1 text-muted hover:bg-surface-2 hover:text-red-600"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </li>
              ))}
            </ol>
          )}
        </div>

        {/* 级联多选：选渠道 → 联动出它的模型 → 勾多个（或手填）→ 加入。 */}
        <div className="mt-4 rounded-xl border border-border bg-surface-2/40 p-3">
          <div className="text-xs font-medium">{t("批量添加目标")}</div>
          <div className="mt-2">
            <Select
              aria-label={t("目标渠道")}
              value={pickChannel ?? ""}
              onChange={(e) => {
                setPickChannel(e.target.value ? Number(e.target.value) : null);
                setChecked(new Set());
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
          </div>

          {pickChannel != null && (
            <>
              {cascadeModels.length > 0 ? (
                <div className="mt-2 flex flex-wrap gap-1.5">
                  {cascadeModels.map((m) => {
                    const on = checked.has(m);
                    return (
                      <button
                        key={m}
                        type="button"
                        onClick={() => toggle(m)}
                        className={cn(
                          "rounded-lg border px-2 py-1 font-mono text-[11px]",
                          on
                            ? "border-accent bg-accent/10 text-accent"
                            : "border-border text-muted hover:bg-surface-2",
                        )}
                      >
                        {on ? "☑ " : "☐ "}
                        {m}
                      </button>
                    );
                  })}
                </div>
              ) : (
                <p className="mt-2 text-[11px] text-muted">
                  {t("这个渠道还没配模型 —— 下面手填要发的模型名。")}
                </p>
              )}

              <div className="mt-2 flex items-center gap-2">
                <TextInput
                  mono
                  className="flex-1"
                  value={custom}
                  onChange={(e) => setCustom(e.target.value)}
                  placeholder={t("或手填一个模型名（不校验上游，照发）")}
                  aria-label={t("手填模型名")}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      addSelected();
                    }
                  }}
                />
                <button
                  onClick={addSelected}
                  disabled={checked.size === 0 && custom.trim().length === 0}
                  className="flex shrink-0 items-center gap-1 rounded-lg bg-accent px-3 py-1.5 text-xs font-medium text-white hover:bg-accent/90 disabled:opacity-40"
                >
                  <Plus className="h-3.5 w-3.5" />
                  {t("加入选中")}
                  {checked.size > 0 ? t("（{n}）", { n: checked.size }) : ""}
                </button>
              </div>
            </>
          )}
        </div>

        <div className="mt-5 flex items-center justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2"
          >
            {t("取消")}
          </button>
          <button
            onClick={() => {
              onSave({ ...draft, from: draft.from.trim() });
              onClose();
            }}
            disabled={!canSave}
            className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
          >
            {t("保存")}
          </button>
        </div>
      </>
    </Modal>
  );
}
