import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Download, Plus, Save, Trash2, Wand2, X } from "lucide-react";
import { useT } from "../../i18n";
import { api } from "../../lib/api";
import { cn } from "../../lib/cn";
import { errText } from "../../lib/err";
import { lookupMeta } from "../../lib/modelCatalog";
import { formatWindow, tierKey } from "../../lib/modelMeta";
import { TARGET_LABELS } from "../../lib/targets";
import { ComboBox } from "../ui/ComboBox";
import { TextInput } from "../ui/Input";
import type { BridgeEntry, ClaudeSuffix, CliTarget } from "../../types";

/// 按窗口给别名补后缀，和后端 `with_window_suffix` 同一套规则。
function withWindowSuffix(alias: string, window: number, kind: ClaudeSuffix): string {
  const raw = alias.trim();
  const stripped = raw.replace(/\[[0-9.]+[kKmM]?\]\s*$/, "").trimEnd();
  const base = stripped || raw;
  if (kind === "off" || window <= 0 || !base) return raw;
  const mega = kind === "1m" ? "m" : "M";
  if (window % 1_000_000 === 0) return `${base}[${window / 1_000_000}${mega}]`;
  if (window % 1_000 === 0) return `${base}[${window / 1_000}k]`;
  return `${base}[${window}]`;
}

/// 出口别名表 —— 这一页真正的内容。
///
/// # 三个名字，别搞混
///
/// ```text
///   CLI 里选的        代理转发时换成      内核拿它选渠道
///   出口别名 alias ──►  落点 target  ──►  渠道的 models[].model
/// ```
///
/// `alias` 是**我们**说了算的名字：写进各 CLI 的目录 / 槽位，用户在 `/model` 里
/// 看到的就是它。`target` 是内核渠道上真实存在的别名。两者相同（默认）时不产生
/// 改写，也就不依赖本地代理；改了名就必须走代理，后端会拦（见 `bridge::validate`）。
///
/// # 为什么勾选是「按 CLI」的
///
/// Claude Code 没有模型目录，是 6 个具名槽位加一份 modelPicker 列表；OpenCode 能
/// 装下全部 83 个。以前一张勾选表推给所有 CLI，结果是「勾了 83 个，导入 Claude Code
/// 写了 0 个」。现在上面的 tab 决定你在编辑哪一家，勾选框的含义就是「写进这一家」——
/// 每一行自己记着它属于哪几家。
///
/// # 窗口和阈值为什么在这里
///
/// 窗口跟着 **落点** 算（`grok-4.6` 500k、`claude-opus-5` 1M），阈值默认 90%，
/// 于是压缩分别在 450k 和 900k 触发。这两个数逐行不同，一份全局策略表达不了 ——
/// 那正是「界面显示 200k、实际能吃 1M」和「顶着上限死锁」的来历。留空 = 自动。

const IMPORT_TARGETS: CliTarget[] = ["claude-code", "codex", "opencode", "grok-build"];

/// 总控没填时的阈值。和后端 `DEFAULT_COMPACT_PERCENT` 是同一个数。
const DEFAULT_PERCENT = 90;

/// 这一行占的 Claude 槽位。`""` / `"none"` 都是「没占」，和后端 `BridgeEntry::slots()`
/// 同一套判断。可以同时占几个：主模型和 opus 都是 claude-opus-5 是最常见的配法。
function slotsOf(r: BridgeEntry): string[] {
  return (r.tiers ?? []).map((s) => s.trim()).filter((s) => s !== "" && s !== "none");
}

const blank = (targets: CliTarget[]): BridgeEntry => ({
  alias: "",
  target: "",
  contextWindow: 0,
  compactPercent: 0,
  targets,
  tiers: [],
});

