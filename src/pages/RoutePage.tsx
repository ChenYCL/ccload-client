import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Plus, Search } from "lucide-react";
import { useT } from "../i18n";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { aliasKey, sameAlias } from "../lib/pins";
import { channelsOf, kernelAliases, type ChannelModels } from "../lib/modelOptions";
import { Panel } from "../components/StateBlock";
import { TextInput } from "../components/ui/Input";
import { AliasRoutes } from "../components/routes/AliasRoutes";
import { RouteEditor } from "../components/routes/RouteEditor";
import type { Page } from "../types";

/// 模型路由 —— 内核侧的那一层：**一个别名会落到哪个渠道的哪个上游模型**。
///
/// # 为什么把「模型链」和「强制路由」合成这一页
///
/// 那两页的输入形状（别名 → 有序的「渠道 + 上游模型」）、写入路径
/// （`channel_writer::patch_channel`）、最终写进内核的东西完全一样，差别只在
/// **应用时优先级怎么算**。分成两页的代价是：同一个别名的两套编排互相看不见，
/// 谁后应用谁赢，而界面上哪一页都不提另一页的存在。合成一页之后，模式成了一个
/// 开关，冲突也就没地方藏了（保存一种会删掉另一种）。
///
/// # 这一页和「CLI 接管」的分工
///
/// 两层，别混：
///   * CLI 接管 / 模型导入 → 写**本机 CLI 的配置文件**：连哪儿、默认发哪个别名、
///     `/model` 里能选到哪些。改的是「谁发什么名字」。
///   * 这一页 / 调度图 → 写**内核的渠道**：那个名字到了内核之后去谁家、变成什么
///     上游模型名。改的是「名字发出去之后的事」。
///
/// 三块内容按「意图 → 现状 → 谁在用」排：上面是你要它长什么样（编排，点应用才
/// 生效），中间是内核现在真的长什么样（可直接改），下面是哪些 CLI 正在发这个名字。

