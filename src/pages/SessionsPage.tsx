import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, History, LifeBuoy, Loader2, RefreshCw, Scissors, Sparkles, Stethoscope } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import { useT } from "../i18n";
import { ComboBox } from "../components/ui/ComboBox";
import { Overlay } from "../components/Modal";
import { Select, TextInput } from "../components/ui/Input";
import { kernelAliases, type ChannelModels } from "../lib/modelOptions";
import {
  cliLabel,
  filterSessions,
  fmtAgo,
  fmtBytes,
  fmtTokens,
  projectName,
  uniqueClis,
  uniqueProjects,
  type SessionSort,
} from "../lib/sessionList";
import type { CleanReport, CompactReport, PollutionReport, SessionInfo, SlimReport } from "../types";

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

/// 瘦身目标和总结模型记在 localStorage：这是「这台机器的中转天花板是多少、用哪个
/// 模型压」的偏好，换次会话不该重填。和侧栏收起、日志实时开关同一个模式。
const TARGET_KEY = "ccload.sessions.target";
const MODEL_KEY = "ccload.sessions.model";

type BatchItem = { id: string; slug: string; ok: boolean; detail: string };

/// 一条救援记录。写进 localStorage 而不是内存：救援刚点完用户常常切去 CLI
/// 验证效果，「完成」的凭据必须还在；而前端没有别的持久层。
type RescueLogEntry = {
  time: number;
  kind: "slim" | "compact" | "clean";
  cli: string;
  slug: string;
  backup: string;
  detail: string;
};

const HISTORY_KEY = "ccload.sessions.rescue-history";
const HISTORY_MAX = 30;

