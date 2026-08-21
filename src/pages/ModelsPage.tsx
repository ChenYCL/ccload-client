import { useMutation, useQuery } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { Check, Download, Eye, RefreshCw, Radar, Wand2 } from "lucide-react";
import { useT } from "../i18n";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { fetchCatalog, lookupMeta } from "../lib/modelCatalog";
import { ALL_TARGETS, TARGET_LABELS } from "../lib/targets";
import { buildUpstreamIndex, matchAlias, type MatchLevel } from "../lib/modelMatch";
import { Select, TextInput } from "../components/ui/Input";
import type { CliTarget, ImportEntry, VisionTargetState } from "../types";
import { errText } from "../lib/err";

/// One-click model catalog import. The kernel can serve every alias that any
/// channel exposes (`models[].model`), but each CLI needs the alias written
/// into its own config before the user can select it. This page aggregates
/// the aliases, lets the user pin context windows, and writes the catalog:
///   * Claude Code → 只写用户显式指定的 tier 槽位（没有目录文件可写）
///   * Codex       → [profiles.<别名>]，顶层 model 不碰
///   * OpenCode    → 合并进 provider.ccload.models，顶层 model 只在缺失时补
/// Also installs the vision MCP (this binary's `vision-mcp` subcommand) so a
/// text-only model gets image descriptions from a multimodal one.
//
// 模型目录只有这三家有可写的位置（Gemini / Grok 的配置里没有模型清单这一节）；
// 视觉 MCP 则是 5 家都能装 —— 它走的是通用 MCP 写入器。
const IMPORT_TARGETS: CliTarget[] = ["claude-code", "codex", "opencode"];
const VISION_TARGETS: CliTarget[] = ALL_TARGETS;

type ChannelModel = { model?: string };
type Channel = {
  id?: number;
  name?: string;
  enabled?: boolean;
  models?: ChannelModel[];
};
/// GET /admin/channels/:id/models/fetch 的响应。内核会按渠道声明的协议去问上游
/// 的 /v1/models，`source` 区分是真拉到的（api）还是内核的预置清单（predefined）。
type ProbeResult = {
  per: { name: string; ok: boolean; ids: string[]; err: string }[];
  ids: string[];
};

type FetchModelsResponse = {
  models?: { model?: string }[];
  protocol?: string;
  source?: string;
};
type RowState = {
  checked: boolean;
  contextWindow: number;
  tier: string;
  vision: boolean;
  fromCatalog: boolean;
};

/// Claude Code 的模型槽位。它没有模型目录文件，能承载模型的位置就是这 5 个
/// 环境变量（ANTHROPIC_MODEL + ANTHROPIC_DEFAULT_*_MODEL）。`none` 表示这一行
/// 只是目录条目，不占槽位 —— 导入 Claude Code 时会跳过它，也就不会覆盖用户
/// 现有的绑定。
const TIERS = ["none", "default", "fable", "sonnet", "opus", "haiku"] as const;
const TIER_LABELS: Record<string, string> = {
  none: "不绑定",
  default: "default（主模型）",
  fable: "fable",
  sonnet: "sonnet",
  opus: "opus",
  haiku: "haiku",
};

/// Claude Code 只有这 5 个槽位。按别名里的 opus/sonnet/… 字样猜，每个槽位只填一次。
function guessTierFromName(alias: string): (typeof TIERS)[number] | null {
  const n = alias.toLowerCase();
  if (n.includes("fable")) return "fable";
  if (n.includes("opus")) return "opus";
  if (n.includes("sonnet")) return "sonnet";
  if (n.includes("haiku")) return "haiku";
  return null;
}

/// 一个目标的写入结果。`skipped` 必须和 `ok` 分开：跳过等于**没写**，塞进
/// `ok` 再靠 `text` 解释，回显模板套出来就是「已写入 跳过（…）」这种自相矛盾的
/// 话，用户没法判断到底写没写。视觉 MCP 那条路只会产出 ok / failed 两态。
type TargetOutcome = {
  t: CliTarget;
  status: "ok" | "skipped" | "failed";
  text: string;
};

