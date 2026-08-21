import { useT } from "../i18n";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import {
  AlertTriangle,
  ArrowDown,
  CheckCircle2,
  GripVertical,
  Plus,
  Radar,
  Trash2,
  X,
  XCircle,
  Zap,
} from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import type { FallbackChain, FallbackHop } from "../types";
import { errText } from "../lib/err";
import { Modal } from "../components/Modal";
import { useReorder } from "../lib/useReorder";
import { Select, TextInput } from "../components/ui/Input";

/// Multi-layer model fallback. The kernel only does one hop (redirect_model)
/// and then retries channels; this page turns a chain like
/// `fable-5 → kimi-k3 → opus-5 → glm-5.3` into N channels of descending
/// priority, so ccLoad's existing selector walks the chain automatically.

/// 与后端 `services::fallback::hop_priority` 同一个公式。写在这里是为了让用户在
/// 保存**之前**就看见每一层会被写成什么优先级 —— 这个值会改到渠道上，不该等
/// 应用完了再从日志里发现。两边任何一处改了，另一处必须跟着改。
const hopPriority = (i: number) => 100 - i * 10;

/// 编辑器里用得到的渠道字段。`enabled` 是关键：被禁用的渠道内核根本不会选，
/// 绑在上面的那一层等于不存在。
type ChannelLite = { id: number; name: string; enabled?: boolean };

/// `GET /admin/channels/:id/models/fetch` 的响应。内核按渠道声明的协议去问上游
/// 要真实清单，客户端不自己实现拉取。
type FetchModelsResponse = { models?: { model?: string }[] };

/// 一个渠道的上游清单探测结果。
type Probe = { models: string[]; err: string };

/// 一层的体检结论。
///
/// 存在的理由：这条链的每一层都只在**主力挂掉的那一刻**才会被用到，而那正是最不
/// 适合发现「渠道早就禁用了」「模型名拼错了」的时刻。这些数据内核本来就给得出，
/// 没道理留到运行时才炸。
type HopVerdict = { level: "error" | "warn" | "ok" | "idle"; text: string };

function hopVerdict(
  hop: FallbackHop,
  channel: ChannelLite | undefined,
  probe: Probe | undefined,
): HopVerdict {
  if (hop.channel_id == null) {
    return { level: "warn", text: "没绑渠道 · 应用时这一层会被跳过" };
  }
  if (!channel) {
    return { level: "error", text: `渠道 #${hop.channel_id} 已不存在` };
  }
  if (channel.enabled === false) {
    return { level: "error", text: "渠道已禁用 · 这一层永远不会被选中" };
  }
  if (!hop.upstream.trim()) return { level: "idle", text: "" };
  if (!probe) return { level: "idle", text: "" };
  if (probe.err) {
    return { level: "warn", text: `上游清单拉不到，无法校验：${probe.err}` };
  }
  return probe.models.includes(hop.upstream.trim())
    ? { level: "ok", text: "上游清单里有这个模型" }
    : {
        level: "error",
        text: `上游清单里没有 ${hop.upstream.trim()} · 请求打到这一层会直接失败`,
      };
}

