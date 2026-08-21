import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, LifeBuoy, Loader2, RefreshCw, Scissors, Sparkles } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import { useT, type Translate } from "../i18n";
import { ComboBox } from "../components/ui/ComboBox";
import { kernelAliases, type ChannelModels } from "../lib/modelOptions";
import type { CompactReport, SessionInfo, SlimReport } from "../types";

/// 会话救援。
///
/// # 这一页解决的问题
///
/// Claude Code 按**模型声明的窗口**决定何时自动压缩，而走 ccLoad 时真正拦你的
/// 是**中转那一家的上限**。两个数对不上时（典型：模型名挂了 `[1m]`，中转其实
/// 只给 500k），阈值就被算在一个不存在的分母上 —— 等它触发，已经越过真实天花
/// 板了。越过之后 `/compact` 自己也发不出去，因为它同样要把整段 transcript 发
/// 上去。会话就此卡死，只会一直报 400 too long。
///
/// # 为什么 token 数不是估的
///
/// 每条 assistant 记录里有上游回报的 usage。真实上下文 =
/// `input_tokens + cache_read + cache_creation`，这是唯一和 400 报错里那个数字
/// 对得上的口径。**只看 `input_tokens` 会小一个数量级** —— 长会话里它常年是个
/// 位数，因为绝大部分都命中了缓存。

/// 瘦身的目标上下文。给天花板留出压缩本身要用的余量 —— 压缩请求要把整段
/// transcript 再发一遍，顶着上限做不成任何事。
const DEFAULT_TARGET = 300_000;
/// 单条文本超过多少字符就截。
const DEFAULT_TEXT_LIMIT = 4_000;
/// 分块总结时每块多大。远低于任何中转的上限，这样切块本身不会再超限。
const CHUNK_TOKENS = 120_000;
/// 尾巴留几轮原文。最近几轮是用户正在做的事，摘要替代不了。
const KEEP_TAIL = 12;
/// 超过这个数就标红。多数第三方中转卡在 200k–500k 之间，取个中间值提醒。
const DANGER_CONTEXT = 400_000;