/// 批量写多个 CLI：一个失败不影响其余，每个目标都带回自己的成败。
/// 必须串行。五路并行会同时改 `backups/manifest.json`，短写入叠在旧文件尾巴上，
/// 表现就是截图里的 `trailing characters at line 46`，装和卸全部卡死。
async function visionBatch(
  targets: CliTarget[],
  enabled: boolean,
  model?: string,
): Promise<TargetOutcome[]> {
  const out: TargetOutcome[] = [];
  for (const t of targets) {
    try {
      const written = await api.visionMcpSet(t, enabled, model);
      out.push({
        t,
        status: "ok",
        text: written.join("、") || (enabled ? "已安装" : "已移除"),
      });
    } catch (e) {
      out.push({ t, status: "failed", text: errText(e) });
    }
  }
  return out;
}

function summarize(rs: TargetOutcome[], okWord: string, failWord: string): string {
  return rs
    .map((r) => `${TARGET_LABELS[r.t]}：${r.status === "ok" ? okWord : `${failWord} —— ${r.text}`}`)
    .join("\n");
}

/// 模型导入的三态回显。跳过要说清「没写、为什么、其它家不受牵连」——它出现的
/// 场合就是混选 Claude + Codex，用户得看得出另外几家是照常写了的。
function describeImport(r: TargetOutcome): string {
  const label = TARGET_LABELS[r.t];
  if (r.status === "skipped") return `${label}：跳过 —— ${r.text}`;
  if (r.status === "failed") return `${label}：失败 —— ${r.text}`;
  return `${label}：已写入 ${r.text}`;
}