export function FallbackPage() {
  const t = useT();
  const qc = useQueryClient();
  const chains = useQuery({ queryKey: ["fallback"], queryFn: api.fallbackList });
  const channels = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.admin<unknown[]>("GET", "channels"),
  });
  const save = useMutation({
    mutationFn: (c: FallbackChain) => api.fallbackSave(c),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["fallback"] }),
  });
  const remove = useMutation({
    mutationFn: (alias: string) => api.fallbackDelete(alias),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["fallback"] }),
  });
  const apply = useMutation({
    mutationFn: (alias: string) => api.fallbackApply(alias),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["channels"] }),
  });

  const [editing, setEditing] = useState<FallbackChain | null>(null);
  const channelList = ((channels.data?.data ?? []) as ChannelLite[]).filter(
    (c) => c.id !== undefined,
  );

  return (
    <div>
      <div className="flex items-center justify-between gap-4">
        <div>
          <h1 className="t-display">{t("模型链")}</h1>
          <p className="mt-1 text-sm text-muted">
            {t("ccLoad 内核只做一层模型重定向，然后按优先级切渠道。这里把一条 fallback 链（例如 fable-5 → kimi-k3 → opus-5）写成一组按优先级 递减的渠道，内核的选择器就会自动走完整个链。")}
          </p>
        </div>
        <button
          onClick={() =>
            setEditing({ alias: "", hops: [{ upstream: "", channel_id: null, channel_name: null }] })
          }
          className="flex shrink-0 items-center gap-1 rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90"
        >
          <Plus className="h-4 w-4" /> {t("新建链")}
        </button>
      </div>

      {chains.data && chains.data.length === 0 && (
        <p className="mt-6 text-sm text-muted">
          {t("还没有链。点「新建链」把第一个别名加进来。")}
        </p>
      )}

      <ul className="mt-6 space-y-3">
        {(chains.data ?? []).map((c) => (
          <li key={c.alias} className="card p-4">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <div className="font-medium">{c.alias}</div>
                <ChainStrip hops={c.hops} channels={channelList} />
              </div>
              <div className="flex shrink-0 items-center gap-2">
                <button
                  onClick={() => apply.mutate(c.alias)}
                  disabled={apply.isPending}
                  className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
                >
                  <Zap className="h-3 w-3" /> {t("应用")}
                </button>
                <button
                  onClick={() => setEditing(c)}
                  className="rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2"
                >
                  {t("编辑")}
                </button>
                <button
                  onClick={() => remove.mutate(c.alias)}
                  aria-label={`删除链 ${c.alias}`}
                  className="rounded-md border border-border px-2 py-1 text-xs text-red-600 hover:bg-surface-2"
                >
                  <Trash2 className="h-3 w-3" />
                </button>
              </div>
            </div>
          </li>
        ))}
      </ul>

      {apply.isError && (
        <p className="mt-3 text-sm text-red-600">{errText(apply.error)}</p>
      )}
      {save.isError && (
        <p className="mt-3 text-sm text-red-600">{errText(save.error)}</p>
      )}

      {editing && (
        <ChainEditor
          chain={editing}
          channels={channelList}
          onClose={() => setEditing(null)}
          onSave={(c) => save.mutate(c)}
        />
      )}
    </div>
  );
}

/// 列表页上的链摘要。以前是 `a → b → c` 一串纯文本，节点和箭头一样重，长一点
/// 就读不出「有几层、哪层在前」。这里用和编辑器同一套节点造型，横向排布、可换行。
function ChainStrip({ hops, channels }: { hops: FallbackHop[]; channels: ChannelLite[] }) {
  const t = useT();
  return (
    <ol className="mt-1.5 flex flex-wrap items-center gap-x-1 gap-y-1.5">
      {hops.map((h, i) => {
        // 「渠道被禁用」不用问上游就能知道，所以列表页也标出来 —— 这一层是死的，
        // 而链的性质决定了你不主动看就要等主力挂掉那天才发现。
        const ch = h.channel_id == null ? undefined : channels.find((c) => c.id === h.channel_id);
        const dead = ch?.enabled === false;
        return (
          <li key={i} className="flex items-center gap-1">
            {i > 0 && <span className="text-muted/60">→</span>}
            <span
              className={cn(
                "flex items-center gap-1.5 rounded-lg border px-2 py-0.5",
                dead
                  ? "border-red-500/40 bg-red-500/10"
                  : "border-border bg-surface-2/60",
              )}
              title={`第 ${i + 1} 层 · 优先级 ${hopPriority(i)}${
                h.channel_name ? ` · 渠道 ${h.channel_name}` : ""
              }${dead ? t(" · 渠道已禁用，这一层不会被选中") : ""}`}
            >
              {dead && <XCircle className="h-3 w-3 shrink-0 text-red-600" />}
              <span
                className={cn(
                  "font-mono text-[11px]",
                  dead && "text-red-700 line-through decoration-red-500/50",
                )}
              >
                {h.upstream || t("（未填）")}
              </span>
              {h.channel_name && (
                <span className={cn("text-[10px]", dead ? "text-red-700/80" : "text-muted")}>
                  {h.channel_name}
                </span>
              )}
            </span>
          </li>
        );
      })}
    </ol>
  );
}

