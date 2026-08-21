import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Radar, RefreshCw } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import { useT, type Translate } from "../i18n";
import { fmtCost } from "../components/formatters";
import type { OAuthUsageWindow, SelfReportedUsage, UsageChannel } from "../types";

/// 订阅额度剩余。
///
/// 数据全部来自内核：它在刷新 OAuth 凭证时会顺带采样上游的额度端点，把每个
/// 额度窗口（5 小时 / 周 / 月）连同「本窗口累计标准成本」存进凭证，
/// `GET /admin/channels` 原样带出来。客户端不自己算额度 —— 每家上游的口径都
/// 不一样（Codex 按 percent、Z.ai 按 limits[]、xAI 按 cents），内核已经归一过
/// 一遍，再算一遍只会得出第二个不一致的答案。
///
/// 「刷新」是 `POST /admin/channels/:id/oauth-usage`：真的去问一次上游。它按次
/// 走网络、个别上游还限速，所以不自动轮询，只在用户点的时候跑。

/// 只有 OAuth 类渠道才有订阅额度。API Key 渠道是按量付费，没有「套餐剩余」
/// 这个概念，列出来只会是一排空卡片。
const OAUTH_AUTH_TYPES = new Set([
  "codex_oauth",
  "antigravity_oauth",
  "xai_oauth",
  "anthropic_oauth",
  "zai_oauth",
]);

/// 供应商标签是专有名词，中英文一样，不进词典。
const AUTH_LABELS: Record<string, string> = {
  codex_oauth: "Codex",
  antigravity_oauth: "Antigravity",
  xai_oauth: "xAI",
  anthropic_oauth: "Anthropic",
  zai_oauth: "Z.ai",
};

/// 上游额度名 → 它比「窗口时长」多说的那一点信息。
///
/// 为什么要这张表：`limit_name` 是上游自己的内部标识（`five_hour`、`seven_day`、
/// `time_limit`），直接显示会在中文界面里插进一串英文；而且它多数时候和左边按
/// `limit_window_seconds` 算出来的时长标签说的是同一件事 —— 界面上就成了
/// 「5 小时 five_hour」这种自己重复自己的话。
///
/// 所以映射到**差异**而不是名字本身：纯粹复述时长的映射成空串（不显示），
/// 只有真正区分同长度窗口的部分（Opus / Fable / 额度种类）才留下来。
/// Anthropic 的三个 7 天窗就是靠这一列区分的，抹掉它们会变成三行一模一样。
const LIMIT_EXTRA: Record<string, string> = {
  five_hour: "",
  seven_day: "",
  weekly: "",
  monthly: "",
  daily: "",
  hourly: "",
  seven_day_opus: "Opus",
  seven_day_sonnet: "Sonnet",
  seven_day_fable: "Fable",
  claude_fable: "Fable",
  claude_opus: "Opus",
  claude_sonnet: "Sonnet",
  weekly_credits: "额度",
  credits: "额度",
  time_limit: "时长",
  tokens_limit: "Token",
  count_limit: "次数",
};

/// 归一上游名：大小写、空格、连字符都可能出现（实测见过 `weekly credits`）。
function normalizeLimit(name: string): string {
  return name.trim().toLowerCase().replace(/[\s-]+/g, "_");
}