export function ModelsPage() {
  const t = useT();
  const channels = useQuery({
    queryKey: ["channels"],
    queryFn: () => api.admin<Channel[]>("GET", "channels"),
  });

  // Third-party catalog for context window / vision; local regex presets
  // are the fallback for offline use and custom aliases.
  const catalog = useQuery({
    queryKey: ["model-catalog"],
    queryFn: fetchCatalog,
    staleTime: 6 * 60 * 60 * 1000,
    retry: 1,
  });

  // 只看**启用中**的渠道。停用的渠道内核根本不会选它，把它的模型写进 CLI 目录
  // 等于给用户一堆点了就报错的选项。
  const liveChannels = useMemo(
    () => (channels.data?.data ?? []).filter((c) => c.enabled !== false),
    [channels.data],
  );
  const disabledCount = (channels.data?.data ?? []).length - liveChannels.length;

  const aliases = useMemo(() => {
    const set = new Set<string>();
    for (const ch of liveChannels) {
      for (const m of ch.models ?? []) {
        if (m.model) set.add(m.model);
      }
    }
    return [...set].sort();
  }, [liveChannels]);

  const [rows, setRows] = useState<Record<string, RowState>>({});
  const row = (alias: string): RowState => {
    if (rows[alias]) return rows[alias];
    const meta = lookupMeta(alias, catalog.data ?? null);
    return {
      checked: true,
      contextWindow: meta.context,
      tier: "none",
      vision: meta.vision,
      fromCatalog: meta.source === "catalog",
    };
  };

  const setRow = (alias: string, patch: Partial<RowState>) =>
    setRows({ ...rows, [alias]: { ...row(alias), ...patch } });

  // 目标是多选：同一批别名往往要同时写进 Claude Code 和 Codex，一次一个的话
  // 表格上的勾选和上下文窗口要重来一遍。逐目标独立成败，结果逐条回显。
  const [targets, setTargets] = useState<CliTarget[]>(["claude-code"]);
  const toggleTarget = (t: CliTarget) =>
    setTargets((prev) =>
      prev.includes(t) ? prev.filter((x) => x !== t) : [...prev, t],
    );
  const [visionPicked, setVisionPicked] = useState<CliTarget[]>([]);
  const [message, setMessage] = useState<string | null>(null);

  // 上游校验：把**所有**渠道的真实模型清单拉回来取并集，用它判断这几百个别名里
  // 哪些是真能用的。
  //
  // 不让用户挑单个渠道：别名本来就是全渠道聚合出来的，一个别名可能只有某一家有；
  // 拿单家的清单去判其余的「上游无」是错的。内核已经实现了「按渠道声明的协议去问
  // 上游要模型列表」（GET /admin/channels/:id/models/fetch），客户端逐个渠道调它、
  // 合并结果，只负责匹配和筛选。
  const probe = useMutation({
    mutationFn: async (): Promise<ProbeResult> => {
      const list = liveChannels.filter((c) => c.id !== undefined);
      const per = await Promise.all(
        list.map(async (c) => {
          try {
            const r = await api.admin<FetchModelsResponse>(
              "GET",
              `channels/${c.id}/models/fetch`,
            );
            const ids = (r.data?.models ?? []).map((m) => m.model ?? "").filter(Boolean);
            // 内核对拉取失败是返回 200 + success=false 的（上游报错属预期内），
            // 所以「拿到 0 个」也算这家没给出清单，要单独标出来而不是当成空集。
            return { name: c.name ?? `#${c.id}`, ok: ids.length > 0, ids, err: "" };
          } catch (e) {
            return { name: c.name ?? `#${c.id}`, ok: false, ids: [] as string[], err: errText(e) };
          }
        }),
      );
      const union = new Set<string>();
      for (const r of per) for (const id of r.ids) union.add(id);
      return { per, ids: [...union] };
    },
    onError: (e) => setMessage(errText(e)),
  });
  const upstreamIds = probe.data?.ids ?? [];
  const probedOk = probe.data?.per.filter((p) => p.ok).length ?? 0;
  const probedFail = probe.data?.per.filter((p) => !p.ok) ?? [];
  const index = useMemo(
    () => buildUpstreamIndex(upstreamIds),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [upstreamIds.join("\u0000")],
  );
  const matches = useMemo(() => {
    // 用「探测跑过没有」而不是「并集是不是空」做判据：全部渠道都拉不到清单时
    // 并集就是空，若此时返回 null，界面既不出「上游」列也不出失败提示，
    // 用户看到的就是「点了拉取但什么都没变」。
    if (!probe.data) return null;
    const m = new Map<string, ReturnType<typeof matchAlias>>();
    for (const a of aliases) m.set(a, matchAlias(a, index));
    return m;
  }, [aliases, index, probe.data]);


  const apply = useMutation({
    mutationFn: async () => {
      const entries: ImportEntry[] = aliases
        .filter((a) => row(a).checked)
        .map((a) => ({
          alias: a,
          contextWindow: row(a).contextWindow,
          tier: row(a).tier,
        }));
      // 逐个写：并行会和视觉 MCP 一样把备份清单踩坏。一家失败不影响其余。
      const rs: TargetOutcome[] = [];
      for (const tg of targets) {
        try {
          // Claude Code 没选槽位时后端会整次失败；其它 CLI 不认 tier，
          // 混选时跳过 Claude，让 Codex/OpenCode 照常写入。
          if (
            tg === "claude-code" &&
            entries.every((e) => !e.tier || e.tier === "none")
          ) {
            rs.push({
              t: tg,
              status: "skipped",
              text: t("没给任何模型选 Tier 槽位，配置未改动；其它 CLI 见各自那行"),
            });
            continue;
          }
          // tier 只有 Claude Code 认；别家传 null，避免写进不认识的字段。
          const forTarget =
            tg === "claude-code"
              ? entries
              : entries.map((e) => ({ ...e, tier: null }));
          const r = await api.modelImport(tg, forTarget);
          const note =
            r.skipped.length > 0
              ? `（${r.skipped.length} 个模型没选 Tier 槽位，未写入）`
              : "";
          rs.push({ t: tg, status: "ok", text: r.written.join("、") + note });
        } catch (e) {
          rs.push({ t: tg, status: "failed", text: errText(e) });
        }
      }
      return rs;
    },
    onSuccess: (rs) => setMessage(rs.map(describeImport).join("\n")),
    onError: (e) => setMessage(errText(e)),
  });

  // 视觉 MCP 装没装、用的哪个模型，读回各 CLI 的真实配置 —— 按钮和下拉都不该
  // 凭记忆显示状态：用户可能在别处删过，也可能上一版客户端根本没写成功
  //（opencode 目标名曾经就是错的）。模型这一项以前只活在下面那个 useState 里，
  // 切走再回来就回到「选择多模态模型」，看起来就是「选了没保存上」。
  const visionState = useQuery({
    queryKey: ["vision-mcp-state"],
    queryFn: api.visionMcpState,
  });
  const visionByTarget = useMemo(() => {
    const m = new Map<CliTarget, VisionTargetState>();
    for (const s of visionState.data ?? []) m.set(s.target, s);
    return m;
  }, [visionState.data]);

  // 已装的那些用的是哪个模型。多数情况五家一致，取第一个即可；不一致时下面
  // 会单独提示，因为「改一次全改」是这个下拉给人的印象，不说就是骗人。
  const installedModels = useMemo(
    () => [
      ...new Set(
        (visionState.data ?? [])
          .filter((s) => s.installed && s.model)
          .map((s) => s.model as string),
      ),
    ],
    [visionState.data],
  );

  // 用户改过就以用户的为准；没改过就跟着磁盘上的值走（含刚装完的回显）。
  // 用 null 而不是 "" 表示「还没动过」—— "" 是一个合法的用户选择（清空）。
  const [visionPick, setVisionPick] = useState<string | null>(null);
  const visionModel = visionPick ?? installedModels[0] ?? "";

  const vision = useMutation({
    mutationFn: (ts: CliTarget[]) => visionBatch(ts, true, visionModel || undefined),
    onSuccess: async (rs) => {
      setMessage(summarize(rs, t("已安装"), t("安装失败")));
      // 先把磁盘上的新值取回来，**再**把选择权交还给它。顺序反了的话中间那一
      // 帧读到的还是旧数据（首次安装时是「什么都没装」），下拉会闪回占位符
      // ——正好是这次要修的那个 bug 的样子。
      await visionState.refetch();
      setVisionPick(null);
    },
    onError: (e) => setMessage(errText(e)),
  });
  const visionOff = useMutation({
    mutationFn: (ts: CliTarget[]) => visionBatch(ts, false),
    onSuccess: (rs) => {
      setMessage(summarize(rs, t("已移除"), t("移除失败")));
      visionState.refetch();
    },
    onError: (e) => setMessage(errText(e)),
  });

  // One-click defaults: back to whatever the catalog/presets say.
  const resetDefaults = () => {
    const next: Record<string, RowState> = {};
    for (const a of aliases) {
      const meta = lookupMeta(a, catalog.data ?? null);
      next[a] = {
        checked: row(a).checked,
        contextWindow: meta.context,
        tier: "none",
        vision: meta.vision,
        fromCatalog: meta.source === "catalog",
      };
    }
    setRows(next);
  };

  /// 按上游匹配结果批量勾选。这是这页最费手的操作 —— 472 行里手动挑出上游
  /// 真有的那些，不给一键筛选等于没做。
  const selectByMatch = (keep: (lvl: MatchLevel) => boolean) => {
    if (!matches) return;
    const next: Record<string, RowState> = { ...rows };
    for (const a of aliases) {
      next[a] = { ...row(a), checked: keep(matches.get(a)?.level ?? "missing") };
    }
    setRows(next);
  };

  const checkedCount = aliases.filter((a) => row(a).checked).length;
  const allChecked = aliases.length > 0 && checkedCount === aliases.length;
  const setAllChecked = (on: boolean) => {
    const next: Record<string, RowState> = { ...rows };
    for (const a of aliases) next[a] = { ...row(a), checked: on };
    setRows(next);
  };
  // Tier 只有 Claude Code 用得上；多选里只要它在，这一列就要能编辑。
  const showTier = targets.includes("claude-code");
  const visionCandidates = aliases.filter((a) => row(a).vision);
  // 下拉里必须包含**当前已装的那个模型**，哪怕目录判它不是多模态、或者它已经
  // 不在渠道清单里了。受控 <select> 的 value 找不到对应 option 时浏览器渲染成
  // 空白 —— 那正是「明明装着模型，界面却显示占位符」的另一种成因。
  const visionOptions = useMemo(
    () => [...new Set([...visionCandidates, ...installedModels])].sort(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [visionCandidates.join("\u0000"), installedModels.join("\u0000")],
  );
  // 含 Claude Code 且一个槽位都没选时，后端必然报错。只选了 Claude 才禁用
  // 按钮；和 Codex/OpenCode 一起选时仍可导入其它家，Claude 那路会跳过。
  const claudeNeedsSlot =
    targets.includes("claude-code") &&
    aliases.every((a) => !row(a).checked || row(a).tier === "none");
  const claudeIsOnlyTarget = targets.length === 1 && targets[0] === "claude-code";
  const importBlocked = claudeNeedsSlot && claudeIsOnlyTarget;

  const fillTiersByName = () => {
    const used = new Set(
      aliases.filter((a) => row(a).checked && row(a).tier !== "none").map((a) => row(a).tier),
    );
    const next: Record<string, RowState> = { ...rows };
    for (const a of aliases) {
      if (!row(a).checked || row(a).tier !== "none") continue;
      const guessed = guessTierFromName(a);
      if (!guessed || used.has(guessed)) continue;
      used.add(guessed);
      next[a] = { ...row(a), tier: guessed };
    }
    setRows(next);
  };

  const bindFirstAsDefault = () => {
    if (aliases.some((a) => row(a).checked && row(a).tier === "default")) return;
    const first = aliases.find((a) => row(a).checked);
    if (!first) return;
    setRow(first, { tier: "default" });
  };

  return (
    <div>
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="t-display">{t("模型导入")}</h1>
          <p className="mt-1 text-sm text-muted">
            {t("从内核渠道聚合所有可用模型别名，")}<strong>{t("追加")}</strong>{t("进各 CLI 的模型目录： Codex 每个别名一个")} <code>{t("[profiles.别名]")}</code>（<code>codex --profile</code> {t("选用）、 OpenCode 合并进")} <code>provider.ccload.models</code>{t("。 两者都不会动你当前正在用的模型。Claude Code 没有目录文件，只有 5 个槽位， 所以要在 Tier 列显式指定 —— 没指定的行不写。")}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            onClick={resetDefaults}
            title={t("按 models.dev 目录与本地预设重填所有行的上下文窗口")}
            className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2"
          >
            <Wand2 className="h-4 w-4" /> {t("填充默认值")}
          </button>
          <button
            onClick={() => catalog.refetch()}
            disabled={catalog.isFetching}
            title={t("重新拉取 models.dev 模型目录")}
            className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
          >
            <RefreshCw className={"h-4 w-4" + (catalog.isFetching ? " animate-spin" : "")} />
            {catalog.isSuccess ? t("已同步") : catalog.isError ? t("目录不可用") : t("同步中")}
          </button>
        </div>
      </div>

      {catalog.isError && (
        <p className="mt-3 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs">
          {t("models.dev 拉取失败，上下文窗口暂用本地预设值（claude 20 万、gemini 100 万等）， 联网后点「同步」重试。")}
        </p>
      )}

      {aliases.length === 0 && (
        <p className="mt-6 text-sm text-muted">
          {channels.isLoading
            ? t("读取渠道中…")
            : t("内核里还没有渠道或渠道没有配置模型，先去「内核后台」添加。")}
        </p>
      )}

      {aliases.length > 0 && (
        <>
          <div className="mt-5 flex flex-wrap items-center gap-2">
            <span className="text-xs text-muted">{t("写入到")}</span>
            {IMPORT_TARGETS.map((tg) => {
              const on = targets.includes(tg);
              return (
                <button
                  key={tg}
                  onClick={() => toggleTarget(tg)}
                  aria-pressed={on}
                  className={cn(
                    "flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-sm",
                    on
                      ? "border-accent bg-accent/12 font-medium text-accent"
                      : "border-border text-muted hover:bg-surface-2",
                  )}
                >
                  {on ? <Check className="h-3.5 w-3.5" /> : null}
                  {TARGET_LABELS[tg]}
                </button>
              );
            })}
            <span className="ml-2 text-xs text-muted">
              {t("已选")} {checkedCount}/{aliases.length} {t("个模型")}
              {disabledCount > 0 && `（已排除 ${disabledCount} 个停用渠道）`}
            </span>
            <button
              onClick={() => apply.mutate()}
              disabled={
                apply.isPending ||
                checkedCount === 0 ||
                targets.length === 0 ||
                importBlocked
              }
              title={
                importBlocked
                  ? t("Claude Code 没有模型目录文件：请在 Tier 列给至少一个模型选一个槽位")
                  : undefined
              }
              className="ml-auto flex items-center gap-1 rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
            >
              <Download className="h-4 w-4" />
              {apply.isPending
                ? t("写入中…")
                : `导入到 ${targets.length} 个 CLI`}
            </button>
          </div>

          {claudeNeedsSlot && (
            <div className="mt-3 flex flex-wrap items-center gap-2 rounded-xl border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-900">
              <span>
                {t(
                  "Claude Code 没有模型目录，只能绑 5 个槽位。现在勾选的行全是「不绑定」，导入不会改 Claude Code。",
                )}
              </span>
              <button
                type="button"
                onClick={fillTiersByName}
                className="rounded-lg border border-amber-500/40 bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
              >
                {t("按名称填槽位")}
              </button>
              <button
                type="button"
                onClick={bindFirstAsDefault}
                className="rounded-lg border border-amber-500/40 bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
              >
                {t("把第一个勾选的设为主模型")}
              </button>
            </div>
          )}

          {/* 上游校验。内核已经能「按渠道声明的协议去问上游要模型清单」
              （GET /admin/channels/:id/models/fetch），这里只负责把它拉回来、
              和别名做匹配、给出一键筛选 —— 匹配规则见 lib/modelMatch.ts。 */}
          <div className="mt-3 flex flex-wrap items-center gap-2 rounded-xl border border-border bg-surface-2/40 px-3 py-2">
            <span className="flex items-center gap-1.5 text-xs text-muted">
              <Radar className="h-3.5 w-3.5" /> {t("上游校验")}
            </span>
            <button
              onClick={() => probe.mutate()}
              disabled={probe.isPending || liveChannels.length === 0}
              title={t("逐个渠道向上游要一次模型列表，然后取并集")}
              className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
            >
              {probe.isPending
                ? `拉取中…（${liveChannels.length} 个渠道）`
                : t("拉取全部渠道的上游模型")}
            </button>

            {matches && (
              <>
                <span className="text-xs text-muted">
                  {probedOk} {t("个渠道给出清单，并集")} {upstreamIds.length} {t("个模型：精确")}{" "}
                  {countBy(matches, "exact")} {t("· 模糊")} {countBy(matches, "fuzzy")} {t("· 缺失")}{" "}
                  {countBy(matches, "missing")}
                </span>
                <button
                  onClick={() => selectByMatch((l) => l === "exact")}
                  className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
                >
                  {t("只选精确命中")}
                </button>
                <button
                  onClick={() => selectByMatch((l) => l !== "missing")}
                  className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
                >
                  {t("含模糊命中")}
                </button>
                {/* 没给出清单的渠道要点名。它们服务的别名会被判成「上游无」，
                    不说清楚的话用户会误删掉能用的模型。 */}
                {probedFail.length > 0 && (
                  <div className="basis-full rounded-lg bg-amber-500/10 px-2.5 py-1.5 text-[11px] text-amber-800">
                    <div className="font-medium">
                      {probedFail.length} {t("个渠道没返回模型清单 —— 它们上的别名会被算成 「上游无」，别据此删掉")}
                    </div>
                    <ul className="mt-0.5 space-y-0.5">
                      {probedFail.map((f) => (
                        <li key={f.name} className="break-all">
                          {f.name}
                          {f.err ? `：${f.err}` : t("：上游没有可用的 /v1/models 或返回为空")}
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </>
            )}
          </div>

          <div className="mt-3 overflow-hidden card">
            <table className="w-full table-fixed text-sm">
              <thead>
                <tr className="border-b border-border bg-surface-2 text-left text-xs text-muted">
                  <th className="w-12 px-3 py-2 text-left">
                    {/* 几十行别名逐个点太慢；表头这个既是「全选/全不选」，也是当前
                        选中比例的指示器（部分选中时显示 indeterminate）。 */}
                    <input
                      type="checkbox"
                      aria-label={allChecked ? t("全不选") : t("全选")}
                      title={allChecked ? t("全不选") : t("全选")}
                      checked={allChecked}
                      ref={(el) => {
                        if (el) el.indeterminate = !allChecked && checkedCount > 0;
                      }}
                      onChange={() => setAllChecked(!allChecked)}
                      className="h-4 w-4"
                    />
                  </th>
                  <th className="px-2 py-2 text-left">{t("模型别名")}</th>
                  <th className="w-56 px-2 py-2 text-left">{t("上下文窗口")}</th>
                  {showTier && <th className="w-32 px-2 py-2 text-left">Tier</th>}
                  <th className="w-24 px-2 py-2 text-left">{t("视觉")}</th>
                  {matches && <th className="w-28 px-2 py-2 text-left">{t("上游")}</th>}
                </tr>
              </thead>
              <tbody>
                {aliases.map((a) => {
                  const r = row(a);
                  return (
                    <tr
                      key={a}
                      className={
                        "border-b border-border/50 " +
                        (r.checked ? "" : "opacity-50")
                      }
                    >
                      <td className="px-3 py-2">
                        <input
                          type="checkbox"
                          aria-label={a}
                          checked={r.checked}
                          onChange={(e) => setRow(a, { checked: e.target.checked })}
                          className="h-4 w-4"
                        />
                      </td>
                      <td className="truncate px-2 py-2 font-mono text-xs" title={a}>{a}</td>
                      <td className="px-2 py-2">
                        <div className="flex items-center gap-1.5">
                          <TextInput
                            small
                            type="number"
                            aria-label={`${a} 的上下文窗口`}
                            value={r.contextWindow}
                            onChange={(e) =>
                              setRow(a, { contextWindow: Number(e.target.value) })
                            }
                            className="w-28 text-right tabular-nums"
                          />
                          {r.fromCatalog && (
                            <span
                              className="shrink-0 whitespace-nowrap rounded bg-emerald-500/15 px-1.5 py-0.5 text-[10px] text-emerald-700"
                              title={t("来自 models.dev 目录")}
                            >
                              {t("目录")}
                            </span>
                          )}
                        </div>
                      </td>
                      {showTier && (
                        <td className="px-2 py-2">
                          <Select
                            small
                            aria-label={`${a} 的 tier`}
                            className="w-full"
                            value={r.tier}
                            onChange={(e) => setRow(a, { tier: e.target.value })}
                          >
                            {TIERS.map((tier) => (
                              <option key={tier} value={tier}>
                                {TIER_LABELS[tier] ?? tier}
                              </option>
                            ))}
                          </Select>
                        </td>
                      )}
                      <td className="px-2 py-2 text-xs">
                        {r.vision ? (
                          <span className="rounded bg-sky-500/15 px-1.5 py-0.5 text-sky-700">
                            {t("原生")}
                          </span>
                        ) : (
                          <span className="rounded bg-amber-500/15 px-1.5 py-0.5 text-amber-700">
                            {t("需辅助")}
                          </span>
                        )}
                      </td>
                      {matches && <MatchCell m={matches.get(a)} />}
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </>
      )}

      <div className="mt-8 card p-4">
        <div className="flex items-center gap-2 font-medium">
          <Eye className="h-4 w-4 text-accent" /> {t("视觉辅助 MCP")}
        </div>
        <p className="mt-1 text-sm text-muted">
          {t("给文本模型装上「眼睛」：本客户端自带一个 MCP 服务器，把图片交给一个多模态模型 描述，再把文字交给当前模型。已支持多模态的模型不需要。四个工具：")}
          <code>describe_image</code>{t("（看图）、")}<code>read_image_text</code>{t("（逐字抄下图上的文字， 报错截图用它）、")}<code>compare_images</code>{t("（比对改动前后）、")}
          <code>describe_screen</code>{t("（直接截当前屏幕，仅 macOS）。")}
        </p>

        <label className="mt-3 flex flex-wrap items-center gap-2 text-sm">
          <span className="text-muted">{t("用哪个模型看图")}</span>
          <Select
            className="w-64"
            value={visionModel}
            onChange={(e) => setVisionPick(e.target.value)}
          >
            <option value="">{t("选择多模态模型")}</option>
            {visionOptions.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </Select>
          {/* 已装的模型必须能被看见、而且要看得出是「已经在用的」而不是刚选的。
              没有这一句，用户改完模型不点安装就走，界面会显示新值、磁盘上却是
              旧值 —— 又变成一次「以为保存了」。 */}
          {installedModels.length > 0 && (
            <span className="text-xs text-muted">
              {t("已装：")}{installedModels.join("、")}
              {visionPick !== null && visionPick !== installedModels[0] && (
                <span className="ml-1 text-amber-700">{t("（改动尚未写入，点下面的安装才生效）")}</span>
              )}
            </span>
          )}
        </label>
        {installedModels.length > 1 && (
          <p className="mt-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-1.5 text-xs text-amber-900">
            {t("这几个 CLI 用的看图模型不一致（")}{installedModels.join("、")}{t("）。上面的下拉只显示 其中一个；要统一就选好模型后对每个 CLI 重新点一次「安装」。")}
          </p>
        )}

        {/* 一行一个 CLI，左边是它现在装没装（读回真实配置，不是按钮的记忆），
            右边只有一个按钮 —— 装了就显示「移除」，没装才显示「安装」。之前
            两个按钮并排且状态未知，点哪个全靠猜。
            最左侧的复选框用于批量：五家都要装时不必点五次。 */}
        <ul className="mt-3 divide-y divide-border/60 rounded-xl border border-border">
          {VISION_TARGETS.map((tg) => {
            const st = visionByTarget.get(tg);
            const on = st?.installed === true;
            return (
              <li key={tg} className="flex items-center gap-3 px-3 py-2">
                <input
                  type="checkbox"
                  aria-label={`选中 ${TARGET_LABELS[tg]}`}
                  checked={visionPicked.includes(tg)}
                  onChange={() =>
                    setVisionPicked((p) =>
                      p.includes(tg) ? p.filter((x) => x !== tg) : [...p, tg],
                    )
                  }
                  className="h-3.5 w-3.5"
                />
                <span
                  className={cn(
                    "h-1.5 w-1.5 shrink-0 rounded-full",
                    !on ? "bg-border" : st?.stale ? "bg-amber-500" : "bg-emerald-500",
                  )}
                />
                <span className="text-sm">{TARGET_LABELS[tg]}</span>
                <span className="text-xs text-muted">
                  {visionState.isPending
                    ? t("读取中…")
                    : on
                      ? `已安装${st?.model ? ` · ${st.model}` : ""}`
                      : t("未安装")}
                </span>
                {/* 装了但凭证过期，和「没装」是两回事：配置看着是好的，每次
                    看图却都 401。不点破的话用户只会看到工具莫名其妙不工作。 */}
                {on && st?.stale && (
                  <span
                    className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-700"
                    title={t("里面存的内核地址或令牌已经不是当前这个内核的了，重新安装即可修好")}
                  >
                    {t("凭证过期")}
                  </span>
                )}
                <button
                  onClick={() => (on ? visionOff.mutate([tg]) : vision.mutate([tg]))}
                  disabled={
                    vision.isPending || visionOff.isPending || (!on && !visionModel)
                  }
                  title={!on && !visionModel ? t("先在上面选一个多模态模型") : undefined}
                  className={cn(
                    "ml-auto rounded-lg border px-2.5 py-1 text-xs disabled:opacity-40",
                    on
                      ? "border-border text-red-600 hover:bg-surface-2"
                      : "border-border bg-surface-raised hover:bg-surface-2",
                  )}
                >
                  {on ? t("移除") : t("安装")}
                </button>
                {/* 已装的也要能改模型/修凭证，否则只能先移除再装一遍。 */}
                {on && (
                  <button
                    onClick={() => vision.mutate([tg])}
                    disabled={vision.isPending || visionOff.isPending || !visionModel}
                    title={
                      !visionModel
                        ? t("先在上面选一个多模态模型")
                        : t("用当前选中的模型和内核凭证重写这一条")
                    }
                    className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
                  >
                    {t("重写")}
                  </button>
                )}
              </li>
            );
          })}
          {visionPicked.length > 0 && (
            <li className="flex items-center gap-2 bg-surface-2/60 px-3 py-2 text-xs">
              <span className="text-muted">{t("已选")} {visionPicked.length} {t("个")}</span>
              <button
                onClick={() => setVisionPicked([])}
                className="text-muted underline-offset-2 hover:underline"
              >
                {t("取消选择")}
              </button>
              <button
                onClick={() => vision.mutate(visionPicked)}
                disabled={vision.isPending || !visionModel}
                title={!visionModel ? t("先在上面选一个多模态模型") : undefined}
                className="ml-auto rounded-lg bg-accent px-2.5 py-1 font-medium text-white hover:bg-accent/90 disabled:opacity-40"
              >
                {t("批量安装")}
              </button>
              <button
                onClick={() => visionOff.mutate(visionPicked)}
                disabled={visionOff.isPending}
                className="rounded-lg border border-border px-2.5 py-1 text-red-600 hover:bg-surface-2 disabled:opacity-40"
              >
                {t("批量移除")}
              </button>
            </li>
          )}
        </ul>
      </div>

      {message && <p className="mt-4 text-sm text-accent">{message}</p>}
      {apply.isError && <p className="mt-4 text-sm text-red-600">{errText(apply.error)}</p>}
    </div>
  );
}

function countBy(
  matches: Map<string, { level: MatchLevel }>,
  level: MatchLevel,
): number {
  let n = 0;
  for (const m of matches.values()) if (m.level === level) n++;
  return n;
}

/// 三态：精确命中、模糊命中（把命中的上游 ID 一并显示，用户要能自己判断这次
/// 模糊是不是对的）、上游没有。
function MatchCell({ m }: { m?: { level: MatchLevel; upstreamId: string | null } }) {
  const t = useT();
  if (!m) return <td className="px-2 py-2" />;
  if (m.level === "exact") {
    return (
      <td className="px-2 py-2">
        <span className="rounded bg-emerald-500/15 px-1.5 py-0.5 text-[10px] text-emerald-700">
          {t("精确")}
        </span>
      </td>
    );
  }
  if (m.level === "fuzzy") {
    return (
      <td className="px-2 py-2">
        <span
          className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-700"
          title={`上游对应：${m.upstreamId}`}
        >
          {t("模糊")}
        </span>
      </td>
    );
  }
  return (
    <td className="px-2 py-2">
      <span className="rounded bg-surface-2 px-1.5 py-0.5 text-[10px] text-muted">
        {t("上游无")}
      </span>
    </td>
  );
}