function fmtTokens(n: number): string {
  if (n <= 0) return "—";
  if (n < 1000) return String(n);
  return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function fmtAgo(unixSec: number, t: Translate): string {
  if (!unixSec) return "";
  const sec = Math.floor(Date.now() / 1000) - unixSec;
  if (sec < 3600) return t("{n} 分钟前", { n: Math.max(1, Math.floor(sec / 60)) });
  if (sec < 86_400) return t("{n} 小时前", { n: Math.floor(sec / 3600) });
  return t("{n} 天前", { n: Math.floor(sec / 86_400) });
}

export function SessionsPage() {
  const t = useT();
  const qc = useQueryClient();
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [target, setTarget] = useState(DEFAULT_TARGET);
  const [model, setModel] = useState("");

  const sessions = useQuery({
    queryKey: ["sessions"],
    queryFn: api.sessionList,
    // 扫描要读几十 MB，不自动轮询 —— 这一页是出事了才来的，不是盯着看的。
    staleTime: Infinity,
  });

  // 分块总结要指定模型。用内核里真实存在的别名，别让用户手打一个不存在的名字。
  const channels = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.admin<ChannelModels[]>("GET", "channels"),
  });
  const models = useMemo(() => kernelAliases(channels.data?.data), [channels.data]);

  const done = (label: string, backup: string, detail: string) => {
    setMessage(`${label}：${detail}\n${t("备份")}：${backup}`);
    qc.invalidateQueries({ queryKey: ["sessions"] });
  };

  const slim = useMutation({
    mutationFn: (s: SessionInfo) => api.sessionSlim(s.path, target, DEFAULT_TEXT_LIMIT),
    onSuccess: (r: SlimReport) =>
      done(
        t("瘦身完成"),
        r.backup,
        t("砍掉 {img} 张图、截短 {txt} 处文本，上下文 {before} → {after}，文件 {b1} → {b2}", {
          img: r.images_stripped,
          txt: r.texts_truncated,
          before: fmtTokens(r.context_before),
          after: fmtTokens(r.context_after),
          b1: fmtBytes(r.bytes_before),
          b2: fmtBytes(r.bytes_after),
        }),
      ),
    onError: (e) => setMessage(errText(e)),
    onSettled: () => setBusyId(null),
  });

  const compact = useMutation({
    mutationFn: (s: SessionInfo) =>
      api.sessionCompact(s.path, model, KEEP_TAIL, CHUNK_TOKENS),
    onSuccess: (r: CompactReport) =>
      done(
        t("分块总结完成"),
        r.backup,
        t("切成 {n} 块分别总结，保留最近 {k} 轮原文，摘要约 {s}（原 {before}）", {
          n: r.chunks,
          k: r.kept_tail,
          s: fmtTokens(r.summary_tokens),
          before: fmtTokens(r.context_before),
        }),
      ),
    onError: (e) => setMessage(errText(e)),
    onSettled: () => setBusyId(null),
  });

  const busy = slim.isPending || compact.isPending;
  const rows = sessions.data ?? [];

  return (
    <div>
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="t-display">{t("会话救援")}</h1>
          <p className="mt-1 max-w-3xl text-sm text-muted">
            {t(
              "会话撑过中转的上限之后会卡死：/compact 自己也要把整段对话发上去，所以它同样超限，从此只会报 400 too long。这里能把它弄回来。表格里的 token 数来自上游回报的用量，不是估算。",
            )}
          </p>
        </div>
        <button
          onClick={() => sessions.refetch()}
          disabled={sessions.isFetching}
          className="flex shrink-0 items-center gap-1 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
        >
          <RefreshCw className={cn("h-4 w-4", sessions.isFetching && "animate-spin")} />
          {t("重新扫描")}
        </button>
      </div>

      <div className="mt-5 card flex flex-wrap items-end gap-4 p-4">
        <label className="text-xs">
          <span className="mb-1 block text-muted">{t("瘦身目标上下文")}</span>
          <input
            type="number"
            step={10_000}
            min={20_000}
            value={target}
            onChange={(e) => setTarget(Number(e.target.value) || DEFAULT_TARGET)}
            className="w-32 rounded-lg border border-border bg-surface-raised px-2 py-1 font-mono"
          />
        </label>
        <label className="min-w-56 flex-1 text-xs">
          <span className="mb-1 block text-muted">{t("总结用的模型")}</span>
          <ComboBox
            value={model}
            onChange={setModel}
            options={models}
            placeholder={t("选一个内核里有的模型")}
            emptyHint={t("内核里没有可用模型")}
          />
        </label>
        <p className="max-w-md text-[11px] leading-relaxed text-muted">
          {t(
            "目标要给天花板留余量 —— 压缩请求本身也要把整段对话发一遍，顶着上限做不成任何事。",
          )}
        </p>
      </div>

      {rows.length === 0 && !sessions.isPending && (
        <p className="mt-6 text-sm text-muted">{t("没有找到任何会话。")}</p>
      )}

      <ul className="mt-4 divide-y divide-border/60 rounded-xl border border-border">
        {rows.map((s) => {
          const danger = s.peak_context >= DANGER_CONTEXT;
          const working = busyId === s.id && busy;
          return (
            <li key={s.id} className="flex flex-wrap items-center gap-3 px-3 py-2.5">
              <span
                className={cn(
                  "h-1.5 w-1.5 shrink-0 rounded-full",
                  s.live ? "bg-emerald-500" : "bg-border",
                )}
                title={s.live ? t("正在运行") : t("已停止")}
              />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-sm" title={s.cwd}>
                  {s.slug || s.id.slice(0, 8)}
                  <span className="ml-2 text-xs text-muted">
                    {s.cwd.split("/").pop() || s.cwd}
                  </span>
                </span>
                <span className="mt-0.5 block truncate font-mono text-[10px] text-muted/80">
                  {s.id}
                </span>
              </span>

              <span className="shrink-0 text-right text-xs">
                <span className="block text-muted">{t("当前")}</span>
                <span className="font-mono">{fmtTokens(s.last_context)}</span>
              </span>
              <span className="shrink-0 text-right text-xs">
                <span className="block text-muted">{t("峰值")}</span>
                <span className={cn("font-mono", danger && "font-semibold text-red-600")}>
                  {fmtTokens(s.peak_context)}
                </span>
              </span>
              <span className="hidden shrink-0 text-right text-xs text-muted sm:block">
                <span className="block">{fmtBytes(s.bytes)}</span>
                <span>{fmtAgo(s.modified_at, t)}</span>
              </span>

              {s.compactions > 0 && (
                <span
                  className="shrink-0 rounded bg-surface-2 px-1.5 py-0.5 text-[10px] text-muted"
                  title={t("最后一次压缩之前的内容本来就不进上下文，救援只处理它之后的部分")}
                >
                  {t("压缩过 {n} 次", { n: s.compactions })}
                </span>
              )}

              {/* 活着的会话必须挡住：进程里有内存态，改完它一落盘就盖回去，
                  用户会以为工具没生效。 */}
              {s.live ? (
                <span
                  className="flex shrink-0 items-center gap-1 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-700"
                  title={t("先退出那个 Claude Code 窗口 —— 进程里有内存态，现在改会被它盖回去")}
                >
                  <AlertTriangle className="h-3 w-3" />
                  {t("运行中")}
                </span>
              ) : (
                <span className="flex shrink-0 items-center gap-1.5">
                  <button
                    onClick={() => {
                      setBusyId(s.id);
                      slim.mutate(s);
                    }}
                    disabled={busy || s.last_context === 0}
                    title={
                      s.last_context === 0
                        ? t("这份记录里没有用量数据，拿不到真实上下文 —— 不敢下手")
                        : t("砍图 + 截长工具结果。本地完成，不花 token，但信息真的丢了")
                    }
                    className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
                  >
                    {working && slim.isPending ? (
                      <Loader2 className="h-3 w-3 animate-spin" />
                    ) : (
                      <Scissors className="h-3 w-3" />
                    )}
                    {t("瘦身")}
                  </button>
                  <button
                    onClick={() => {
                      setBusyId(s.id);
                      compact.mutate(s);
                    }}
                    disabled={busy || !model || s.last_context === 0}
                    title={
                      !model
                        ? t("先在上面选一个模型")
                        : t("分块总结后追加一个原生压缩边界。旧内容一个字节不动，花 token 但保信息")
                    }
                    className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
                  >
                    {working && compact.isPending ? (
                      <Loader2 className="h-3 w-3 animate-spin" />
                    ) : (
                      <Sparkles className="h-3 w-3" />
                    )}
                    {t("分块总结")}
                  </button>
                </span>
              )}
            </li>
          );
        })}
      </ul>

      <div className="mt-4 card p-4">
        <div className="flex items-center gap-1.5 text-sm font-medium">
          <LifeBuoy className="h-4 w-4 text-accent" />
          {t("两种救法的区别")}
        </div>
        <ul className="mt-2 space-y-1.5 text-xs leading-relaxed text-muted">
          <li>
            {t(
              "瘦身 —— 把图片换成占位符、超长工具结果留首尾。纯本地、秒级、不花 token，但被砍掉的内容是真的没了。急着把会话弄活用它。",
            )}
          </li>
          <li>
            {t(
              "分块总结 —— 把对话切成小段分别总结再合并，然后追加一个和 Claude Code 自己压缩时一模一样的边界。任何一次请求都远低于天花板，所以不会像 /compact 那样自己也超限。花 token，但信息以摘要形式留下来了。",
            )}
          </li>
          <li>
            {t(
              "两种都会先把原文件另存一份 .bak，而且都不删记录 —— transcript 是靠 uuid 串起来的链表，删行会让恢复出来的会话缺胳膊少腿。",
            )}
          </li>
        </ul>
      </div>

      {message && <p className="mt-4 whitespace-pre-line text-sm text-accent">{message}</p>}
    </div>
  );
}