/// 一层的体检结果条。只在有话说的时候占位置 —— 一切正常且没校验过时不显示，
/// 免得四层各挂一条「暂无问题」把编辑器撑长。
function HopStatus({ verdict }: { verdict: HopVerdict }) {
  if (verdict.level === "idle" || !verdict.text) return null;
  const Icon =
    verdict.level === "ok" ? CheckCircle2 : verdict.level === "warn" ? AlertTriangle : XCircle;
  return (
    <div
      className={cn(
        "mt-1 flex items-center gap-1 pl-[3.4rem] text-[11px]",
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

function ChainEditor({
  chain,
  channels,
  onClose,
  onSave,
}: {
  chain: FallbackChain;
  channels: ChannelLite[];
  onClose: () => void;
  onSave: (c: FallbackChain) => void;
}) {
  const t = useT();
  const [draft, setDraft] = useState<FallbackChain>(chain);

  const setHops = (hops: FallbackHop[]) => setDraft({ ...draft, hops });
  const setHop = (i: number, patch: Partial<FallbackHop>) => {
    const hops = [...draft.hops];
    hops[i] = { ...hops[i], ...patch };
    setHops(hops);
  };
  const addHop = () =>
    setHops([...draft.hops, { upstream: "", channel_id: null, channel_name: null }]);
  const removeHop = (i: number) => setHops(draft.hops.filter((_, j) => j !== i));

  const channelList = channels;
  const channelOf = (id: number | null | undefined) =>
    id == null ? undefined : channelList.find((c) => c.id === id);

  // 上游探测按需触发，不在打开编辑器时自动跑：每个渠道一次真实的上游请求，
  // 有的要好几秒、有的按次计费，不该因为用户点了「编辑」就替他花掉。
  const [probes, setProbes] = useState<Record<number, Probe>>({});
  const check = useMutation({
    mutationFn: async () => {
      const ids = [...new Set(draft.hops.map((h) => h.channel_id).filter((id): id is number => id != null))];
      const pairs = await Promise.all(
        ids.map(async (id) => {
          try {
            const r = await api.admin<FetchModelsResponse>("GET", `channels/${id}/models/fetch`);
            const models = (r.data?.models ?? []).map((m) => m.model ?? "").filter(Boolean);
            // 内核对拉取失败是返回 200 + success=false 的（上游报错属预期内），
            // 所以「拿到 0 个」要当成「没给出清单」，不能当成空集去判定「都不存在」——
            // 那会把每一层都误报成红的。
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

  const reorder = useReorder(draft.hops, setHops);

  return (
    <Modal onClose={onClose} className="max-w-3xl">
      <>
        <h2 className="t-title">{t("编辑模型链")}</h2>
        <p className="mt-1 text-xs text-muted">
          {t("从上到下依次尝试。上面的层优先级更高，拖住左侧手柄可以换顺序 （也可以聚焦手柄后按 ↑ ↓）。")}
        </p>

        <label className="mt-4 block text-xs">
          <div className="text-muted">{t("别名（CLI 里写的模型名）")}</div>
          <TextInput
            value={draft.alias}
            onChange={(e) => setDraft({ ...draft, alias: e.target.value })}
            placeholder="fable-5"
            className="mt-1"
          />
        </label>

        {/* 管线本体。节点之间那条竖线 + 箭头是「失败了就往下走」的唯一视觉线索，
            没有它这就只是一列长得一样的表单行。 */}
        <ol ref={reorder.listRef} className="relative mt-4 space-y-2">
          {draft.hops.map((hop, i) => {
            const dragging = reorder.drag?.from === i;
            return (
              <li
                key={i}
                style={{ transform: `translateY(${reorder.offsetOf(i)}px)` }}
                className={cn(
                  "relative rounded-xl border bg-surface-raised p-2.5",
                  // 被拖的节点不加过渡（要跟手），其余节点让位要有过渡才看得懂。
                  dragging
                    ? "z-10 border-accent/50 shadow-[var(--shadow-raised)]"
                    : "border-border transition-transform duration-[180ms] ease-[cubic-bezier(0.32,0.72,0,1)]",
                )}
              >
                {/* 连接线：画在节点之间的空隙里，最后一层没有下一层就不画。 */}
                {i < draft.hops.length - 1 && !reorder.drag && (
                  <span
                    aria-hidden
                    className="absolute -bottom-2 left-[1.85rem] flex h-2 w-px items-center justify-center bg-border"
                  >
                    <ArrowDown className="h-2.5 w-2.5 shrink-0 text-border" />
                  </span>
                )}

                <div className="flex items-center gap-2">
                  <button
                    onPointerDown={reorder.start(i)}
                    onKeyDown={reorder.onKeyDown(i)}
                    aria-label={`第 ${i + 1} 层，拖动或按上下键调整顺序`}
                    title={t("拖动排序")}
                    className="flex cursor-grab touch-none items-center rounded-md p-1 text-muted hover:bg-surface-2 active:cursor-grabbing"
                  >
                    <GripVertical className="h-4 w-4" />
                  </button>

                  <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-accent/10 text-[11px] font-medium text-accent">
                    {i + 1}
                  </span>

                  <TextInput
                    mono
                    value={hop.upstream}
                    onChange={(e) => setHop(i, { upstream: e.target.value })}
                    placeholder={t("上游模型，例如 kimi-k3")}
                    className="flex-1"
                  />

                  <Select
                    className="w-52 shrink-0"
                    aria-label={`第 ${i + 1} 层的渠道`}
                    value={hop.channel_id ?? ""}
                    onChange={(e) => {
                      const id = e.target.value ? Number(e.target.value) : null;
                      setHop(i, {
                        channel_id: id,
                        // 名字一并存下来，列表页不必再去查一次渠道表。
                        channel_name:
                          channelList.find((c) => c.id === id)?.name ?? null,
                      });
                    }}
                  >
                    <option value="">{t("选择渠道")}</option>
                    {channelList.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name} (#{c.id})
                      </option>
                    ))}
                  </Select>

                  <button
                    onClick={() => removeHop(i)}
                    disabled={draft.hops.length <= 1}
                    aria-label={`删除第 ${i + 1} 层`}
                    className="shrink-0 rounded-md border border-border p-1.5 text-muted hover:bg-surface-2 hover:text-red-600 disabled:opacity-40"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>

                <div className="mt-1 pl-[3.4rem] text-[11px] text-muted">
                  {t("应用后会把该渠道的优先级写成")} {hopPriority(i)}
                  <span className="text-muted/70">{t("（影响该渠道服务的所有模型）")}</span>
                </div>
                <HopStatus
                  verdict={hopVerdict(
                    hop,
                    channelOf(hop.channel_id),
                    hop.channel_id == null ? undefined : probes[hop.channel_id],
                  )}
                />
              </li>
            );
          })}
        </ol>

        <div className="mt-3 flex flex-wrap items-center gap-2">
          <button
            onClick={addHop}
            className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 text-xs hover:bg-surface-2"
          >
            <Plus className="h-3.5 w-3.5" /> {t("添加一层")}
          </button>
          {/* 上游校验。内核已经能「按渠道声明的协议去问上游要模型清单」
              （GET /admin/channels/:id/models/fetch），这里只负责把它拉回来、
              和每一层的 redirect 目标对一下。 */}
          <button
            onClick={() => check.mutate()}
            disabled={check.isPending}
            title={t("逐个渠道去问上游要真实模型清单，核对每一层的模型名")}
            className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 text-xs hover:bg-surface-2 disabled:opacity-40"
          >
            <Radar className="h-3.5 w-3.5" />
            {check.isPending ? t("校验中…") : t("校验上游模型")}
          </button>
          {check.isError && (
            <span className="text-[11px] text-red-600">{errText(check.error)}</span>
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
              onSave(draft);
              onClose();
            }}
            className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90"
          >
            {t("保存")}
          </button>
        </div>
      </>
    </Modal>
  );
}