/// 认识的名字给差异文案，不认识的按 snake_case 拆开首字母大写 ——
/// 至少不再是一行 `time_limit`。
function limitExtra(name: string, t: Translate): string {
  const key = normalizeLimit(name);
  if (!key) return "";
  const known = LIMIT_EXTRA[key];
  if (known !== undefined) return known ? t(known) : "";
  return key
    .split("_")
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

/// 窗口时长 → 人话。内核给的是秒，上游各家的窗口长度并不整齐（Codex 的 5 小时
/// 是 18000，Anthropic 的 7 天窗是 604800），所以按量级归档而不是查表。
///
/// 拿不到时长时**不写「未知窗口」**：那句话既没信息又占着主标题的位置。改用
/// 额度名兜底 —— Z.ai 的 `time_limit` 显示成「时长」比「未知窗口」有用得多。
function windowLabel(w: OAuthUsageWindow, t: Translate): string {
  const seconds = w.limit_window_seconds;
  if (seconds <= 0) {
    return limitExtra(w.limit_name, t) || limitExtra(w.kind, t) || t("额度窗口");
  }
  const hours = seconds / 3600;
  if (hours < 1) return t("{n} 分钟", { n: Math.round(seconds / 60) });
  if (hours < 24) return t("{n} 小时", { n: Math.round(hours) });
  const days = Math.round(hours / 24);
  if (days === 7) return t("每周");
  if (days >= 28 && days <= 31) return t("每月");
  return t("{n} 天", { n: days });
}

/// 距离重置还有多久。上游给的是绝对时刻，用户关心的是「还要等多久」。
function untilReset(resetAt: number, t: Translate): string {
  if (!resetAt) return "";
  const sec = resetAt - Math.floor(Date.now() / 1000);
  if (sec <= 0) return t("即将重置");
  const h = Math.floor(sec / 3600);
  if (h < 1) return t("{n} 分钟后重置", { n: Math.max(1, Math.round(sec / 60)) });
  if (h < 48) return t("{n} 小时后重置", { n: h });
  return t("{n} 天后重置", { n: Math.round(h / 24) });
}

/// 剩余多少算危险。用剩余而不是已用：用户看的是「还能用多久」。
function tone(remaining: number): "ok" | "warn" | "bad" {
  if (remaining <= 5) return "bad";
  if (remaining <= 20) return "warn";
  return "ok";
}

const BAR: Record<string, string> = {
  ok: "bg-emerald-500",
  warn: "bg-amber-500",
  bad: "bg-red-500",
};
const TEXT: Record<string, string> = {
  ok: "text-emerald-700",
  warn: "text-amber-700",
  bad: "text-red-600",
};

export function UsagePage() {
  const t = useT();
  const qc = useQueryClient();
  const [message, setMessage] = useState<string | null>(null);

  const channels = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.admin<UsageChannel[]>("GET", "channels"),
  });

  // 非 OAuth 渠道：内核不给它们采样额度，但上游自己可能知道（cursor2Oauth
  // 这类代理手里有原厂凭证）。它们走「自报」那条路。
  const otherChannels = useMemo(
    () =>
      (channels.data?.data ?? []).filter(
        (c) => !(c.auth_type && OAUTH_AUTH_TYPES.has(c.auth_type)),
      ),
    [channels.data],
  );

  const probe = useMutation({
    mutationFn: () => api.channelUsageProbe(otherChannels.map((c) => c.id)),
    onSuccess: (r) => {
      if (r.found.length === 0 && r.errors.length === 0) {
        setMessage(t("这些渠道的上游都没有提供 /usage 接口。"));
      } else if (r.errors.length > 0) {
        setMessage(r.errors.join("\n"));
      } else {
        setMessage("");
      }
    },
    onError: (e) => setMessage(errText(e)),
  });

  const oauthChannels = useMemo(
    () =>
      (channels.data?.data ?? []).filter(
        (c) => c.auth_type && OAUTH_AUTH_TYPES.has(c.auth_type),
      ),
    [channels.data],
  );

  // 逐个渠道去问上游。串行还是并行：并行 —— 每个渠道打的是不同的上游，互相
  // 不抢锁，而串行会让 5 个渠道等成 5 倍时长。一个失败不影响其余。
  const refresh = useMutation({
    mutationFn: async (ids: number[]) =>
      Promise.all(
        ids.map(async (id) => {
          try {
            await api.admin("POST", `channels/${id}/oauth-usage`);
            return { id, err: "" };
          } catch (e) {
            return { id, err: errText(e) };
          }
        }),
      ),
    onSuccess: (rs) => {
      const failed = rs.filter((r) => r.err);
      setMessage(
        failed.length === 0
          ? t("已刷新 {n} 个渠道的额度", { n: rs.length })
          : failed
              .map((f) => {
                const name = oauthChannels.find((c) => c.id === f.id)?.name ?? `#${f.id}`;
                return `${name}：${f.err}`;
              })
              .join("\n"),
      );
      qc.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (e) => setMessage(errText(e)),
  });

  return (
    <div>
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="t-display">{t("订阅用量")}</h1>
          <p className="mt-1 text-sm text-muted">
            {t(
              "各 OAuth 渠道的套餐额度窗口与剩余量。数据由内核在刷新凭证时向上游采样，这里只是读回来 —— 点「刷新额度」会真的去问一次上游。API Key 渠道按量计费、没有套餐窗口，不在这一页。",
            )}
          </p>
        </div>
        <button
          onClick={() => refresh.mutate(oauthChannels.map((c) => c.id))}
          disabled={refresh.isPending || oauthChannels.length === 0}
          className="flex shrink-0 items-center gap-1 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
        >
          <RefreshCw className={cn("h-4 w-4", refresh.isPending && "animate-spin")} />
          {refresh.isPending ? t("刷新中…") : t("刷新额度")}
        </button>
        {/* 自报是另一条路：内核不管，客户端直接问上游。所以单独一个按钮，
            而且必须是点了才发 —— 自动探测会朝一堆第三方上游发它们根本不认识
            的 /usage 请求，是在给别人的服务器添噪声。 */}
        <button
          onClick={() => probe.mutate()}
          disabled={probe.isPending || otherChannels.length === 0}
          title={t("问非 OAuth 渠道的上游有没有自报用量的接口")}
          className="flex shrink-0 items-center gap-1 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
        >
          <Radar className={cn("h-4 w-4", probe.isPending && "animate-pulse")} />
          {probe.isPending ? t("探测中…") : t("探测自报用量")}
        </button>
      </div>

      {oauthChannels.length === 0 && (
        <p className="mt-6 text-sm text-muted">
          {channels.isLoading
            ? t("读取渠道中…")
            : t(
                "还没有 OAuth 订阅渠道。去「内核后台」用 Codex / Anthropic / Antigravity / xAI / Z.ai 登录后，这里才会有额度可看。",
              )}
        </p>
      )}

      <ul className="mt-6 space-y-3">
        {oauthChannels.map((c) => (
          <ChannelCard
            key={c.id}
            channel={c}
            busy={refresh.isPending}
            onRefresh={() => refresh.mutate([c.id])}
          />
        ))}
      </ul>

      {(probe.data?.found ?? []).map((u) => (
        <SelfReportedCard key={u.channel_id} usage={u} />
      ))}

      {message && <p className="mt-4 whitespace-pre-line text-sm text-accent">{message}</p>}
    </div>
  );
}

