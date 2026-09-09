import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useT } from "../../i18n";
import { api } from "../../lib/api";
import { cn } from "../../lib/cn";
import { errText } from "../../lib/err";
import { aliasKey, sameAlias } from "../../lib/pins";
import { upstreamModelsOf, type ChannelModels } from "../../lib/modelOptions";
import type { Pin, RouteHit } from "../../types";
import { ComboBox } from "../ui/ComboBox";
import { Select } from "../ui/Input";

/// 一个别名在内核里**现在**落到哪些渠道 —— 内核侧的真身，可直接改。
///
/// 为什么这一块要能编辑：内核只按**渠道优先级**选路，没有 per-model 优先级。一个
/// 高优先级渠道上多了一条 `claude-fable-5-1 → claude-opus-5` 的改写（本地编排、
/// 内核后台手加都可能），真正服务它的渠道就永远排在后面，日志里看着像故障转移，
/// 其实是主路由。看到这件事之后，下一步一定是「那我把它改回来」—— 以前只能去内核
/// 后台翻渠道表单，现在就在这一行上改。
///
/// 三种写入都走 `channel_model_set` / `channel_model_remove`（后端 `patch_channel`，
/// 和模型路由的「应用」同一条路径，凭据回传那些坑只处理一次）。它改的是**内核现状**，
/// 不落任何本地编排文件 —— 想让它可重复应用，用上面那张「编排」卡。
///
/// 首选渠道钉住是另一回事：那是本地代理那一层的规则（先发私有别名 `x@ch15`），
/// 不改内核，只在 CLI 走本地代理时生效。

/// 模型名的厂商家族。只认得出来的那几家 —— `Opus 5`、`fable` 这种虚拟别名本来
/// 就是要被改写成别家的，不该报警。
const FAMILIES = ["grok", "glm", "gpt", "claude", "gemini", "kimi", "deepseek", "qwen", "llama", "mistral"];

export function familyOf(name: string): string | null {
  let s = name.trim().toLowerCase();
  const slash = s.lastIndexOf("/");
  if (slash >= 0) s = s.slice(slash + 1);
  const m = /^([a-z]+)/.exec(s);
  if (!m) return null;
  return FAMILIES.includes(m[1]) ? m[1] : null;
}

/// 优先级最高的活动落点把别名改写成了**另一家**的模型。同家族内的改写
/// （claude-opus-4-8 → claude-fable-5）是正常的版本映射，不算。
export function crossFamilyRewrite(alias: string, hits: RouteHit[]): RouteHit | null {
  const top = hits.find((h) => !h.disabled);
  if (!top) return null;
  const a = familyOf(alias);
  const b = familyOf(top.upstream);
  return a && b && a !== b ? top : null;
}

/// 同一个渠道可能以两种写法（带/不带后缀）服务同一个别名，选渠道时按渠道去重。
export function channelChoices(hits: RouteHit[]): RouteHit[] {
  const seen = new Set<number>();
  return hits.filter((h) => (seen.has(h.channel_id) ? false : (seen.add(h.channel_id), true)));
}

/// 剥掉窗口后缀 `[1M]`，**保留大小写和 thinking 后缀**。
///
/// 和 `aliasKey` 不同：那个是拿来做匹配的（顺手小写），这个是拿**写进内核**的
/// —— 新增一条落点时要用内核认的那个名字，`[1M]` 是 CLI 侧的约定，写进去就成了
/// 另一个别名。
function baseAlias(name: string): string {
  let s = name.trim();
  for (;;) {
    const next = s.endsWith("]") && s.lastIndexOf("[") > 0 ? s.slice(0, s.lastIndexOf("[")).trimEnd() : s;
    if (next === s) return s;
    s = next;
  }
}