export function BridgeTable({
  aliases,
  catalog,
  onMessage,
}: {
  /** 内核渠道上现有的别名，落点下拉的候选。 */
  aliases: string[];
  /** models.dev 目录，用来推断窗口。null = 没拉到，走本地预设。 */
  catalog: Parameters<typeof lookupMeta>[1];
  onMessage: (m: string) => void;
}) {
  const t = useT();
  const qc = useQueryClient();
  const saved = useQuery({ queryKey: ["bridge"], queryFn: api.bridgeList });
  const settings = useQuery({ queryKey: ["app-settings"], queryFn: api.settingsGet });
  const proxyOn = settings.data?.route_cli_through_proxy ?? false;
  // 分档表和上限夹子。不看它们的话这一列显示的「自动会算成多少」和真正写进 CLI
  // 的数会不一样 —— 设置里把 grok 手填成 300k，这里却还显示 500k。而「看得见
  // 会写成多少」正是这张表存在的理由。
  const policy = useQuery({ queryKey: ["context-policy"], queryFn: api.contextPolicyGet });
  // 磁盘上那几个槽位和 modelPicker 现在写着什么。表和磁盘对不上的地方（还没
  // 写入、或者空出的槽位写入时会被清掉）要显示出来，不然用户只能靠猜。
  const preview = useQuery({
    queryKey: ["cli-preview"],
    queryFn: api.cliPreviewAll,
    refetchOnWindowFocus: true,
  });

  // null = 还没动过，显示磁盘上那份。动过之后草稿才是真相 —— 中途 refetch
  // 把用户正在编的表换掉，是「我明明改了」那类 bug 里最气人的一种。
  const [draft, setDraft] = useState<BridgeEntry[] | null>(null);
  const [suffixDraft, setSuffixDraft] = useState<ClaudeSuffix | null>(null);
  const rows = draft ?? saved.data?.entries ?? [];
  const suffix: ClaudeSuffix = suffixDraft ?? saved.data?.claudeSuffix ?? "1M";
  const dirty =
    (draft !== null && JSON.stringify(draft) !== JSON.stringify(saved.data?.entries ?? [])) ||
    (suffixDraft !== null && suffixDraft !== (saved.data?.claudeSuffix ?? "1M"));

  const [tab, setTab] = useState<CliTarget>("claude-code");
  const [prune, setPrune] = useState(false);

  const set = (next: BridgeEntry[]) => setDraft(next);
  const patch = (i: number, p: Partial<BridgeEntry>) =>
    set(rows.map((r, j) => (j === i ? { ...r, ...p } : r)));
  const drop = (i: number) => set(rows.filter((_, j) => j !== i));
  const toggle = (i: number) => {
    const r = rows[i];
    const on = r.targets.includes(tab);
    patch(i, {
      targets: on ? r.targets.filter((x) => x !== tab) : [...r.targets, tab],
    });
  };

  /// 这一行的窗口和压缩触发点。
  ///
  /// 窗口留空时按**落点**推断 —— 出口名是我们编的，`ccload-fast` 什么都推不出来，
  /// 它背后的 `grok-4.6` 才是 500k。推断的优先级必须和后端
  /// `BridgeEntry::window` 一致：分档表手填 → models.dev / 预设，最后套上限夹子。
  /// 对不上的话这一列就是在骗人。
  const overrides = useMemo(() => {
    const m = new Map<string, number>();
    for (const [k, v] of Object.entries(policy.data?.overrides ?? {})) {
      if (v > 0) m.set(tierKey(k), v);
    }
    return m;
  }, [policy.data?.overrides]);

  const resolve = (r: BridgeEntry) => {
    const target = (r.target || r.alias).trim();
    const mode = policy.data?.mode ?? "auto";
    let auto: number;
    if (mode === "off") {
      // 「不写入」档：这一行不带窗口，CLI 保持自己的默认。
      auto = 0;
    } else if (mode === "fixed") {
      // 「固定」档：不看模型名，五家一律这个数。
      auto = policy.data?.fixed_tokens ?? 0;
    } else {
      auto = target ? (overrides.get(tierKey(target)) ?? lookupMeta(target, catalog).context) : 0;
      const cap = policy.data?.cap_tokens ?? 0;
      if (cap > 0 && auto > cap) auto = cap;
    }
    const window = r.contextWindow > 0 ? r.contextWindow : auto;
    const percent =
      r.compactPercent > 0 ? r.compactPercent : policy.data?.compact_percent || DEFAULT_PERCENT;
    return { auto, window, percent, trigger: Math.floor((window * percent) / 100) };
  };

  const save = useMutation({
    mutationFn: () => api.bridgeSave(rows, suffix),
    onSuccess: (r) => {
      qc.setQueryData(["bridge"], { entries: r.entries, claudeSuffix: r.claudeSuffix });
      setDraft(null);
      setSuffixDraft(null);
      onMessage([...r.log, ...r.warnings.map((w) => `⚠ ${w}`)].join("\n"));
    },
    onError: (e) => onMessage(errText(e)),
  });

  // 每家 CLI 认领了几行。按钮上要写清楚这次会动哪几家的配置文件。
  const counts = useMemo(() => {
    const m = new Map<CliTarget, number>();
    for (const r of rows) for (const x of r.targets) m.set(x, (m.get(x) ?? 0) + 1);
    return m;
  }, [rows]);

  // 只写**当前 tab 那一家**。
  //
  // 「正在编辑 Claude Code」旁边摆一个「写进 2 家 CLI」是自相矛盾的：用户刚在
  // 这一档里调完槽位，点下去却顺手把 Grok Build 的 95 条也落了盘。每家的配置
  // 各自留在表里（切 tab 就看得见），写入也就该各写各的。
  const apply = useMutation({
    mutationFn: () => api.bridgeApply([tab], prune),
    onSuccess: (rs) => {
      qc.invalidateQueries({ queryKey: ["cli-preview"] });
      onMessage(
        rs
          .map((r) => {
            const label = TARGET_LABELS[r.target];
            if (r.status === "skipped") return `${label}：跳过 —— ${r.text}`;
            if (r.status === "failed") return `${label}：失败 —— ${r.text}`;
            return `${label}：已写入 ${r.text}`;
          })
          .join("\n"),
      );
    },
    onError: (e) => onMessage(errText(e)),
  });

  // 「补齐」= 把内核现有的别名铺进表里（同名落点，不依赖代理）。
  //
  // Claude Code 那一档**不勾任何行**，只让后端按名字认领 5 个槽位 —— 它没有目录
  // 文件，勾 83 行里的 78 行一个字都写不进去，只会让人在一堆没用的复选框里找。
  const seed = useMutation({
    mutationFn: () =>
      tab === "claude-code"
        ? api.bridgeSeed(aliases, [], true)
        : api.bridgeSeed(aliases, [tab], false),
    onSuccess: (entries) => {
      setDraft(entries);
      onMessage(
        tab === "claude-code"
          ? t("已把内核别名铺进表里，并按名字认领了 Claude Code 的槽位。确认后点「保存」。")
          : t("已按内核别名补齐（同名落点，不依赖代理）。确认后点「保存」。"),
      );
    },
    onError: (e) => onMessage(errText(e)),
  });

  const renamed = rows.filter(
    (r) => r.alias.trim() && r.target.trim() && r.alias.trim() !== r.target.trim(),
  ).length;
  const pickedHere = rows.filter((r) => r.targets.includes(tab)).length;
  const busy = save.isPending || apply.isPending || seed.isPending;

  // 磁盘上那份还没读回来时一个字都不给编。
  //
  // 不挡的话：用户在加载途中点一下「加一行」，草稿就变成 `[空行]`；随后
  // saved.data 带着 20 条到了，但 `rows = draft` —— 那 20 条从屏幕上消失，
  // 点保存会把它们整体抹掉。空表和「还没读到」在这个组件里长得一模一样，
  // 所以必须靠加载态区分，不能靠 `rows.length === 0`。
  if (!saved.isSuccess) {
    return (
      <p className="card bg-surface-raised px-4 py-8 text-center text-sm text-muted">
        {saved.isError ? errText(saved.error) : t("读取出口别名表…")}
      </p>
    );
  }

  return (
    <div className="space-y-3">
      {/* 编辑哪一家。单选而不是多选：勾选框的含义是「写进这一家」，
          多选之下那个含义就说不清了（勾上代表全都写？还是任意一家？）。 */}
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs text-muted">{t("正在编辑")}</span>
        {IMPORT_TARGETS.map((x) => (
          <button
            key={x}
            onClick={() => setTab(x)}
            aria-pressed={tab === x}
            className={cn(
              "flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-sm",
              tab === x
                ? "border-accent bg-accent/12 font-medium text-accent"
                : "border-border text-muted hover:bg-surface-2",
            )}
          >
            {TARGET_LABELS[x]}
            <span className="rounded bg-surface-2 px-1 text-[10px] tabular-nums">
              {counts.get(x) ?? 0}
            </span>
          </button>
        ))}
        <div className="flex-1" />
        {dirty && (
          <span className="rounded-full bg-amber-500/15 px-2 py-0.5 text-[11px] font-medium text-amber-700">
            {t("未保存")}
          </span>
        )}
        <button
          onClick={() => save.mutate()}
          disabled={busy || !dirty}
          className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
        >
          <Save className="h-4 w-4" /> {save.isPending ? t("保存中…") : t("保存")}
        </button>
        <button
          onClick={() => apply.mutate()}
          disabled={busy || dirty || pickedHere === 0}
          title={
            dirty
              ? t("先保存 —— 写进 CLI 用的是已保存的那份")
              : pickedHere === 0
                ? t("这一家一行都没配")
                : t("只写 {cli} 的配置文件，其它几家不动", { cli: TARGET_LABELS[tab] })
          }
          className="flex items-center gap-1 rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
        >
          <Download className="h-4 w-4" />
          {apply.isPending ? t("写入中…") : t("写进 {cli}", { cli: TARGET_LABELS[tab] })}
        </button>
      </div>

      {/* 改名必须经过代理。直连时内核收到的是我们编的名字，它不认，每条请求都
          503 —— 后端保存时会拦，但用户在这里就该看见，而不是点了保存才知道。 */}
      {renamed > 0 && !proxyOn && (
        <p className="rounded-lg border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-800">
          {t(
            "有 {n} 行改了名，但 CLI 现在直连内核 —— 改写只发生在本地代理里，直连时内核收到的是这个新名字、根本不认它。请先去「CLI 接管」页打开「通过本地代理」，否则这些行保存不了。",
            { n: renamed },
          )}
        </p>
      )}

      <div className="flex flex-wrap items-center gap-2 rounded-xl border border-border bg-surface-2/40 px-3 py-2">
        <button
          onClick={() => seed.mutate()}
          disabled={busy || aliases.length === 0}
          title={t("把内核里还没进表的别名补进来，出口名和落点同名。已有的行不动。")}
          className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
        >
          <Wand2 className="h-3.5 w-3.5" /> {t("从内核别名补齐")}
        </button>
        {/* 下面三个只对「装得下很多模型」的 CLI 有意义。Claude Code 是 6 个固定
            位置，批量勾选在那边没有任何含义 —— 摆着只会让人以为勾了就有用。 */}
        {tab !== "claude-code" && (
          <>
            <button
              onClick={() => set([...rows, blank([tab])])}
              className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
            >
              <Plus className="h-3.5 w-3.5" /> {t("加一行")}
            </button>
            <button
              onClick={() =>
                set(rows.map((r) => ({ ...r, targets: [...new Set([...r.targets, tab])] })))
              }
              className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
            >
              {t("全勾给 {cli}", { cli: TARGET_LABELS[tab] })}
            </button>
            <button
              onClick={() =>
                set(rows.map((r) => ({ ...r, targets: r.targets.filter((x) => x !== tab) })))
              }
              className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
            >
              {t("全不给 {cli}", { cli: TARGET_LABELS[tab] })}
            </button>
          </>
        )}
        <div className="flex-1" />
        {/* 只增不删会让 OpenCode / Grok 的目录一路涨：退役的名字留在选择器里，
            选中就是一个 404。默认关着 —— 删配置得是用户明确要的。 */}
        {(tab === "opencode" || tab === "grok-build") && (
          <label className="flex cursor-pointer items-center gap-1.5 text-xs">
            <input type="checkbox" checked={prune} onChange={(e) => setPrune(e.target.checked)} />
            {t("顺手清掉这张表里没有的旧别名")}
          </label>
        )}
      </div>

      {rows.length === 0 ? (
        <div className="rounded-xl border border-dashed border-border px-3 py-8 text-center">
          <p className="text-sm text-muted">{t("还没有出口别名。")}</p>
          <p className="mt-1 text-xs text-muted/70">
            {t("点「从内核别名补齐」把内核现有的名字铺进来 —— 默认同名，行为和现在完全一样，之后再挑几条改名或改窗口。")}
          </p>
        </div>
      ) : tab === "claude-code" ? (
        /* Claude Code 没有模型目录文件：6 个具名槽位 + 一份列表。给它一张 82 行
           的勾选表，用户就得在 82 个复选框里找出该占槽位的那 6 个 —— 那正是用户
           说的「操作很不清晰」。这里把菜单的形状直接画出来。 */
        <ClaudeSlots
          rows={rows}
          aliases={aliases}
          resolve={resolve}
          onChange={set}
          suffix={suffix}
          onSuffix={setSuffixDraft}
          onDisk={
            (preview.data ?? []).find((x) => x.target === "claude-code")?.claude_slots ?? {}
          }
          pickerOnDisk={
            (preview.data ?? []).find((x) => x.target === "claude-code")?.claude_picker ?? []
          }
        />
      ) : (
        <div className="overflow-hidden card">
          <table className="w-full table-fixed text-sm">
            <thead>
              <tr className="border-b border-border bg-surface-2 text-left text-xs text-muted">
                <th className="w-10 px-3 py-2">
                  <span className="sr-only">{t("写进这一家")}</span>
                </th>
                <th className="px-2 py-2">{t("出口别名")}</th>
                <th className="px-2 py-2">{t("落点（内核别名）")}</th>
                <th className="w-40 px-2 py-2">{t("上下文窗口")}</th>
                <th className="w-44 px-2 py-2">{t("压缩阈值")}</th>
                <th className="w-10 px-2 py-2" />
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => {
                const on = r.targets.includes(tab);
                const { auto, window, trigger } = resolve(r);
                const renames = !!r.alias.trim() && !!r.target.trim() && r.alias.trim() !== r.target.trim();
                return (
                  <tr key={i} className={cn("border-b border-border/50", !on && "opacity-45")}>
                    <td className="px-3 py-2">
                      <input
                        type="checkbox"
                        aria-label={t("把「{alias}」写进 {cli}", {
                          alias: r.alias || `#${i + 1}`,
                          cli: TARGET_LABELS[tab],
                        })}
                        checked={on}
                        onChange={() => toggle(i)}
                        className="h-4 w-4"
                      />
                    </td>
                    <td className="px-2 py-2">
                      <TextInput
                        small
                        mono
                        aria-label={t("第 {n} 行的出口别名", { n: i + 1 })}
                        value={r.alias}
                        onChange={(e) => patch(i, { alias: e.target.value })}
                        placeholder={t("CLI 里看到的名字")}
                        className="w-full"
                      />
                    </td>
                    <td className="px-2 py-2">
                      <div className="flex items-center gap-1.5">
                        <ComboBox
                          className="min-w-0 flex-1"
                          aria-label={t("第 {n} 行的落点", { n: i + 1 })}
                          value={r.target}
                          onChange={() => {}}
                          onCommit={(v) => patch(i, { target: v })}
                          placeholder={t("内核里的别名")}
                          options={aliases}
                          emptyHint={t("内核里还没有别名，先去内核后台建渠道")}
                        />
                        {renames && (
                          <span
                            title={t("改了名 —— 只在 CLI 走本地代理时成立")}
                            className="shrink-0 rounded bg-accent/15 px-1.5 py-0.5 text-[10px] text-accent"
                          >
                            {t("改写")}
                          </span>
                        )}
                      </div>
                    </td>
                    <td className="px-2 py-2">
                      <TextInput
                        small
                        type="number"
                        aria-label={t("第 {n} 行的上下文窗口", { n: i + 1 })}
                        value={r.contextWindow || ""}
                        onChange={(e) => patch(i, { contextWindow: Number(e.target.value) || 0 })}
                        placeholder={auto ? String(auto) : t("自动")}
                        className="w-full text-right tabular-nums"
                      />
                    </td>
                    {/* 「500k · 90% → 450k」—— 用户要的就是这一句：这一行的上下文
                        按各自的窗口在各自的点上拆分，而不是一份全局百分比。 */}
                    <td className="px-2 py-2">
                      <div className="flex items-center gap-1">
                        <TextInput
                          small
                          type="number"
                          aria-label={t("第 {n} 行的压缩阈值百分比", { n: i + 1 })}
                          value={r.compactPercent || ""}
                          onChange={(e) => patch(i, { compactPercent: Number(e.target.value) || 0 })}
                          placeholder={String(policy.data?.compact_percent || DEFAULT_PERCENT)}
                          className="w-14 text-right tabular-nums"
                        />
                        <span className="whitespace-nowrap text-[11px] text-muted">
                          % {window > 0 && `→ ${formatWindow(trigger)}`}
                        </span>
                      </div>
                    </td>
                    <td className="px-2 py-2">
                      <button
                        onClick={() => drop(i)}
                        aria-label={t("删掉第 {n} 行", { n: i + 1 })}
                        className="rounded-md border border-border p-1 text-muted hover:bg-surface-2 hover:text-red-600"
                      >
                        <X className="h-3.5 w-3.5" />
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted">
        <span>
          {t("共 {total} 行，{n} 行会写进 {cli}", {
            total: rows.length,
            n: pickedHere,
            cli: TARGET_LABELS[tab],
          })}
        </span>
        {renamed > 0 && (
          <span className="flex items-center gap-1">
            <Check className="h-3 w-3" />
            {t("{n} 行改了名，转发前由本地代理换回落点名", { n: renamed })}
          </span>
        )}
        {rows.length > 0 && (
          <button
            onClick={() => set([])}
            className="ml-auto flex items-center gap-1 text-muted hover:text-red-600"
          >
            <Trash2 className="h-3 w-3" /> {t("清空整张表")}
          </button>
        )}
      </div>
    </div>
  );
}

/// Claude Code 的槽位编辑器。
///
/// # 为什么这一家不给表格
///
/// Claude Code **没有模型目录文件**。它的 `/model` 菜单由三部分拼成：5 个环境变量
/// 槽位（`ANTHROPIC_MODEL` + `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL`）、
/// 一个 `ANTHROPIC_CUSTOM_MODEL_OPTION`，以及 settings.json 里的 `modelPicker` 列表
/// （Claude Code 2.1.243 起）。前六个是具名位置，这里直接把菜单的形状画出来 ——
/// 左边是槽位，右边选一个别名，旁边写清楚它会写成多大窗口、在哪儿触发压缩。
/// 剩下勾了 Claude Code 的行全部进列表，一行一个 chip，想放多少放多少。
///
/// # 一个槽位只能有一行，一行可以占几个槽位
///
/// 两行认领同一个槽位在后端是硬错误（静默后来居上是修过的老 bug）。这里从形状上
/// 就杜绝了：一个槽位一个下拉，选新的自动把旧的那行摘下来。反过来是允许的：
/// 主模型和 opus 都填 claude-opus-5 是最常见的配法，单槽位的模型表达不了它，
/// 表现就是用户磁盘上明明有值、这里却显示空着。
const CLAUDE_SLOTS: { id: string; label: string; env: string; hint: string }[] = [
  { id: "default", label: "主模型", env: "ANTHROPIC_MODEL", hint: "不选模型时用的那个" },
  { id: "opus", label: "opus", env: "ANTHROPIC_DEFAULT_OPUS_MODEL", hint: "/model 里的 Custom Opus" },
  { id: "sonnet", label: "sonnet", env: "ANTHROPIC_DEFAULT_SONNET_MODEL", hint: "/model 里的 Custom Sonnet" },
  { id: "haiku", label: "haiku", env: "ANTHROPIC_DEFAULT_HAIKU_MODEL", hint: "子代理和后台任务走它" },
  { id: "fable", label: "fable", env: "ANTHROPIC_DEFAULT_FABLE_MODEL", hint: "/model 里的 Custom Fable" },
  { id: "custom", label: "自定义项", env: "ANTHROPIC_CUSTOM_MODEL_OPTION", hint: "菜单末尾多出来的一行" },
];

/// 从这个版本起 Claude Code 才认 modelPicker。和后端 `PICKER_MIN_VERSION` 是同一个数。
const PICKER_MIN_VERSION = "2.1.243";

const withClaude = (r: BridgeEntry): BridgeEntry => ({
  ...r,
  targets: [...new Set<CliTarget>([...r.targets, "claude-code"])],
});

function ClaudeSlots({
  rows,
  aliases,
  resolve,
  onChange,
  suffix,
  onSuffix,
  onDisk,
  pickerOnDisk,
}: {
  rows: BridgeEntry[];
  aliases: string[];
  resolve: (r: BridgeEntry) => { auto: number; window: number; percent: number; trigger: number };
  onChange: (next: BridgeEntry[]) => void;
  suffix: ClaudeSuffix;
  onSuffix: (s: ClaudeSuffix) => void;
  /// 磁盘上这几个槽位现在写着什么。槽位 id → 模型名。
  onDisk: Record<string, string>;
  /// 磁盘上 modelPicker 里我们写的那些行。
  pickerOnDisk: string[];
}) {
  const t = useT();
  const mine = (r: BridgeEntry) => r.targets.includes("claude-code");
  const inSlot = (slot: string) => rows.find((r) => mine(r) && slotsOf(r).includes(slot));

  // 候选：表里已有的出口别名 + 内核别名。前者在前 —— 那是用户自己配过的。
  const options = useMemo(
    () => [...new Set([...rows.map((r) => r.alias.trim()).filter(Boolean), ...aliases])],
    [rows, aliases],
  );

  /// 改某一行的窗口 / 阈值。槽位视图里手上只有 row 本身，没有表格那种下标。
  const patchRow = (row: BridgeEntry, p: Partial<BridgeEntry>) =>
    onChange(rows.map((r) => (r === row ? { ...r, ...p } : r)));

  /// 把某个槽位换成 `alias`。空字符串 = 空出这个槽位。
  ///
  /// 旧占用者只摘掉这个槽位，Claude Code 的勾**不动**：它还占着别的槽位就留在
  /// 那儿，一个都不占了就掉进下面的列表 —— 看得见，真不要了去列表里点 ×。
  /// 以前是顺手取消勾选，那时列表还是隐形的，掉进去等于凭空消失。
  const assign = (slot: string, alias: string) => {
    const want = alias.trim();
    let next = rows.map((r) => {
      let out = r;
      const isNew = want !== "" && r.alias.trim() === want;
      if (slotsOf(r).includes(slot) && !isNew) {
        out = { ...out, tiers: slotsOf(out).filter((s) => s !== slot) };
      }
      if (isNew) {
        out = withClaude({ ...out, tiers: [...new Set([...slotsOf(out), slot])] });
      }
      return out;
    });
    // 选了一个表里还没有的名字：补一行（同名落点，不依赖代理）。
    if (want && !rows.some((r) => r.alias.trim() === want)) {
      next = [
        ...next,
        { alias: want, target: want, contextWindow: 0, compactPercent: 0, targets: ["claude-code"] as CliTarget[], tiers: [slot] },
      ];
    }
    onChange(next);
  };

  // 勾了 Claude Code 却没占槽位的那些：全部写进 modelPicker，一行一个 chip。
  // 以前这一堆是隐形的 —— 用户勾了 82 个、看到「已写入」，然后发现 /model 里
  // 还是那 5 个。
  const picker = rows.filter((r) => mine(r) && slotsOf(r).length === 0);
  // 表里还没勾 Claude Code 的行 —— 「全放进来」动的就是它们。
  const rest = rows.filter((r) => !mine(r) && r.alias.trim() !== "");
  const [pick, setPick] = useState("");

  /// 把这些名字勾给 Claude Code（不占槽位 → 进列表）。表里没有的补一行。
  const addToPicker = (names: string[]) => {
    const want = new Set(names.map((a) => a.trim()).filter(Boolean));
    if (want.size === 0) return;
    let next = rows.map((r) => (want.has(r.alias.trim()) ? withClaude(r) : r));
    for (const a of want) {
      if (!rows.some((r) => r.alias.trim() === a)) {
        next = [
          ...next,
          { alias: a, target: a, contextWindow: 0, compactPercent: 0, targets: ["claude-code"] as CliTarget[], tiers: [] },
        ];
      }
    }
    onChange(next);
  };
  const removeFromPicker = (row: BridgeEntry) =>
    onChange(
      rows.map((r) => (r === row ? { ...r, targets: r.targets.filter((x) => x !== "claude-code") } : r)),
    );

  // 磁盘上 modelPicker 里我们的行和这里对不对得上（忽略顺序、忽略窗口后缀）。
  // 对不上就像槽位那样标一个「磁盘：N 行」—— 写入是显式动作。
  const pickerSynced = useMemo(() => {
    const here = [...new Set(picker.map((r) => tierKey(r.alias)))].sort();
    const disk = [...new Set(pickerOnDisk.map((s) => tierKey(s)))].sort();
    return here.length === disk.length && here.every((x, i) => x === disk[i]);
  }, [picker, pickerOnDisk]);

  return (
    <div className="card divide-y divide-border">
      <p className="px-4 py-3 text-xs text-muted">
        {t("Claude Code 没有模型目录文件。/model 菜单 = 下面 6 个槽位 + 一份 modelPicker 列表。每一行三段：槽位 · 写进 CLI 的名字 → 它到了内核落在哪个别名上，两个框都能改。一个名字可以同时占几个槽位。")}
        {/* 「为什么只有 6 个」曾经是这一页最常被问的一句。现在 6 个之外还有
            modelPicker 列表，整张表都放得进 /model；`--model` 也仍然不受限制。 */}
        <span className="mt-1 block text-muted/80">
          {t("读表时磁盘上已有的槽位会先收进来；写入时这里空着的槽位会被清掉。槽位放不下的模型进最下面的列表，整张表都能放进 /model；任何出口别名也都能用 --model <名字> 直接选。")}
        </span>
        <span className="mt-2 flex flex-wrap items-center gap-1.5">
          <span className="text-muted">{t("写进 CLI 的窗口后缀")}</span>
          {(["off", "1m", "1M"] as const).map((k) => (
            <button
              key={k}
              type="button"
              aria-pressed={suffix === k}
              onClick={() => onSuffix(k)}
              className={cn(
                "rounded-md border px-2 py-0.5 font-mono text-[11px]",
                suffix === k
                  ? "border-accent bg-accent/12 text-accent"
                  : "border-border text-muted hover:bg-surface-2",
              )}
            >
              {k === "off" ? t("不写后缀") : `[${k}]`}
            </button>
          ))}
          <span className="text-muted/80">
            {t("Claude Code 只认 [1m]（大小写都行）；没带时 opus-5 / sonnet-5 按 200k 夹。其它窗口（[500k]）走全局上限，跟着主模型。窗口数字来自 models.dev，可在下面手改。")}
          </span>
        </span>
      </p>
      {CLAUDE_SLOTS.map((s) => {
        const row = inSlot(s.id);
        const info = row ? resolve(row) : null;
        return (
          <div key={s.id} className="px-4 py-2.5">
            <div className="flex flex-wrap items-center gap-x-2 gap-y-1.5">
              <div className="w-28 shrink-0">
                <div className="text-sm font-medium">{t(s.label)}</div>
                <div className="font-mono text-[10px] leading-tight text-muted/70">{s.env}</div>
              </div>
              <ComboBox
                className="min-w-0 flex-1"
                aria-label={t("{slot} 槽位发哪个名字", { slot: s.label })}
                value={row?.alias ?? ""}
                onChange={() => {}}
                onCommit={(v) => assign(s.id, v)}
                placeholder={t("空着 —— 不写这个槽位")}
                options={options}
                emptyHint={t("内核里还没有别名，先去内核后台建渠道")}
              />
              {row &&
                suffix !== "off" &&
                info &&
                withWindowSuffix(row.alias, info.window, suffix) !== row.alias.trim() && (
                  <span
                    title={t("写入时会带上这个后缀")}
                    className="shrink-0 font-mono text-[10px] text-muted"
                  >
                    → {withWindowSuffix(row.alias, info.window, suffix)}
                  </span>
                )}
              {/* 落点。表格视图有这一列，槽位视图以前没有 —— 于是这 6 个槽位只能
                  挑别名、不能改它落到哪，而「换上游」恰恰是换模型最常做的事。
                  两个框的含义不同：左边写进 CLI（/model 里显示的名字），右边是
                  代理转发前换成的内核别名。两个框都走提交语义：assign 会**造行**，
                  逐键 onChange 的中间值（`claude-fa`、`claude-fabl`…）会当场各造
                  一行，保存后原样写进 CLI 配置 —— 那批垃圾就是这条路进来的。 */}
              <span aria-hidden className="shrink-0 text-muted/60">
                →
              </span>
              <ComboBox
                className="min-w-0 flex-1"
                aria-label={t("{slot} 槽位落到哪个内核别名", { slot: s.label })}
                value={row?.target ?? ""}
                onChange={() => {}}
                onCommit={(v) => row && patchRow(row, { target: v })}
                placeholder={row ? t("内核里的别名") : t("先在左边选一个")}
                options={aliases}
                emptyHint={t("内核里还没有别名，先去内核后台建渠道")}
              />
              {row && (
                <button
                  onClick={() => assign(s.id, "")}
                  aria-label={t("空出 {slot} 槽位", { slot: s.label })}
                  className="shrink-0 rounded-md border border-border p-1 text-muted hover:bg-surface-2 hover:text-red-600"
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              )}
            </div>
            {/* 窗口和阈值在这里也要能改。表格视图有这两个输入框，槽位视图以前
                只把算出来的数显示出来 —— 同一件事在两个 tab 里能力不一样，
                用户会以为 Claude Code 这一档根本不支持。留空 = 跟总控走。 */}
            <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 pl-[7.5rem]">
              {row ? (
                <>
                  <SlotWindow row={row} info={info} onPatch={(p) => patchRow(row, p)} />
                  {row.alias.trim() !== (row.target || row.alias).trim() && (
                    <span
                      title={t("改了名 —— 只在 CLI 走本地代理时成立")}
                      className="rounded bg-accent/15 px-1.5 py-0.5 text-[10px] text-accent"
                    >
                      {t("改写")}
                    </span>
                  )}
                  {/* 同一行还占着别的槽位时说一声：改这一行的落点或窗口，那几个
                      槽位一起变 —— 它们本来就是同一条记录。 */}
                  {slotsOf(row).length > 1 && (
                    <span className="text-[11px] text-muted">
                      {t("同时占着：{slots}", {
                        slots: slotsOf(row)
                          .filter((x) => x !== s.id)
                          .map((x) => t(CLAUDE_SLOTS.find((c) => c.id === x)?.label ?? x))
                          .join(" / "),
                      })}
                    </span>
                  )}
                </>
              ) : (
                <span className="text-[11px] text-muted">{t(s.hint)}</span>
              )}
              <DiskNote
                slot={s.id}
                row={row}
                onDisk={onDisk}
                written={
                  row && info ? withWindowSuffix(row.alias, info.window, suffix) : row?.alias
                }
              />
            </div>
          </div>
        );
      })}

      {/* 第 7 段：modelPicker 列表。槽位之外还想放进 /model 的模型都在这里，
          想放多少放多少 —— 「为什么只有 6 个」这个问题从此不成立。 */}
      <div className="px-4 py-2.5">
        <div className="flex flex-wrap items-start gap-x-2 gap-y-1.5">
          <div className="w-28 shrink-0">
            <div className="text-sm font-medium">{t("菜单里的其它模型")}</div>
            <div className="font-mono text-[10px] leading-tight text-muted/70">modelPicker</div>
          </div>
          <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5 py-0.5">
            {picker.length === 0 && (
              <span className="text-[11px] text-muted">{t("空着 —— /model 里只有上面那 6 个")}</span>
            )}
            {picker.map((r) => {
              const renames = r.alias.trim() !== (r.target || r.alias).trim();
              return (
                <span
                  key={r.alias}
                  className="flex items-center gap-1 rounded-md border border-border bg-surface-2/60 px-1.5 py-0.5 font-mono text-[11px]"
                >
                  {r.alias}
                  {renames && <span className="text-muted">→ {r.target}</span>}
                  <button
                    onClick={() => removeFromPicker(r)}
                    aria-label={t("把「{alias}」从 /model 菜单拿掉", { alias: r.alias })}
                    className="text-muted hover:text-red-600"
                  >
                    <X className="h-3 w-3" />
                  </button>
                </span>
              );
            })}
          </div>
        </div>
        <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 pl-[7.5rem]">
          <ComboBox
            className="w-60"
            aria-label={t("往 /model 菜单里加一个别名")}
            value={pick}
            onChange={setPick}
            // 回车或点选候选 = 直接加进菜单。这里 onChange 只回显文本（「加进
            // 菜单」按钮要用它），造行发生在 addToPicker，不能逐键触发。
            onCommit={(v) => {
              if (v.trim()) {
                addToPicker([v]);
                setPick("");
              }
            }}
            placeholder={t("加一个别名")}
            options={options.filter((o) => !rows.some((r) => mine(r) && r.alias.trim() === o))}
            emptyHint={t("内核里还没有别名，先去内核后台建渠道")}
          />
          <button
            onClick={() => {
              addToPicker([pick]);
              setPick("");
            }}
            disabled={!pick.trim()}
            className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
          >
            {t("加进菜单")}
          </button>
          <button
            onClick={() => addToPicker(rest.map((r) => r.alias))}
            disabled={rest.length === 0}
            title={t("把表里其余的行全部勾给 Claude Code，它们会出现在 /model 菜单末尾")}
            className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
          >
            {t("其余 {n} 行全放进来", { n: rest.length })}
          </button>
          <span className="text-[11px] text-muted">
            {t("需要 Claude Code ≥ {v}；更早的版本会忽略这一段", { v: PICKER_MIN_VERSION })}
          </span>
          {!pickerSynced && (
            <span
              title={t("磁盘上的 modelPicker 和这里不一致 —— 点「写进 Claude Code」同步")}
              className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-[10px] text-muted"
            >
              {t("磁盘：")}
              {t("{n} 行", { n: pickerOnDisk.length })}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

/// 槽位那一行右边的「窗口 / 阈值」两个格子。
///
/// 和表格视图里的两列是同一件事，只是挤在一行里。两处都要能改 —— 只在表格里
/// 给输入框、在槽位视图里显示只读文字，会让人以为 Claude Code 这一档不支持
/// 逐模型的窗口。留空 = 跟总控走，占位符显示的就是总控会算出来的那个数。
function SlotWindow({
  row,
  info,
  onPatch,
}: {
  row: BridgeEntry;
  info: { auto: number; window: number; percent: number; trigger: number } | null;
  onPatch: (p: Partial<BridgeEntry>) => void;
}) {
  const t = useT();
  return (
    <div className="flex shrink-0 items-center gap-1">
      <TextInput
        small
        type="number"
        aria-label={t("「{alias}」的上下文窗口", { alias: row.alias })}
        title={t("留空 = 跟上下文窗口总控走")}
        value={row.contextWindow || ""}
        onChange={(e) => onPatch({ contextWindow: Number(e.target.value) || 0 })}
        placeholder={info?.auto ? String(info.auto) : t("自动")}
        className="w-24 text-right tabular-nums"
      />
      <TextInput
        small
        type="number"
        aria-label={t("「{alias}」的压缩阈值百分比", { alias: row.alias })}
        title={t("窗口的百分之几触发自动压缩。留空 = 跟总控走")}
        value={row.compactPercent || ""}
        onChange={(e) => onPatch({ compactPercent: Number(e.target.value) || 0 })}
        placeholder="90"
        className="w-12 text-right tabular-nums"
      />
      <span className="w-16 whitespace-nowrap text-[11px] text-muted">
        % {info && info.window > 0 ? `→ ${formatWindow(info.trigger)}` : ""}
      </span>
    </div>
  );
}

/// 「磁盘上还写着 X」。
///
/// 表和磁盘对不上的两种情形都得说出来：表里是新名字、磁盘还是旧的（点写入才
/// 覆盖）；或者槽位在表里空着、磁盘上还有（读表时磁盘上的槽位会先收进来，所以
/// 这一种只在用户刚点过 × 之后出现 —— 写入会清掉它，要留就得选回来）。
///
/// 只在「磁盘上有、而表里和它不一样」时出现。一致时说了等于噪音。
function DiskNote({
  slot,
  row,
  onDisk,
  written,
}: {
  slot: string;
  row: BridgeEntry | undefined;
  onDisk: Record<string, string>;
  /** 写入时会落在磁盘上的名字（含窗口后缀）。跟磁盘一致就不提示。 */
  written?: string;
}) {
  const t = useT();
  const disk = (onDisk[slot] ?? "").trim();
  if (!disk) return null;
  if (row && (written ?? row.alias).trim() === disk) return null;
  return (
    <span
      title={
        row
          ? t("表里是这个名字，磁盘上还是旧的 —— 点「写进 Claude Code」才会覆盖")
          : t("磁盘上还有它，写入时会清掉 —— 要留就在左边选回来")
      }
      className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-[10px] text-muted"
    >
      {t("磁盘：")}
      {disk}
    </span>
  );
}