function ChannelCard({
  channel,
  busy,
  onRefresh,
}: {
  channel: UsageChannel;
  busy: boolean;
  onRefresh: () => void;
}) {
  const t = useT();
  const usage = channel.oauth_usage;
  const plan = [usage?.plan_type, usage?.subscription_tier].filter(Boolean).join(" · ");
  const windows = usage?.windows ?? [];

  // 时长标签重复的窗口才需要额度名来区分（Anthropic 的三个 7 天窗就是这样）。
  // 不重复时那一列纯属复述，显示出来只会让每行都长出一串上游内部标识。
  const dupLabels = useMemo(() => {
    const seen = new Map<string, number>();
    for (const w of windows) {
      const k = windowLabel(w, t);
      seen.set(k, (seen.get(k) ?? 0) + 1);
    }
    return new Set([...seen].filter(([, n]) => n > 1).map(([k]) => k));
  }, [windows, t]);

  return (
    <li className={cn("card p-4", channel.enabled === false && "opacity-60")}>
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{channel.name}</span>
            <span className="rounded bg-surface-2 px-1.5 py-0.5 text-[10px] text-muted">
              {AUTH_LABELS[channel.auth_type ?? ""] ?? channel.auth_type}
            </span>
            {plan && <span className="text-xs text-muted">{plan}</span>}
            {channel.enabled === false && (
              <span className="rounded bg-red-500/15 px-1.5 py-0.5 text-[10px] text-red-700">
                {t("已禁用")}
              </span>
            )}
          </div>
        </div>
        <button
          onClick={onRefresh}
          disabled={busy}
          className="shrink-0 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
        >
          {t("刷新")}
        </button>
      </div>

      {/* 没采到过额度和「额度是 0」完全是两回事，必须分开说：前者是「还没问过
          上游」，后者是「真的用完了」。混成一句话会让用户去查一个不存在的故障。 */}
      {windows.length === 0 ? (
        <p className="mt-3 text-xs text-muted">
          {t(
            "还没采到额度窗口。内核只在刷新凭证或你点「刷新」时才向上游采样；有的套餐本来就不提供额度端点。",
          )}
        </p>
      ) : (
        <ul className="mt-3 space-y-2.5">
          {windows.map((w) => (
            <WindowRow
              key={`${w.limit_name}|${w.kind}`}
              w={w}
              showExtra={dupLabels.has(windowLabel(w, t))}
            />
          ))}
        </ul>
      )}

      {usage?.warnings && usage.warnings.length > 0 && (
        <div className="mt-3 flex items-start gap-1.5 rounded-lg bg-amber-500/10 px-2.5 py-1.5 text-[11px] text-amber-800">
          <AlertTriangle className="mt-px h-3 w-3 shrink-0" />
          <div className="space-y-0.5">
            {usage.warnings.map((w) => (
              <div key={w}>{w}</div>
            ))}
          </div>
        </div>
      )}
    </li>
  );
}