/// 「CLI 接管」页上的一行落点摘要。
///
/// 那一页管的是**这个 CLI 发什么名字**，落点是另一层的事；但「选了 A 却一直在跑
/// B」偏偏要在这两层之间才看得出来，所以这里留一行结论 + 一个跳转，完整的编辑面
/// 在「模型路由」。以前整块可编辑的落点面板挂在每张 CLI 卡片下面，五张卡片就是
/// 五份同样的控件，改哪一份都一样 —— 那是重复，不是方便。
export function AliasLanding({
  alias,
  onOpen,
}: {
  alias: string | null | undefined;
  onOpen: (alias: string) => void;
}) {
  const t = useT();
  const name = alias?.trim() ?? "";
  const routes = useQuery({
    queryKey: ["alias-routes", name],
    queryFn: () => api.aliasRoutes(name),
    enabled: name.length > 0,
  });
  if (!name || routes.isError) return null;
  const hits = routes.data ?? [];
  if (routes.isSuccess && hits.length === 0) {
    return (
      <p className="mt-2 text-[11px] text-amber-700">
        {t("内核里没有任何启用渠道服务「{alias}」—— 请求会 503。", { alias: name })}{" "}
        <button onClick={() => onOpen(name)} className="text-accent hover:underline">
          {t("去模型路由挂一个")}
        </button>
      </p>
    );
  }
  const top = hits.find((h) => !h.disabled);
  if (!top) return null;
  const suspicious = crossFamilyRewrite(name, hits);
  const others = hits.filter((h) => !h.disabled).length - 1;

  return (
    <div className="mt-2 flex flex-wrap items-baseline gap-x-1.5 text-[11px]">
      <span className="text-muted">{t("内核落点")}</span>
      <span className="font-mono">{top.channel_name}</span>
      <span className="opacity-60">→</span>
      <span className="font-mono">{top.upstream}</span>
      {others > 0 && <span className="text-muted">{t("· 另有 {n} 个备选", { n: others })}</span>}
      {suspicious && <span className="text-amber-700">{t("⚠ 改写成了别家的模型")}</span>}
      <button
        onClick={() => onOpen(name)}
        className="rounded border border-border px-1.5 py-0 text-[10px] text-muted hover:bg-surface-2 hover:text-content"
      >
        {t("改落点")}
      </button>
    </div>
  );
}