export function RoutePage({ onNavigate }: { onNavigate?: (page: Page) => void }) {
  const t = useT();
  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  const running = kernel.data?.state === "running";
  const settings = useQuery({ queryKey: ["app-settings"], queryFn: api.settingsGet });
  const channels = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.admin<ChannelModels[]>("GET", "channels"),
    enabled: running,
  });
  const chains = useQuery({ queryKey: ["fallback"], queryFn: api.fallbackList });
  const routes = useQuery({ queryKey: ["forced-routes"], queryFn: api.forcedRouteList });
  const pins = useQuery({ queryKey: ["pins"], queryFn: api.pinList });
  const preview = useQuery({ queryKey: ["cli-preview"], queryFn: api.cliPreviewAll });

  const channelList = useMemo(
    () => channelsOf(channels.data).filter((c) => c.id !== undefined),
    [channels.data],
  );
  // 远端模式下写的是别人机器上的渠道。空串 = 本机托管内核。
  const remoteKernel =
    settings.data?.kernel?.mode === "remote"
      ? (settings.data.kernel.remote_url ?? "").trim()
      : "";

  // 从「CLI 接管」点「改落点」进来时带的别名。读一次就清掉 —— 留着的话下次手动
  // 切到这一页会被它劫持，用户会以为自己点错了。
  const [focused] = useState<string | null>(() => {
    const v = sessionStorage.getItem("ccload:focus-alias");
    sessionStorage.removeItem("ccload:focus-alias");
    return v;
  });
  // 手工新建的别名。内核和本地编排里都还没有它，刷新一次查询也不会带回来，
  // 所以先记在这里，等它真的有了编排就会自然并进下面的并集。
  const [extra, setExtra] = useState<string[]>([]);
  const aliases = useMemo(() => {
    const seen = new Map<string, string>();
    const add = (name: string | undefined | null) => {
      const s = name?.trim();
      if (!s) return;
      const k = aliasKey(s);
      if (!seen.has(k)) seen.set(k, s);
    };
    kernelAliases(channelList).forEach(add);
    (chains.data ?? []).forEach((c) => add(c.alias));
    (routes.data ?? []).forEach((r) => add(r.from));
    extra.forEach(add);
    // 从 CLI 接管跳进来的那个名字也要在列表里。它可能内核里根本没有（正是
    // 「请求会 503」那种情况），左边不列出来的话选中项就没有对应行，看起来像
    // 点错了页。
    add(focused);
    return [...seen.values()].sort((a, b) => a.localeCompare(b));
  }, [channelList, chains.data, routes.data, extra, focused]);

  const [query, setQuery] = useState("");
  const [picked, setPicked] = useState<string | null>(focused);
  // 首屏选第一个；别名列表是异步来的，所以在它到位之后补选一次。
  useEffect(() => {
    if (aliases.length === 0) return;
    if (picked === null) {
      setPicked(aliases[0]);
      return;
    }
    if (aliases.includes(picked)) return;
    // 带窗口后缀的写法（`claude-opus-5[1M]`）和内核里的条目是同一个别名，而列表
    // 按 aliasKey 去重后只留了一种写法。从 CLI 接管跳进来时带的往往是**带后缀**
    // 的那个，不归一的话左边没有高亮行、右边却在显示内容，看着像选错了。
    const same = aliases.find((a) => sameAlias(a, picked));
    if (same) setPicked(same);
  }, [aliases, picked]);
  const alias = picked ?? "";

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    return q ? aliases.filter((a) => a.toLowerCase().includes(q)) : aliases;
  }, [aliases, query]);

  const chain = (chains.data ?? []).find((c) => sameAlias(c.alias, alias));
  const route = (routes.data ?? []).find((r) => sameAlias(r.from, alias));
  const hits = useQuery({
    queryKey: ["alias-routes", alias],
    queryFn: () => api.aliasRoutes(alias),
    enabled: running && alias.length > 0,
  });
  const consumers = (preview.data ?? []).filter(
    (p) => p.current_model && sameAlias(p.current_model, alias),
  );

  return (
    <div className="space-y-5">
      <header>
        <h1 className="t-display">{t("模型路由")}</h1>
        <p className="mt-1 max-w-3xl text-sm text-muted">
          {t("内核侧的那一层：一个别名到了内核之后去哪个渠道、变成哪个上游模型名。内核只按渠道优先级选路，没有 per-model 优先级 —— 所以「选了 A 却一直在跑 B」几乎总是某个高优先级渠道上的一条改写，而不是故障转移。CLI 发什么名字是另一层，在「CLI 接管」页。")}
        </p>
        {/* 改的是哪个内核，必须写在脸上：远端模式下这一页动的是**别人机器上**的
            渠道，而界面和本地模式一模一样。 */}
        {remoteKernel ? (
          <p className="mt-1.5 inline-flex flex-wrap items-center gap-x-1.5 rounded-lg border border-amber-500/40 bg-amber-500/10 px-2.5 py-1 text-xs text-amber-900">
            {t("改的是远端内核")}
            <span className="font-mono">{remoteKernel}</span>
            {t("上的渠道 —— 共用这台内核的人都会跟着变。")}
          </p>
        ) : (
          <p className="mt-1.5 text-xs text-muted">
            {t("改的是本机托管内核上的渠道。")}
          </p>
        )}
      </header>

      {!running ? (
        <div className="card bg-surface-raised px-4 py-8 text-center">
          <p className="text-sm text-muted">{t("内核未运行，读不到渠道，也没法改落点。")}</p>
          <p className="mt-1 text-xs text-muted/70">{t("从左下角「启动内核」开始。")}</p>
        </div>
      ) : (
        <div className="flex flex-col gap-5 lg:flex-row">
          <AliasList
            aliases={shown}
            total={aliases.length}
            picked={alias}
            query={query}
            onQuery={setQuery}
            onPick={setPicked}
            onCreate={(name) => {
              setExtra((cur) => (cur.includes(name) ? cur : [...cur, name]));
              setPicked(name);
              setQuery("");
            }}
            badgeOf={(a) => ({
              mode: (chains.data ?? []).some((c) => sameAlias(c.alias, a))
                ? "fallback"
                : (routes.data ?? []).some((r) => sameAlias(r.from, a))
                  ? "exclusive"
                  : null,
              pinned: (pins.data ?? []).some((p) => sameAlias(p.alias, a)),
            })}
          />

          <div className="min-w-0 flex-1 space-y-5">
            {alias === "" ? (
              <div className="card bg-surface-raised px-4 py-8 text-center text-sm text-muted">
                {t("内核里还没有任何别名。先去内核后台建一个渠道，或者在左边新建一个别名。")}
              </div>
            ) : (
              <>
                <Panel
                  title={t("① 编排 · 你要它落到哪")}
                  hint={t("本地记录，点「应用到内核」才写进渠道")}
                >
                  <RouteEditor
                    // 换别名要整个换掉编辑状态：草稿属于上一个别名，跟着走等于串台。
                    key={alias}
                    alias={alias}
                    channels={channelList}
                    chain={chain}
                    route={route}
                    hits={hits.data ?? []}
                  />
                </Panel>

                <Panel
                  title={t("② 内核落点 · 它现在落到哪")}
                  hint={t("GET /admin/channels · 改这里立刻生效，不经过编排")}
                >
                  <AliasRoutes
                    key={alias}
                    alias={alias}
                    proxyOn={settings.data?.route_cli_through_proxy ?? false}
                    channels={channelList}
                  />
                </Panel>

                <Panel title={t("③ 谁在发这个别名")} hint={t("各 CLI 配置里现在写着的默认模型")}>
                  {consumers.length === 0 ? (
                    <p className="text-xs text-muted">
                      {t("没有 CLI 的默认模型是这个名字。它可能只在 CLI 的 /model 菜单里被临时选中，或者压根没人用。")}{" "}
                      <button
                        onClick={() => onNavigate?.("cli")}
                        className="text-accent hover:underline"
                      >
                        {t("去 CLI 接管看看")}
                      </button>
                    </p>
                  ) : (
                    <ul className="flex flex-wrap gap-2">
                      {consumers.map((p) => (
                        <li
                          key={p.target}
                          className="flex items-center gap-1.5 rounded-lg border border-border bg-surface-2/60 px-2 py-1 text-xs"
                        >
                          <span className="font-medium">{p.label}</span>
                          <span className="font-mono text-[11px] text-muted">{p.current_model}</span>
                        </li>
                      ))}
                    </ul>
                  )}
                </Panel>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/// 左边的别名列表。列表本身就是这一页的导航 —— 一个别名一条，选中的那条右边
/// 展开全貌。徽标只标两件**会改变请求去向**的事：有没有编排、有没有被钉住。
function AliasList({
  aliases,
  total,
  picked,
  query,
  onQuery,
  onPick,
  onCreate,
  badgeOf,
}: {
  aliases: string[];
  total: number;
  picked: string;
  query: string;
  onQuery: (q: string) => void;
  onPick: (a: string) => void;
  onCreate: (a: string) => void;
  badgeOf: (a: string) => { mode: "fallback" | "exclusive" | null; pinned: boolean };
}) {
  const t = useT();
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState("");

  const create = () => {
    const name = draft.trim();
    if (!name) return;
    onCreate(name);
    setDraft("");
    setCreating(false);
  };

  return (
    <aside className="w-full shrink-0 lg:w-64">
      <div className="card bg-surface-raised p-3">
        <div className="flex items-center justify-between gap-2">
          <span className="t-title text-sm">{t("别名")}</span>
          <span className="text-[11px] text-muted">{total}</span>
        </div>
        {/* 这一列到底是什么，必须写在脸上：它和右边那些下拉里填的**不是同一种
            东西**。这里是别名（CLI 发出去的名字、内核拿它选渠道），右边填的是
            上游真实模型名（发给那家渠道时用的名字）。取反了不会当场报错，要等
            真正发请求那一刻才炸。 */}
        <p className="mt-0.5 text-[10px] leading-snug text-muted/70">
          {t("CLI 发出去的名字。右边配的是它落到某个渠道之后、发给上游的真实模型名。")}
        </p>

        <div className="relative mt-2">
          <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted" />
          <TextInput
            small
            aria-label={t("搜索别名")}
            value={query}
            onChange={(e) => onQuery(e.target.value)}
            placeholder={t("搜索")}
            className="w-full pl-7"
          />
        </div>

        <ul className="mt-2 max-h-[32rem] space-y-0.5 overflow-y-auto">
          {aliases.map((a) => {
            const b = badgeOf(a);
            return (
              <li key={a}>
                <button
                  onClick={() => onPick(a)}
                  aria-current={a === picked ? "true" : undefined}
                  className={cn(
                    "flex w-full items-center gap-1.5 rounded-lg px-2 py-1.5 text-left",
                    a === picked ? "bg-accent/12 text-accent" : "text-muted hover:bg-surface-2 hover:text-content",
                  )}
                >
                  <span className="min-w-0 flex-1 truncate font-mono text-xs">{a}</span>
                  {b.mode && (
                    <span
                      title={b.mode === "fallback" ? t("有退让编排") : t("有独占编排")}
                      className="shrink-0 rounded bg-surface-2 px-1 text-[10px] text-muted"
                    >
                      {b.mode === "fallback" ? t("退让") : t("独占")}
                    </span>
                  )}
                  {b.pinned && (
                    <span
                      title={t("钉了首选渠道")}
                      className="shrink-0 rounded bg-accent/15 px-1 text-[10px] text-accent"
                    >
                      {t("钉")}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
          {aliases.length === 0 && (
            <li className="px-2 py-3 text-center text-xs text-muted">{t("没有匹配的别名")}</li>
          )}
        </ul>

        <div className="mt-2 border-t border-border pt-2">
          {creating ? (
            <div className="flex items-center gap-1.5">
              <TextInput
                small
                mono
                autoFocus
                aria-label={t("新别名")}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") create();
                  if (e.key === "Escape") setCreating(false);
                }}
                placeholder={t("别名")}
                className="min-w-0 flex-1"
              />
              <button
                onClick={create}
                disabled={draft.trim().length === 0}
                className="shrink-0 rounded-lg bg-accent px-2 py-1 text-[11px] font-medium text-white disabled:opacity-40"
              >
                {t("加")}
              </button>
            </div>
          ) : (
            <button
              onClick={() => setCreating(true)}
              title={t("给一个内核里还没有的名字建编排 —— 应用之后内核才认得它")}
              className="flex w-full items-center justify-center gap-1 rounded-lg border border-border px-2 py-1.5 text-xs text-muted hover:bg-surface-2 hover:text-content"
            >
              <Plus className="h-3.5 w-3.5" /> {t("新建别名")}
            </button>
          )}
        </div>
      </div>
    </aside>
  );
}