function WindowRow({ w, showExtra }: { w: OAuthUsageWindow; showExtra: boolean }) {
  const t = useT();
  // 优先用上游给的 remaining_percent。它和 100 - used_percent 通常一致，但
  // 个别上游两个都给且并不互补（分别来自不同字段），此时信 remaining ——
  // 用户关心的就是这个数。
  const remaining = Number.isFinite(w.remaining_percent)
    ? w.remaining_percent
    : 100 - w.used_percent;
  const used = Math.min(100, Math.max(0, 100 - remaining));
  const tn = tone(remaining);
  const extra = showExtra ? limitExtra(w.limit_name, t) : "";
  // 内核按微美元存，这一格是「本窗口内累计的标准成本」——不乘渠道倍率，
  // 和总览的 effective_cost 不是一个口径，所以标注写清楚。
  const cost =
    w.standard_cost_microusd == null ? null : w.standard_cost_microusd / 1_000_000;

  return (
    <li>
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 text-xs">
        <span className="font-medium">{windowLabel(w, t)}</span>
        {extra && <span className="text-muted">{extra}</span>}
        <span className={cn("ml-auto tabular-nums", TEXT[tn])}>
          {t("剩余 {n}%", { n: remaining.toFixed(1) })}
        </span>
      </div>
      <div
        className="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-surface-2"
        role="meter"
        aria-valuenow={Math.round(used)}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label={`${windowLabel(w, t)}${extra ? ` ${extra}` : ""}`}
      >
        <div className={cn("h-full rounded-full", BAR[tn])} style={{ width: `${used}%` }} />
      </div>
      <div className="mt-0.5 flex flex-wrap gap-x-3 text-[11px] text-muted">
        <span>{t("已用 {n}%", { n: used.toFixed(1) })}</span>
        {w.reset_at > 0 && <span>{untilReset(w.reset_at, t)}</span>}
        {cost != null && (
          <span title={t("本窗口内累计的标准成本，未乘渠道倍率")}>
            {t("本窗口计费")} {fmtCost(cost)}
          </span>
        )}
      </div>
    </li>
  );
}


/// 上游自报的用量卡片。
///
/// 和上面那些 OAuth 卡片长一样，但数据来路完全不同 —— 那些是内核采样存下来的，
/// 这些是客户端刚刚直接问上游要的。所以标一句「上游自报」，别让人以为内核也
/// 在管它：内核对这类渠道的额度一无所知，刷新按钮对它们没用。
function SelfReportedCard({ usage }: { usage: SelfReportedUsage }) {
  const t = useT();
  const dupLabels = useMemo(() => {
    const seen = new Map<string, number>();
    for (const w of usage.windows) {
      const k = windowLabel(w, t);
      seen.set(k, (seen.get(k) ?? 0) + 1);
    }
    return new Set([...seen].filter(([, n]) => n > 1).map(([k]) => k));
  }, [usage.windows, t]);

  return (
    <li className="card mt-3 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">{usage.channel_name}</span>
        <span className="rounded bg-surface-2 px-1.5 py-0.5 text-[10px] text-muted">
          {usage.provider || t("上游自报")}
        </span>
        {usage.plan_type && <span className="text-xs text-muted">{usage.plan_type}</span>}
        <span
          className="rounded bg-sky-500/15 px-1.5 py-0.5 text-[10px] text-sky-700"
          title={t("这份数据是客户端直接问上游要的，内核并不知道它")}
        >
          {t("上游自报")}
        </span>
      </div>

      <ul className="mt-3 space-y-2.5">
        {usage.windows.map((w) => (
          <WindowRow
            key={`${w.limit_name}|${w.kind}`}
            w={w}
            showExtra={dupLabels.has(windowLabel(w, t))}
          />
        ))}
      </ul>

      {/* 上游的原话。我们算出来的百分比要和它对得上 —— 对不上就是解析错了，
          与其让用户自己发现，不如把原文摆在旁边。 */}
      {usage.display_message && (
        <p className="mt-2 text-[11px] text-muted">
          {t("上游原文")}：{usage.display_message}
        </p>
      )}
    </li>
  );
}