export function AliasRoutes({
  alias,
  proxyOn,
  channels,
}: {
  alias: string | null | undefined;
  /** 「CLI 走本地代理」开着没有 —— 钉住只在代理那一层生效。 */
  proxyOn: boolean;
  /** 渠道清单，页面已经拉过。给「改上游 / 加渠道」提供候选和渠道名。 */
  channels?: ChannelModels[];
}) {
  const t = useT();
  const qc = useQueryClient();
  const name = alias?.trim() ?? "";
  const routes = useQuery({
    queryKey: ["alias-routes", name],
    queryFn: () => api.aliasRoutes(name),
    enabled: name.length > 0,
  });
  const pins = useQuery({ queryKey: ["pins"], queryFn: api.pinList });
  const pin = pins.data?.find((p) => sameAlias(p.alias, name));

  // 行内编辑态：改哪一条的落点、改成什么。同一时刻只开一行。
  const [editing, setEditing] = useState<{ channel: number; value: string } | null>(null);
  // 删除是两步的：先点「删除」把按钮变成确认，再点一次才真删。整块落点里
  // 只有它会造成「请求直接 503」，不该一步到位。
  const [confirming, setConfirming] = useState<number | null>(null);
  const [adding, setAdding] = useState(false);
  const [addChannel, setAddChannel] = useState<number | null>(null);
  const [addModel, setAddModel] = useState("");

  const invalidate = () => {
    qc.invalidateQueries({ queryKey: ["alias-routes"] });
    qc.invalidateQueries({ queryKey: ["channels"] });
    qc.invalidateQueries({ queryKey: ["pins"] });
    // 落点变了，窗口的最窄口径也变了。
    qc.invalidateQueries({ queryKey: ["context-window-preview"] });
    qc.invalidateQueries({ queryKey: ["context-tiers"] });
    qc.invalidateQueries({ queryKey: ["cli-preview"] });
  };
  const toggle = useMutation({
    // 内核 PUT /channels/:id 收到**只有** {model, disabled} 两个键的 body 时走的是
    // 「切这一条模型条目」的分支，不会当成整体更新；别往里多塞任何字段。
    mutationFn: (h: RouteHit) =>
      api.admin("PUT", `channels/${h.channel_id}`, {
        body: { model: h.alias, disabled: !h.disabled },
      }),
    onSuccess: invalidate,
  });
  const savePin = useMutation({ mutationFn: (p: Pin) => api.pinSave(p), onSuccess: invalidate });
  const deletePin = useMutation({ mutationFn: () => api.pinDelete(name), onSuccess: invalidate });
  /// 把钉住的私有别名重新写回内核。整表重写，不只这一条 —— 会被清掉的从来不止一条。
  const repairPin = useMutation({ mutationFn: () => api.pinResync(), onSuccess: invalidate });
  const write = useMutation({
    mutationFn: (v: { channel: number; entry: string; upstream: string }) =>
      api.channelModelSet(v.channel, v.entry, v.upstream),
    onSuccess: () => {
      setEditing(null);
      setAdding(false);
      setAddChannel(null);
      setAddModel("");
      // 四个 mutation 的 data 都会留着，下面的日志按固定顺序取第一个非空的。
      // 不清掉别人的话，屏幕上会一直挂着上一次操作的结论。
      drop.reset();
      savePin.reset();
      deletePin.reset();
      invalidate();
    },
  });
  const drop = useMutation({
    mutationFn: (v: { channel: number; entry: string }) =>
      api.channelModelRemove(v.channel, v.entry),
    onSuccess: () => {
      setConfirming(null);
      write.reset();
      savePin.reset();
      deletePin.reset();
      invalidate();
    },
  });
  const busy = toggle.isPending || savePin.isPending || deletePin.isPending || write.isPending || drop.isPending;
  const outcome = savePin.data ?? deletePin.data;
  const log = write.data ?? drop.data ?? outcome?.log ?? [];

  if (!name) return null;
  if (routes.isError) {
    return (
      <p className="mt-2 text-[11px] text-muted">
        {t("内核没连上，看不到「{alias}」在内核里的落点。", { alias: name })}
      </p>
    );
  }
  const hits = routes.data ?? [];
  // 还没取回来（首屏 / 换别名的那一瞬）：什么都不画，别闪一下「没人服务它」。
  if (!routes.isSuccess) return null;

  const suspicious = crossFamilyRewrite(name, hits);
  const honest = hits.find((h) => !h.disabled && familyOf(h.upstream) === familyOf(name));
  const pinnedId = pin?.targets[0]?.channel_id ?? null;
  const choices = channelChoices(hits);
  // 钉住的渠道在内核里已经没了（删了 / 停用了 / 不再服务这个别名）：下拉里没有它，
  // 原生 select 会默默显示第一项「不钉住」，和实际状态相反。补一个占位项把真相摆出来。
  const orphan = pinnedId !== null && !choices.some((h) => h.channel_id === pinnedId);
  // 钉住写进内核的那条私有别名（`claude-opus-5@ch15`）还在不在。
  //
  // 它住在渠道的 `models[]` 里，而那张表**会被别人整体重写**：「模型桥接」页的
  // 「同步渠道模型清单 · 覆盖」拿上游返回的清单替换它，上游当然不会返回我们编的
  // `@ch15`；在内核后台改渠道、导 CSV 也一样。没了之后钉住不报错，只是每条请求
  // 先白挨一个 503 再用原名重发 —— 日志里就是一对对的「503 首选 / 200」，
  // 0ms、0 token、$0，纯浪费一个往返，还把日志刷满红色。
  const brokenPin =
    pin?.targets.some((tg) => {
      const ch = (channels ?? []).find((c) => c.id === tg.channel_id);
      // 渠道整个没了归上面的 orphan 管，这里只判「渠道在、私有条目没了」。
      if (!ch) return false;
      // 两边都小写再比：写进内核的那条保留原大小写（routing_base 不改），
      // aliasKey 会小写。
      const want = `${aliasKey(pin.alias)}@ch${tg.channel_id}`;
      return !(ch.models ?? []).some((m) => (m.model ?? "").trim().toLowerCase() === want);
    }) ?? false;
  // 钉住表里存的落点是点下拉那一刻的快照；渠道条目之后在内核后台被改过，快照就过期了。
  const stale = (() => {
    if (!pin || pinnedId === null) return null;
    const live = hits.find((h) => h.channel_id === pinnedId && !h.disabled);
    if (!live) return null;
    const stored = pin.targets[0]?.upstream?.trim() ?? "";
    return stored && stored !== live.upstream.trim() ? live : null;
  })();
  // 新增落点写哪个名字：优先用内核里**已有的**拼写（照抄最不容易写歪），
  // 一条都没有时退回剥掉窗口后缀的入参。
  const entryName = hits[0]?.alias ?? baseAlias(name);
  const channelById = (id: number | null) => channels?.find((c) => c.id === id);
  // 「加渠道」的候选渠道：已经在服务这个别名的渠道没什么可加的，排掉。
  const addable = (channels ?? []).filter(
    (c) => c.id !== undefined && !hits.some((h) => h.channel_id === c.id),
  );

  const choose = (value: string) => {
    // 四个 mutation 的 data 都会留着；清掉其余的，日志才显示最近这次的。
    write.reset();
    drop.reset();
    if (value === "") {
      savePin.reset();
      if (pin) deletePin.mutate();
      return;
    }
    const h = choices.find((c) => String(c.channel_id) === value);
    if (!h) return;
    deletePin.reset();
    savePin.mutate({
      alias: name,
      targets: [{ channel_id: h.channel_id, channel_name: h.channel_name, upstream: h.upstream }],
      fallback: pin?.fallback ?? true,
    });
  };

  return (
    <div className="mt-2 text-[11px]">
      {/* 一条落点都没有：这正是「请求会 503」的状态，而下面那个「加一个渠道」
          就是解法，所以这一档**不能**早退 —— 早退等于把提示和解法分在两处，
          而解法那处根本不会渲染。 */}
      {hits.length === 0 ? (
        <p className="text-amber-700">
          {t("内核里没有任何启用渠道服务「{alias}」—— 请求会 503。在下面给它挂一个渠道，或去内核后台加。", { alias: name })}
        </p>
      ) : (
        <div className="text-muted">{t("内核落点（按优先级，第一条是默认去处）· 可直接改")}</div>
      )}
      <ul className="mt-0.5 space-y-0.5">
        {hits.map((h) => {
          const isEditing = editing?.channel === h.channel_id;
          return (
            <li key={`${h.channel_id}:${h.alias}`} className="flex flex-wrap items-baseline gap-x-1.5">
              <span className="font-mono">{h.channel_name}</span>
              <span className="opacity-60">({h.priority})</span>
              <span className="opacity-60">→</span>

              {isEditing ? (
                <>
                  <ComboBox
                    className="min-w-[14rem]"
                    aria-label={t("「{alias}」在 {channel} 上的上游模型", {
                      alias: name,
                      channel: h.channel_name,
                    })}
                    value={editing.value}
                    onChange={(v) => setEditing({ channel: h.channel_id, value: v })}
                    options={upstreamModelsOf(channelById(h.channel_id))}
                    placeholder={t("上游模型名")}
                    emptyHint={t("这个渠道还没配模型，直接填要发给上游的名字")}
                  />
                  <button
                    type="button"
                    disabled={busy || editing.value.trim().length === 0}
                    onClick={() =>
                      write.mutate({
                        channel: h.channel_id,
                        entry: h.alias,
                        upstream: editing.value.trim(),
                      })
                    }
                    className="rounded border border-accent/50 bg-accent/10 px-1.5 py-0 text-[10px] text-accent hover:bg-accent/20 disabled:opacity-40"
                  >
                    {t("保存")}
                  </button>
                  <button
                    type="button"
                    onClick={() => setEditing(null)}
                    className="rounded border border-border px-1.5 py-0 text-[10px] text-muted hover:bg-surface-2"
                  >
                    {t("取消")}
                  </button>
                </>
              ) : (
                <>
                  <span className={h.disabled ? "font-mono line-through opacity-50" : "font-mono"}>
                    {h.upstream}
                  </span>
                  {h.disabled && <span className="opacity-60">{t("已停用")}</span>}
                  {pinnedId === h.channel_id && (
                    <span className="rounded bg-accent/15 px-1 text-[10px] text-accent">{t("首选")}</span>
                  )}
                  {suspicious === h && pinnedId === null && (
                    <span className="text-amber-700">{t("⚠ 改写成了别家的模型")}</span>
                  )}
                  {/* 停用的条目不给改落点：写入是把整条条目 upsert 掉（`model_entry`
                      只带 model / redirect_model），disabled 标记会跟着丢 —— 点一下
                      「保存」就把一条停用的改写悄悄启用了。先「重新启用」再改。 */}
                  <button
                    type="button"
                    disabled={busy || h.disabled}
                    title={h.disabled ? t("这条已停用。先「重新启用」再改落点。") : undefined}
                    onClick={() => setEditing({ channel: h.channel_id, value: h.upstream })}
                    className="rounded border border-border px-1.5 py-0 text-[10px] text-muted hover:bg-surface-2 hover:text-content disabled:opacity-40"
                  >
                    {t("改落点")}
                  </button>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => toggle.mutate(h)}
                    className="rounded border border-border px-1.5 py-0 text-[10px] text-muted hover:bg-surface-2 hover:text-content disabled:opacity-40"
                  >
                    {h.disabled ? t("重新启用") : t("停用这条")}
                  </button>
                  {confirming === h.channel_id ? (
                    <>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => drop.mutate({ channel: h.channel_id, entry: h.alias })}
                        className="rounded border border-red-400 bg-red-50 px-1.5 py-0 text-[10px] text-red-700 hover:bg-red-100 disabled:opacity-40"
                      >
                        {t("确认移除")}
                      </button>
                      <button
                        type="button"
                        onClick={() => setConfirming(null)}
                        className="rounded border border-border px-1.5 py-0 text-[10px] text-muted hover:bg-surface-2"
                      >
                        {t("取消")}
                      </button>
                    </>
                  ) : (
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => setConfirming(h.channel_id)}
                      title={t("把这个别名从这个渠道的模型清单里摘掉。内核里再没人服务它时，请求会 503。")}
                      className="rounded border border-border px-1.5 py-0 text-[10px] text-muted hover:bg-red-50 hover:text-red-700 disabled:opacity-40"
                    >
                      {t("移除")}
                    </button>
                  )}
                </>
              )}
            </li>
          );
        })}
      </ul>

      {/* 加一条落点：选渠道 → 填上游模型名（该渠道已有的模型当候选，也能手填）。 */}
      <div className="mt-1 flex flex-wrap items-center gap-1.5">
        {adding ? (
          <>
            <Select
              small
              aria-label={t("要加哪个渠道")}
              value={addChannel ?? ""}
              onChange={(e) => {
                setAddChannel(e.target.value ? Number(e.target.value) : null);
                setAddModel("");
              }}
              className="min-w-[11rem]"
            >
              <option value="">{t("选择渠道")}</option>
              {addable.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name ?? t("渠道")} (#{c.id})
                  {c.enabled === false ? t("（已禁用）") : ""}
                </option>
              ))}
            </Select>
            <ComboBox
              className="min-w-[14rem]"
              aria-label={t("要写进去的上游模型名")}
              value={addModel}
              onChange={setAddModel}
              options={upstreamModelsOf(channelById(addChannel))}
              placeholder={t("上游模型名，例如 claude-opus-5")}
              emptyHint={t("这个渠道还没配模型，直接填要发给上游的名字")}
            />
            <button
              type="button"
              disabled={busy || addChannel === null || addModel.trim().length === 0}
              onClick={() =>
                addChannel !== null &&
                write.mutate({ channel: addChannel, entry: entryName, upstream: addModel.trim() })
              }
              className="rounded border border-accent/50 bg-accent/10 px-1.5 py-0.5 text-[10px] text-accent hover:bg-accent/20 disabled:opacity-40"
            >
              {t("添加")}
            </button>
            <button
              type="button"
              onClick={() => {
                setAdding(false);
                setAddChannel(null);
                setAddModel("");
              }}
              className="rounded border border-border px-1.5 py-0.5 text-[10px] text-muted hover:bg-surface-2"
            >
              {t("取消")}
            </button>
          </>
        ) : (
          <button
            type="button"
            onClick={() => setAdding(true)}
            className="rounded border border-border px-1.5 py-0.5 text-[10px] text-muted hover:bg-surface-2 hover:text-content"
          >
            {t("+ 给「{alias}」加一个渠道", { alias: entryName })}
          </button>
        )}
      </div>

      {/* 首选渠道：默认只走它，别的渠道只当备胎。一个落点都没有、也没钉过的时候
          不显示 —— 那个下拉里只会有一项「不钉住」，占位置又不能干任何事。 */}
      <div
        className={cn(
          "mt-1.5 flex-wrap items-center gap-x-2 gap-y-1",
          choices.length === 0 && !pin ? "hidden" : "flex",
        )}
      >
        <label className="text-muted">{t("首选渠道（本地代理钉住）")}</label>
        <Select
          small
          value={pinnedId === null ? "" : String(pinnedId)}
          disabled={busy || pins.isLoading}
          onChange={(e) => choose(e.target.value)}
          className="min-w-[12rem]"
        >
          <option value="">{t("不钉住（内核默认顺序）")}</option>
          {orphan && pin && (
            <option value={String(pinnedId)} disabled>
              {t("{channel}（渠道已不在内核里）", { channel: pin.targets[0]?.channel_name || `#${pinnedId}` })}
            </option>
          )}
          {choices.map((h) => (
            <option key={h.channel_id} value={String(h.channel_id)}>
              {h.channel_name} → {h.upstream}
              {/* 落点和别名不是一个名字 = 这条渠道会把它改写成别的模型。那次
                  「选了 fable 却一直跑 opus」就是没看出这一点。 */}
              {aliasKey(h.upstream) !== aliasKey(name) ? `　${t("（改写）")}` : ""}
            </option>
          ))}
        </Select>
        {pin && (
          <label className="flex items-center gap-1 text-muted">
            <input
              type="checkbox"
              checked={pin.fallback}
              disabled={busy}
              onChange={(e) => {
                deletePin.reset();
                savePin.mutate({ ...pin, fallback: e.target.checked });
              }}
            />
            {t("首选不可用时退到内核默认顺序")}
          </label>
        )}
      </div>
      {pin && stale && (
        <p className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-lg border border-amber-500/40 bg-amber-500/10 px-2.5 py-1.5 leading-relaxed text-amber-900">
          <span>
            {t("钉住记录里的落点是 {stored}，但 {channel} 现在把它落到 {live}。重存一次让两边对齐 —— 否则以后任何一次重存都会把私有别名写回 {stored}。", {
              stored: pin.targets[0]?.upstream ?? "?",
              channel: stale.channel_name,
              live: stale.upstream,
            })}
          </span>
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              deletePin.reset();
              savePin.mutate(pin);
            }}
            className="rounded border border-amber-500/50 px-1.5 py-0 text-[10px] hover:bg-amber-500/15 disabled:opacity-40"
          >
            {t("同步")}
          </button>
        </p>
      )}
      {pin && !proxyOn && (
        <p className="mt-1 text-amber-700">
          {t("钉住只在 CLI 走本地代理时生效 —— 「CLI 走本地代理」现在是关的，请求仍按内核默认顺序走。")}
        </p>
      )}
      {orphan && (
        <p className="mt-1 text-amber-700">
          {t("钉住的渠道已不在内核里：开着退让时每次请求都先白挨一个 503 再退回默认顺序，关着退让时请求会一直失败。换一个首选渠道或取消钉住。")}
        </p>
      )}
      {!orphan && brokenPin && (
        <p className="mt-1 flex flex-wrap items-center gap-1.5 text-amber-700">
          {t("钉住的私有别名不在内核渠道里了（刷过模型清单之后常见）。现在每条请求都先白挨一个 503 再用原名重发 —— 日志里那一对对的「503 首选 / 200」就是它。")}
          <button
            onClick={() => repairPin.mutate()}
            disabled={repairPin.isPending}
            className="rounded border border-amber-500/50 px-1.5 py-0 text-[10px] hover:bg-amber-500/15 disabled:opacity-40"
          >
            {repairPin.isPending ? t("写回中…") : t("写回内核")}
          </button>
        </p>
      )}
      {pin && proxyOn && (
        <p className="mt-1 text-muted">
          {pin.fallback
            ? t("请求先发 {alias} 的私有别名只落到 {channel}；它冷却 / 限流 / 5xx 时用原名重发，回到上面的默认顺序。", {
                alias: name,
                channel: pin.targets[0]?.channel_name ?? "?",
              })
            : t("请求只落到 {channel}；它不可用时直接把错误交给 CLI，不退到别的渠道。", {
                channel: pin.targets[0]?.channel_name ?? "?",
              })}{" "}
          {t("钉住按别名生效：所有发「{alias}」的 CLI 都一样。", { alias: name })}
        </p>
      )}

      {suspicious && pinnedId === null && (
        <p className="mt-1.5 rounded-lg border border-amber-500/40 bg-amber-500/10 px-2.5 py-1.5 leading-relaxed text-amber-900">
          {t("你选的是 {alias}，但优先级最高的落点 {channel}（{prio}）会把它改写成 {upstream}。内核只按渠道优先级选路，这不是故障转移，是主路由。", {
            alias: name,
            channel: suspicious.channel_name,
            prio: suspicious.priority,
            upstream: suspicious.upstream,
          })}
          {honest
            ? " " +
              t("真正服务 {alias} 的 {channel}（{prio}）排在后面；把它设为首选渠道，或停用上面那条改写。", {
                alias: name,
                channel: honest.channel_name,
                prio: honest.priority,
              })
            : ""}
        </p>
      )}
      {log.length > 0 && (
        <ul className="mt-1 space-y-0.5 text-muted">
          {log.map((line, i) => (
            <li key={i}>✓ {line}</li>
          ))}
        </ul>
      )}
      {(toggle.isError || savePin.isError || deletePin.isError || write.isError || drop.isError) && (
        <p className="mt-1 text-red-600">
          {errText(toggle.error ?? savePin.error ?? deletePin.error ?? write.error ?? drop.error)}
        </p>
      )}
    </div>
  );
}