function loadHistory(): RescueLogEntry[] {
  try {
    const v = JSON.parse(localStorage.getItem(HISTORY_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => x && x.time) : [];
  } catch {
    return [];
  }
}

function pushHistory(e: RescueLogEntry) {
  const list = [e, ...loadHistory()].slice(0, HISTORY_MAX);
  localStorage.setItem(HISTORY_KEY, JSON.stringify(list));
  // 同页的其它组件（历史面板）靠它感知，不用把 setState 提到父层。
  window.dispatchEvent(new CustomEvent("ccload:rescue-history"));
}

export function SessionsPage() {
  const t = useT();
  const qc = useQueryClient();
  const [message, setMessage] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  // 瘦身目标存字符串而不是数字。数字态存不了「用户正在改」的中间态：全选后键入
  // 第一个字符前输入框是空串，`Number("") || DEFAULT` 会立刻把值弹回默认 —— 用户
  // 看到的就是「打字没反应」。字符串允许空着，用到的时候（点瘦身/失焦校验）才解析。
  const [targetText, setTargetText] = useState(
    () => localStorage.getItem(TARGET_KEY) || String(DEFAULT_TARGET),
  );
  const target = Number(targetText) || 0;
  // 空串是合法值（= 按模型链自动挑），getItem 拿不到才回落空。
  const [model, setModel] = useState(() => localStorage.getItem(MODEL_KEY) ?? "");
  useEffect(() => {
    localStorage.setItem(TARGET_KEY, targetText);
    localStorage.setItem(MODEL_KEY, model);
  }, [targetText, model]);

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

  const [history, setHistory] = useState<RescueLogEntry[]>(loadHistory);
  useEffect(() => {
    const on = () => setHistory(loadHistory());
    window.addEventListener("ccload:rescue-history", on);
    return () => window.removeEventListener("ccload:rescue-history", on);
  }, []);

  const done = (
    label: string,
    backup: string,
    detail: string,
    kind: RescueLogEntry["kind"],
    s: SessionInfo,
  ) => {
    setMessage(`${label}：${detail}\n${t("备份")}：${backup}`);
    pushHistory({
      time: Math.floor(Date.now() / 1000),
      kind,
      cli: s.cli,
      slug: s.slug || s.id.slice(0, 8),
      backup,
      detail,
    });
    qc.invalidateQueries({ queryKey: ["sessions"] });
  };

  const slim = useMutation({
    mutationFn: (s: SessionInfo) => api.sessionSlim(s.path, target, DEFAULT_TEXT_LIMIT),
    onSuccess: (r: SlimReport, s) =>
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
        "slim",
        s,
      ),
    onError: (e) => setMessage(errText(e)),
    onSettled: () => setBusyId(null),
  });

  const compact = useMutation({
    mutationFn: (s: SessionInfo) =>
      api.sessionCompact(s.path, model, KEEP_TAIL, CHUNK_TOKENS),
    onSuccess: (r: CompactReport, s) =>
      done(
        t("分块总结完成"),
        r.backup,
        t("用 {model} 切成 {n} 块分别总结，保留最近 {k} 轮原文，摘要约 {s}（原 {before}）", {
          model: r.model || model || t("自动"),
          n: r.chunks,
          k: r.kept_tail,
          s: fmtTokens(r.summary_tokens),
          before: fmtTokens(r.context_before),
        }),
        "compact",
        s,
      ),
    onError: (e) => setMessage(errText(e)),
    onSettled: () => setBusyId(null),
  });

  /// 一键救援 = 对勾上的会话逐条分块总结。串行：这些请求打同一个渠道，并发
  /// 只会把它顶到限流，而救援本来就不赶那几秒。
  const batch = useMutation({
    mutationFn: async (items: SessionInfo[]) => {
      const out: BatchItem[] = [];
      for (const s of items) {
        setBusyId(s.id);
        setMessage(
          t("救援中 {i}/{n}：{name}", {
            i: out.length + 1,
            n: items.length,
            name: s.slug || s.id.slice(0, 8),
          }),
        );
        try {
          const r = await api.sessionCompact(s.path, model, KEEP_TAIL, CHUNK_TOKENS);
          out.push({
            id: s.id,
            slug: s.slug || s.id.slice(0, 8),
            ok: true,
            detail: t("用 {model} 切成 {n} 块分别总结，保留最近 {k} 轮原文，摘要约 {s}（原 {before}）", {
              model: r.model || model || t("自动"),
              n: r.chunks,
              k: r.kept_tail,
              s: fmtTokens(r.summary_tokens),
              before: fmtTokens(r.context_before),
            }),
          });
        } catch (e) {
          out.push({
            id: s.id,
            slug: s.slug || s.id.slice(0, 8),
            ok: false,
            detail: errText(e),
          });
        }
      }
      return out;
    },
    onSuccess: (items) => {
      const ok = items.filter((x) => x.ok).length;
      const fail = items.length - ok;
      const lines = items.map((x) => `${x.ok ? "✓" : "✗"} ${x.slug}：${x.detail}`);
      setMessage(
        `${t("一键救援完成：成功 {ok}，失败 {fail}", { ok, fail })}\n${lines.join("\n")}`,
      );
      setSelected(new Set());
      qc.invalidateQueries({ queryKey: ["sessions"] });
    },
    onError: (e) => setMessage(errText(e)),
    onSettled: () => setBusyId(null),
  });

  // 污染体检：按需算（要读整份正文），结果留在弹窗里，清洗完就地刷新。
  const [checking, setChecking] = useState<SessionInfo | null>(null);
  const [report, setReport] = useState<PollutionReport | null>(null);
  const checkup = useMutation({
    mutationFn: (s: SessionInfo) => api.sessionPollution(s.path),
    onSuccess: (r) => setReport(r),
    onError: (e) => {
      setChecking(null);
      setMessage(errText(e));
    },
  });
  const clean = useMutation({
    mutationFn: (s: SessionInfo) => api.sessionClean(s.path),
    onSuccess: (r: CleanReport, s) => {
      setChecking(null);
      setReport(null);
      done(
        t("清洗完成"),
        r.backup,
        t("删掉 {o} 条孤儿工具结果、折叠 {d} 处重复、清理 {c} 条跨模型思维链", {
          o: r.orphans_removed,
          d: r.duplicates_collapsed,
          c: r.reasoning_stripped,
        }),
        "clean",
        s,
      );
    },
    onError: (e) => setMessage(errText(e)),
  });

  const openCheckup = (s: SessionInfo) => {
    setChecking(s);
    setReport(null);
    checkup.mutate(s);
  };

  const busy = slim.isPending || compact.isPending || batch.isPending;

  /// 列表的筛选 / 搜索 / 排序。
  ///
  /// 三家 CLI 的会话混在一张表里（Claude Code / Grok Build / Codex），所以既要能按
  /// **CLI** 缩范围，也要能按**项目**（cwd 的最后一段）缩。Gemini CLI 和 OpenCode
  /// 不在列表里 —— 它们磁盘上没有对话正文，没有可救的东西。
  const [query, setQuery] = useState("");
  const [project, setProject] = useState("");
  const [cli, setCli] = useState("");
  const [sort, setSort] = useState<SessionSort>("recent");

  const all = sessions.data ?? [];
  const projects = useMemo(() => uniqueProjects(all), [all]);
  const clis = useMemo(() => uniqueClis(all), [all]);
  const rows = useMemo(
    () => filterSessions(all, { query, project, cli, sort }),
    [all, query, project, cli, sort],
  );

  /// 能勾的：没在跑、有真实上下文。活着的改了会被进程盖回去；没用量的不敢压。
  const selectable = rows.filter((s) => !s.live && s.last_context > 0);
  const selectedRows = selectable.filter((s) => selected.has(s.id));
  const allChecked = selectable.length > 0 && selectedRows.length === selectable.length;

  const toggle = (id: string, on: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  };
  const toggleAll = (on: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      for (const s of selectable) {
        if (on) next.add(s.id);
        else next.delete(s.id);
      }
      return next;
    });
  };

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
            value={targetText}
            onChange={(e) => setTargetText(e.target.value)}
            // 失焦时才收口：空/非法值回落默认。打字过程中不碰它。
            onBlur={() =>
              setTargetText(String(Number(targetText) || DEFAULT_TARGET))
            }
            className="w-32 rounded-lg border border-border bg-surface-raised px-2 py-1 font-mono"
          />
        </label>
        <label className="min-w-56 flex-1 text-xs">
          <span className="mb-1 block text-muted">{t("总结用的模型")}</span>
          <ComboBox
            value={model}
            onChange={setModel}
            options={models}
            placeholder={t("空着 = 按模型链自动挑窗口够的")}
            emptyHint={t("内核里没有可用模型")}
          />
        </label>
        <p className="max-w-md text-[11px] leading-relaxed text-muted">
          {t(
            "目标要给天花板留余量 —— 压缩请求本身也要把整段对话发一遍，顶着上限做不成任何事。",
          )}
        </p>
      </div>

      {/* 筛选 / 搜索 / 排序。几十个会话时不给这三样，找一个就只能靠滚。 */}
      <div className="mt-4 flex flex-wrap items-center gap-2">
        <TextInput
          small
          className="min-w-56 flex-1"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("搜名字、uuid 或路径")}
          aria-label={t("搜索会话")}
        />
        {/* CLI 筛选。只列扫到的那几家，没装的不占位置。 */}
        <Select
          small
          className="w-40 shrink-0"
          value={cli}
          onChange={(e) => setCli(e.target.value)}
          aria-label={t("按 CLI 筛选")}
        >
          <option value="">{t("全部 CLI（{n}）", { n: clis.length })}</option>
          {clis.map(([c, n]) => (
            <option key={c} value={c}>
              {cliLabel(c)}（{n}）
            </option>
          ))}
        </Select>
        <Select
          small
          className="w-52 shrink-0"
          value={project}
          onChange={(e) => setProject(e.target.value)}
          aria-label={t("按项目筛选")}
        >
          <option value="">{t("全部项目（{n}）", { n: projects.length })}</option>
          {projects.map(([p, n]) => (
            <option key={p} value={p}>
              {p}（{n}）
            </option>
          ))}
        </Select>
        <Select
          small
          className="w-40 shrink-0"
          value={sort}
          onChange={(e) => setSort(e.target.value as SessionSort)}
          aria-label={t("排序")}
        >
          <option value="recent">{t("最近改动")}</option>
          <option value="oldest">{t("最早改动")}</option>
          <option value="peak">{t("峰值最大")}</option>
          <option value="current">{t("当前最大")}</option>
          <option value="size">{t("文件最大")}</option>
        </Select>
        {(query || project || cli) && (
          <button
            onClick={() => {
              setQuery("");
              setProject("");
              setCli("");
            }}
            className="shrink-0 whitespace-nowrap rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs text-muted hover:bg-surface-2"
          >
            {t("清除筛选")}
          </button>
        )}
        <span className="shrink-0 text-xs text-muted">
          {rows.length === all.length
            ? t("共 {n} 个会话", { n: all.length })
            : t("{shown} / {total}", { shown: rows.length, total: all.length })}
        </span>
      </div>

      {/* 批量操作条。没勾任何一条时按钮也在，免得「勾了才出现」让人找不到入口。 */}
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <label className="flex items-center gap-1.5 text-xs text-muted">
          <input
            type="checkbox"
            checked={allChecked}
            disabled={selectable.length === 0 || busy}
            onChange={(e) => toggleAll(e.target.checked)}
          />
          {t("全选当前列表")}
        </label>
        <button
          onClick={() => batch.mutate(selectedRows)}
          disabled={busy || selectedRows.length === 0}
          title={
            selectedRows.length === 0
              ? t("先勾要救的会话")
              : t("对勾上的会话逐条分块总结。空着模型就按模型链挑窗口够的那一跳")
          }
          className="flex items-center gap-1 rounded-lg bg-accent px-2.5 py-1 text-xs font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
        >
          {batch.isPending ? (
            <Loader2 className="h-3 w-3 animate-spin" />
          ) : (
            <Sparkles className="h-3 w-3" />
          )}
          {selectedRows.length > 0
            ? t("一键救援（{n}）", { n: selectedRows.length })
            : t("一键救援")}
        </button>
        <span className="text-[11px] text-muted">
          {t("一键救援 = 对勾上的会话逐条分块总结，活着的和没有用量的会跳过。")}
        </span>
      </div>

      {rows.length === 0 && !sessions.isPending && (
        <p className="mt-6 text-sm text-muted">
          {all.length === 0 ? t("没有找到任何会话。") : t("没有匹配的会话。换个关键词或清除筛选。")}
        </p>
      )}

      <ul className="mt-4 divide-y divide-border/60 rounded-xl border border-border">
        {rows.map((s) => {
          const danger = s.peak_context >= DANGER_CONTEXT;
          const working = busyId === s.id && busy;
          const canSelect = !s.live && s.last_context > 0;
          return (
            <li key={s.id} className="flex flex-wrap items-center gap-3 px-3 py-2.5">
              <input
                type="checkbox"
                checked={selected.has(s.id)}
                disabled={!canSelect || busy}
                onChange={(e) => toggle(s.id, e.target.checked)}
                aria-label={t("选中 {name}", { name: s.slug || s.id.slice(0, 8) })}
                className="shrink-0"
              />
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
                  <span className="ml-2 rounded bg-surface-2 px-1 py-px text-[10px] text-muted">
                    {cliLabel(s.cli)}
                  </span>
                  <span className="ml-1.5 text-xs text-muted">{projectName(s)}</span>
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
                  title={t("先退出那个 CLI 窗口 —— 进程里有内存态，现在改会被它盖回去")}
                >
                  <AlertTriangle className="h-3 w-3" />
                  {t("运行中")}
                </span>
              ) : (
                <span className="flex shrink-0 items-center gap-1.5">
                  <button
                    onClick={() => openCheckup(s)}
                    disabled={busy}
                    title={t("查这条会话有没有孤儿工具结果、重试循环、跨模型思维链")}
                    className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
                  >
                    <Stethoscope className="h-3 w-3" />
                    {t("体检")}
                  </button>
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
                    disabled={busy || s.last_context === 0}
                    title={t("分块总结后追加一个原生压缩边界。空着模型就按模型链挑窗口够的那一跳")}
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

      {checking && (
        <Overlay onClose={() => !clean.isPending && setChecking(null)}>
          <div className="material-modal animate-materialize w-[32rem] max-w-full rounded-xl border border-border p-4">
            <h2 className="t-title">{t("污染体检")}</h2>
            <p className="mt-0.5 truncate text-xs text-muted" title={checking.cwd}>
              {checking.slug || checking.id.slice(0, 8)} · {cliLabel(checking.cli)}
            </p>

            {checkup.isPending && (
              <p className="mt-4 flex items-center gap-2 text-sm text-muted">
                <Loader2 className="h-4 w-4 animate-spin" />
                {t("正在读整份正文…")}
              </p>
            )}

            {report && (
              <div className="mt-4 space-y-2 text-sm">
                <Finding
                  n={report.orphan_tool_results}
                  label={t("孤儿工具结果")}
                  hint={t("配不上任何调用 —— CLI 复原对话时会拿它没辙。清洗直接删。")}
                />
                <Finding
                  n={report.redundant_tool_results}
                  label={t("重复的重试循环")}
                  hint={t(
                    "同一次调用的同样输出重复出现，是卡在重试里的痕迹。清洗只折叠内容、保留条目 —— 删了会让对应的调用落空。",
                  )}
                />
                <Finding
                  n={report.cross_model_reasoning}
                  label={t("跨模型思维链")}
                  hint={t(
                    "密文由另一家上游签发，换家之后解不开，正是「conversation history is incompatible」的病灶。清洗把非当前那家换成占位。",
                  )}
                />
                {report.reasoning_issuers.length > 0 && (
                  <p className="text-xs text-muted">
                    {t("签发方")}：
                    {report.reasoning_issuers.map(([k, n]) => `${k}×${n}`).join("、")}
                  </p>
                )}
                <p className="text-xs text-muted">
                  {t("共扫描 {n} 行正文", { n: report.entries })}
                </p>
              </div>
            )}

            <div className="mt-5 flex items-center justify-end gap-2">
              <button
                onClick={() => setChecking(null)}
                disabled={clean.isPending}
                className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
              >
                {t("关闭")}
              </button>
              {report && (
                <button
                  onClick={() => clean.mutate(checking)}
                  disabled={
                    clean.isPending ||
                    checking.live ||
                    report.orphan_tool_results +
                      report.redundant_tool_results +
                      report.cross_model_reasoning ===
                      0
                  }
                  title={
                    checking.live
                      ? t("先退出那个 CLI 窗口 —— 进程里有内存态，现在改会被它盖回去")
                      : t("写前会先备份整份原文")
                  }
                  className="flex items-center gap-1 rounded-lg bg-accent px-3 py-1.5 text-sm font-medium text-white hover:bg-accent/90 disabled:opacity-40"
                >
                  {clean.isPending && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
                  {t("清洗")}
                </button>
              )}
            </div>
          </div>
        </Overlay>
      )}

      {message && <p className="mt-4 whitespace-pre-line text-sm text-accent">{message}</p>}

      {/* 救援历史。救援点完用户常切去 CLI 验证，回来时那条「已完成」早没了 ——
          记录是唯一还在场的凭据，顺带把备份路径留在手边（那是唯一的后悔药）。 */}
      {history.length > 0 && (
        <details className="mt-4 rounded-xl border border-border">
          <summary className="flex cursor-pointer items-center gap-1.5 px-3 py-2 text-sm text-muted">
            <History className="h-3.5 w-3.5" />
            {t("救援记录（{n}）", { n: history.length })}
          </summary>
          <ul className="divide-y divide-border/60 border-t border-border">
            {history.map((h) => (
              <li key={h.time} className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 px-3 py-2 text-xs">
                <span className="tabular-nums text-muted">{new Date(h.time * 1000).toLocaleTimeString()}</span>
                <span className="rounded bg-surface-2 px-1.5 py-px text-[10px] text-muted">{cliLabel(h.cli as SessionInfo["cli"])}</span>
                <span className={cn("rounded px-1.5 py-px text-[10px]", h.kind === "compact" ? "bg-accent/15 text-accent" : "bg-surface-2 text-muted")}>
                  {h.kind === "compact" ? t("分块总结") : h.kind === "clean" ? t("清洗") : t("瘦身")}
                </span>
                <span className="font-medium">{h.slug}</span>
                <span className="min-w-0 flex-1 truncate text-muted" title={`${h.detail}
${h.backup}`}>
                  {h.detail}
                </span>
                <span className="truncate font-mono text-[10px] text-muted/70" title={h.backup}>
                  {h.backup.split("/").pop()}
                </span>
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

/// 体检里的一条发现。0 是好消息，所以零和非零要一眼分得开。
function Finding({ n, label, hint }: { n: number; label: string; hint: string }) {
  return (
    <div className="rounded-lg border border-border px-3 py-2">
      <div className="flex items-baseline gap-2">
        <span
          className={cn(
            "rounded px-1.5 py-px text-xs font-medium tabular-nums",
            n > 0 ? "bg-amber-500/15 text-amber-700" : "bg-emerald-500/12 text-emerald-700",
          )}
        >
          {n}
        </span>
        <span className="text-sm">{label}</span>
      </div>
      <p className="mt-0.5 text-[11px] leading-relaxed text-muted">{hint}</p>
    </div>
  );
}
