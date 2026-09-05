import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useT } from "../../i18n";
import { api } from "../../lib/api";
import { errText } from "../../lib/err";
import { sameAlias } from "../../lib/pins";
import type { Pin, RouteHit } from "../../types";
import { Select } from "../ui/Input";

/// 某个别名在内核里会落到哪些渠道 —— 「Grok Build 选了 grok-4.6 却一直在跑 glm」
/// 那类问题的诊断面，加上解法：**首选渠道钉住**。
///
/// 内核只按**渠道优先级**选路，没有 per-model 的优先级。一个高优先级渠道上多了一条
/// `grok-4.6 → glm-5.3-flash` 的改写（模型链 / 强制路由 / 内核后台手加，都可能），
/// 真正服务 grok-4.6 的 xAI 渠道就永远排在后面，日志里看起来像「fallback 了」，
/// 其实是主路由。把落点按优先级摆出来，第一条就是请求默认去的地方；跨了家族的
/// 改写标出来。
///
/// 「首选渠道」选了哪个，本地代理就先把模型名换成那个渠道的私有别名（`grok-4.6@ch21`，
/// 只有它认）发出去；内核回「没接住」类的失败再用原名重发、回到默认顺序 —— 这才是
/// 用户要的「选了哪个渠道就默认走它，别的只当备胎」。只在 CLI 走本地代理时生效。

/// 模型名的厂商家族。只认得出来的那几家 —— `Opus 5`、`fable`、`gb-deep` 这种虚拟
/// 别名本来就是要被改写成别家的，不该报警。
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

export function AliasRoutes({
  alias,
  proxyOn,
}: {
  alias: string | null | undefined;
  /** 「CLI 走本地代理」开着没有 —— 钉住只在代理那一层生效。 */
  proxyOn: boolean;
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
  const busy = toggle.isPending || savePin.isPending || deletePin.isPending;
  const outcome = savePin.data ?? deletePin.data;

  if (!name) return null;
  if (routes.isError) {
    return (
      <p className="mt-2 text-[11px] text-muted">
        {t("内核没连上，看不到「{alias}」在内核里的落点。", { alias: name })}
      </p>
    );
  }
  const hits = routes.data ?? [];
  if (routes.isSuccess && hits.length === 0) {
    return (
      <p className="mt-2 text-[11px] text-amber-700">
        {t("内核里没有任何启用渠道服务「{alias}」—— 请求会 503。去内核后台或「模型链」给它挂一个上游。", { alias: name })}
      </p>
    );
  }
  if (hits.length === 0) return null;

  const suspicious = crossFamilyRewrite(name, hits);
  const honest = hits.find((h) => !h.disabled && familyOf(h.upstream) === familyOf(name));
  const pinnedId = pin?.targets[0]?.channel_id ?? null;
  const choices = channelChoices(hits);
  // 钉住的渠道在内核里已经没了（删了 / 停用了 / 不再服务这个别名）：下拉里没有它，
  // 原生 select 会默默显示第一项「不钉住」，和实际状态相反。补一个占位项把真相摆出来。
  const orphan = pinnedId !== null && !choices.some((h) => h.channel_id === pinnedId);

  const choose = (value: string) => {
    if (value === "") {
      // 两个 mutation 的 data 都会留着；清掉另一个，日志才显示最近这次的。
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
      <div className="text-muted">{t("内核落点（按优先级，第一条是默认去处）")}</div>
      <ul className="mt-0.5 space-y-0.5">
        {hits.map((h) => (
          <li key={`${h.channel_id}:${h.alias}`} className="flex flex-wrap items-baseline gap-x-1.5">
            <span className="font-mono">{h.channel_name}</span>
            <span className="opacity-60">({h.priority})</span>
            <span className="opacity-60">→</span>
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
            <button
              type="button"
              disabled={busy}
              onClick={() => toggle.mutate(h)}
              className="rounded border border-border px-1.5 py-0 text-[10px] text-muted hover:bg-surface-2 hover:text-content disabled:opacity-40"
            >
              {h.disabled ? t("重新启用") : t("停用这条")}
            </button>
          </li>
        ))}
      </ul>

      {/* 首选渠道：默认只走它，别的渠道只当备胎。 */}
      <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1">
        <label className="text-muted">{t("首选渠道")}</label>
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
      {outcome && outcome.log.length > 0 && (
        <ul className="mt-1 space-y-0.5 text-muted">
          {outcome.log.map((line, i) => (
            <li key={i}>✓ {line}</li>
          ))}
        </ul>
      )}
      {(toggle.isError || savePin.isError || deletePin.isError) && (
        <p className="mt-1 text-red-600">{errText(toggle.error ?? savePin.error ?? deletePin.error)}</p>
      )}
    </div>
  );
}
