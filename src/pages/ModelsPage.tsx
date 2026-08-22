import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { Check, Download, Eye, Image as ImageIcon, RefreshCw, Radar, Wand2 } from "lucide-react";
import { useT } from "../i18n";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { fetchCatalog, lookupMeta } from "../lib/modelCatalog";
import { ALL_TARGETS, TARGET_LABELS } from "../lib/targets";
import { buildUpstreamIndex, matchAlias, type MatchLevel } from "../lib/modelMatch";
import { Select, TextInput } from "../components/ui/Input";
import { McpTargetList, type McpTargetRow } from "../components/models/McpTargetList";
import type { CliTarget, ImageApi, ImportEntry, RefreshMode } from "../types";
import { errText } from "../lib/err";

/// One-click model catalog import. The kernel can serve every alias that any
/// channel exposes (`models[].model`), but each CLI needs the alias written
/// into its own config before the user can select it. This page aggregates
/// the aliases, lets the user pin context windows, and writes the catalog:
///   * Claude Code → 只写用户显式指定的 tier 槽位（没有目录文件可写）
///   * Codex       → [profiles.<别名>]，顶层 model 不碰
///   * OpenCode    → 合并进 provider.ccload.models，顶层 model 只在缺失时补
/// Also installs the vision MCP (this binary's `vision-mcp` subcommand) so a
/// text-only model gets image descriptions from a multimodal one, and the image
/// MCP (`image-mcp`) so every CLI can generate and edit images.
//
// 模型目录只有这三家有可写的位置（Gemini / Grok 的配置里没有模型清单这一节）；
// 两个自带 MCP 则是 5 家都能装 —— 它们走的是通用 MCP 写入器。
const IMPORT_TARGETS: CliTarget[] = ["claude-code", "codex", "opencode"];
const VISION_TARGETS: CliTarget[] = ALL_TARGETS;
const IMAGE_TARGETS: CliTarget[] = ALL_TARGETS;

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
  // 面板只认 McpTargetRow 那几项 —— 视觉这边没有「走哪条路」这种额外信息。
  const visionRows = useMemo(() => {
    const m = new Map<CliTarget, McpTargetRow>();
    for (const s of visionState.data ?? []) {
      m.set(s.target, { installed: s.installed, model: s.model, stale: s.stale });
    }
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
            {t("从内核渠道聚合所有可用模型别名，")}<strong>{t("追加")}</strong>{t("进各 CLI 的模型目录： Codex 每个别名一个")} <code>{t("[profiles.别名]")}</code>（<code>codex --profile</code> {t("选用）、 OpenCode 合并进")} <code>provider.ccload.models</code>{t("。两者都不会动你当前正在用的模型。Claude Code 没有目录文件，只有 5 个槽位，所以要在 Tier 列显式指定 —— 没指定的行不写。")}
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
          {t("models.dev 拉取失败，上下文窗口暂用本地预设值（claude 20 万、gemini 100 万等），联网后点「同步」重试。")}
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
                      {probedFail.length} {t("个渠道没返回模型清单 —— 它们上的别名会被算成「上游无」，别据此删掉")}
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

          {/* 上游改了模型清单之后，渠道里存的还是旧的那份 —— 内核不会自己发现
              「某个模型消失了」。这一块就是把它同步回来。放在上游校验下面：
              校验告诉你「哪些别名上游没有」，这里负责把它们真的删掉。 */}
          <RefreshPanel channels={liveChannels} onDone={setMessage} />

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
          {t(
            "给文本模型装上「眼睛」：本客户端自带一个 MCP 服务器，把图片交给一个多模态模型描述，再把文字交给当前模型。已支持多模态的模型不需要。对话里只有 [Image 1] 没有路径时，把 image 设成 \"1\"，不要让用户把图另存一份。",
          )}
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
              {t("已装：")}{installedModels.join(t("、"))}
              {visionPick !== null && visionPick !== installedModels[0] && (
                <span className="ml-1 text-amber-700">{t("（改动尚未写入，点下面的安装才生效）")}</span>
              )}
            </span>
          )}
        </label>
        {installedModels.length > 1 && (
          <p className="mt-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-1.5 text-xs text-amber-900">
            {t("这几个 CLI 用的看图模型不一致（")}{installedModels.join(t("、"))}{t("）。上面的下拉只显示其中一个；要统一就选好模型后对每个 CLI 重新点一次「安装」。")}
          </p>
        )}

        {/* 一行一个 CLI，左边是它现在装没装（读回真实配置，不是按钮的记忆），
            右边只有一个按钮 —— 装了就显示「移除」，没装才显示「安装」。之前
            两个按钮并排且状态未知，点哪个全靠猜。
            最左侧的复选框用于批量：五家都要装时不必点五次。 */}
        <McpTargetList
          targets={VISION_TARGETS}
          rows={visionRows}
          loading={visionState.isPending}
          picked={visionPicked}
          onPicked={setVisionPicked}
          ready={!!visionModel}
          notReadyHint={t("先在上面选一个多模态模型")}
          busy={vision.isPending || visionOff.isPending}
          onInstall={(ts) => vision.mutate(ts)}
          onRemove={(ts) => visionOff.mutate(ts)}
        />
      </div>

      <ImagePanel aliases={aliases} onMessage={setMessage} />

      {message && <p className="mt-4 text-sm text-accent">{message}</p>}
      {apply.isError && <p className="mt-4 text-sm text-red-600">{errText(apply.error)}</p>}
    </div>
  );
}

/// 生图 MCP 的安装面板。
///
/// 和视觉那块并列而不是合并：它多两个只有生图才有的开关，而这两个开关都是
/// **能力**层面的，藏起来会直接让功能不可用 ——
///   * 走哪条路：默认 `auto`，按模型名挑端点、挑错了当场换另一条重试；钉死成
///     `chat`（能生成也能改图）或 `images`（只能生成）是给知道自己在干什么的人留的。
///   * 存到哪：生成的图落在磁盘，工具只把路径回给模型（回图本身等于每张图
///     往 transcript 里灌一兆 base64，正好是「会话救援」要清理的东西）。
///
/// 模型这里不做「能不能生图」的自动筛选：第三方目录里没有这个字段，猜错了会
/// 把用户真正能用的那个模型从下拉里藏掉。改成全列 + 一句说明。
function ImagePanel({
  aliases,
  onMessage,
}: {
  aliases: string[];
  onMessage: (m: string) => void;
}) {
  const t = useT();
  const [picked, setPicked] = useState<CliTarget[]>([]);

  const state = useQuery({
    queryKey: ["image-mcp-state"],
    queryFn: api.imageMcpState,
  });

  const rows = useMemo(() => {
    const m = new Map<CliTarget, McpTargetRow>();
    for (const s of state.data ?? []) {
      m.set(s.target, {
        installed: s.installed,
        model: s.model,
        stale: s.stale,
        note: s.api,
      });
    }
    return m;
  }, [state.data]);

  // 已装的用的是哪个模型 / 哪条路。和视觉同一个理由：下拉要显示磁盘上的真值，
  // 否则切走再回来就变回占位符，看着像「选了没保存上」。
  const installedModels = useMemo(
    () => [
      ...new Set(
        (state.data ?? []).filter((s) => s.installed && s.model).map((s) => s.model as string),
      ),
    ],
    [state.data],
  );
  const installedApi = (state.data ?? []).find((s) => s.installed)?.api ?? null;

  const [modelPick, setModelPick] = useState<string | null>(null);
  const model = modelPick ?? installedModels[0] ?? "";
  const [apiPick, setApiPick] = useState<ImageApi | null>(null);
  const imageApi: ImageApi = apiPick ?? installedApi ?? "auto";
  // 空 = 后端的默认目录（~/.ccload-client/images）。留空是绝大多数人的正确选择，
  // 所以 placeholder 直接把那个路径写出来，而不是写「可选」。
  //
  // 和模型、走哪条路一样要从磁盘回显：不回显的话，为了改别的项按一次安装，
  // 用户自己设的目录会被一个空字符串换成默认目录 —— 之后生成的图去了别处，
  // 而界面上什么都没说。
  const [dirPick, setDirPick] = useState<string | null>(null);
  const installedDir = (state.data ?? []).find((s) => s.installed)?.out_dir ?? "";
  const outDir = dirPick ?? installedDir;

  // 下拉必须包含**当前已装的那个模型**，哪怕它已经不在渠道清单里了 ——
  // 受控 select 的 value 找不到 option 时浏览器渲染成空白。
  const options = useMemo(
    () => [...new Set([...aliases, ...installedModels])].sort(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [aliases.join("\0"), installedModels.join("\0")],
  );

  // 必须串行：五路并行会同时改 backups/manifest.json，短写入叠在旧文件尾巴上。
  // 同 visionBatch 上面那段注释。
  const batch = async (targets: CliTarget[], enabled: boolean): Promise<TargetOutcome[]> => {
    const out: TargetOutcome[] = [];
    for (const tg of targets) {
      try {
        const written = enabled
          ? await api.imageMcpSet(tg, true, model, imageApi, outDir || undefined)
          : await api.imageMcpSet(tg, false);
        out.push({
          t: tg,
          status: "ok",
          text: written.join("、") || (enabled ? t("已安装") : t("已移除")),
        });
      } catch (e) {
        out.push({ t: tg, status: "failed", text: errText(e) });
      }
    }
    return out;
  };

  const install = useMutation({
    mutationFn: (ts: CliTarget[]) => batch(ts, true),
    onSuccess: async (rs) => {
      onMessage(summarize(rs, t("已安装"), t("安装失败")));
      // 先取回磁盘上的新值，**再**把选择权交还给它 —— 顺序反了中间那一帧会
      // 闪回占位符。和视觉面板同一个坑。
      await state.refetch();
      setModelPick(null);
      setApiPick(null);
      setDirPick(null);
    },
    onError: (e) => onMessage(errText(e)),
  });

  // 已装的那几家钉死在某一条路上。
  //
  // 钉死的值是写进 CLI 配置里的 `CCLOAD_IMAGE_API`，换一个新版客户端不会让它
  // 自己变 —— 而老版本把默认值写成了 chat，于是**所有**老用户都是「钉死 chat」，
  // 新版按模型选端点、选错了换一条的能力一个人也吃不到，还会继续撞上那句
  // 「这个模型不在这个端点上」。所以这里必须主动说，并且给一个直接改的按钮。
  const pinned = useMemo(
    () =>
      (state.data ?? [])
        .filter((s) => s.installed && s.api && s.api !== "auto")
        .map((s) => s.target),
    [state.data],
  );
  // 逐家按**它自己**存的模型和目录重写，只动「走哪条路」这一项：这几家装的
  // 模型未必一样，拿面板上选中的那个一把梭会顺手改掉别家的配置。
  const toAuto = useMutation({
    mutationFn: async (): Promise<TargetOutcome[]> => {
      const out: TargetOutcome[] = [];
      for (const s of state.data ?? []) {
        if (!s.installed || !s.api || s.api === "auto" || !s.model) continue;
        try {
          const written = await api.imageMcpSet(
            s.target,
            true,
            s.model,
            "auto",
            s.out_dir || undefined,
          );
          out.push({ t: s.target, status: "ok", text: written.join("、") || t("已安装") });
        } catch (e) {
          out.push({ t: s.target, status: "failed", text: errText(e) });
        }
      }
      return out;
    },
    onSuccess: async (rs) => {
      onMessage(summarize(rs, t("已改成自动"), t("改写失败")));
      await state.refetch();
      setApiPick(null);
    },
    onError: (e) => onMessage(errText(e)),
  });
  const remove = useMutation({
    mutationFn: (ts: CliTarget[]) => batch(ts, false),
    onSuccess: (rs) => {
      onMessage(summarize(rs, t("已移除"), t("移除失败")));
      state.refetch();
    },
    onError: (e) => onMessage(errText(e)),
  });

  return (
    <div className="mt-8 card p-4">
      <div className="flex items-center gap-2 font-medium">
        <ImageIcon className="h-4 w-4 text-accent" /> {t("生图 MCP")}
      </div>
      <p className="mt-1 text-sm text-muted">
        {t(
          "给每个 CLI 装上「手」：本客户端自带一个 MCP 服务器，把文字变成图，也能按指令改一张已有的图 —— 做游戏素材、图标、UI 草图都用它。生成的图写到磁盘，工具只把路径交回给模型；模型想看自己画的是什么，接着调视觉 MCP 的 describe_image 即可。",
        )}
      </p>

      <label className="mt-3 flex flex-wrap items-center gap-2 text-sm">
        <span className="text-muted">{t("用哪个模型生图")}</span>
        <Select className="w-64" value={model} onChange={(e) => setModelPick(e.target.value)}>
          <option value="">{t("选择生图模型")}</option>
          {options.map((a) => (
            <option key={a} value={a}>
              {a}
            </option>
          ))}
        </Select>
        {installedModels.length > 0 && (
          <span className="text-xs text-muted">
            {t("已装：")}
            {installedModels.join(t("、"))}
            {modelPick !== null && modelPick !== installedModels[0] && (
              <span className="ml-1 text-amber-700">
                {t("（改动尚未写入，点下面的安装才生效）")}
              </span>
            )}
          </span>
        )}
      </label>
      <p className="mt-1 text-xs text-muted/80">
        {t("这里不做自动筛选：第三方目录里没有「能不能生图」这一项，猜错会把你真正能用的那个模型藏掉。选一个渠道里确实能出图的别名。")}
      </p>

      <label className="mt-3 flex flex-wrap items-center gap-2 text-sm">
        <span className="text-muted">{t("走哪条路")}</span>
        <Select
          className="w-64"
          value={imageApi}
          onChange={(e) => setApiPick(e.target.value as ImageApi)}
        >
          <option value="auto">{t("自动（按模型挑，推荐）")}</option>
          <option value="chat">{t("对话生图（能生成也能改图）")}</option>
          <option value="images">{t("生图端点（只能生成）")}</option>
        </Select>
        <span className="text-xs text-muted">
          {imageApi === "auto"
            ? t("按模型名挑端点：grok-imagine / gpt-image / dall-e 走生图端点，其余先走对话；上游要是回「这个模型不在这个端点上」就当场换另一条重试。改图永远走对话。")
            : imageApi === "chat"
              ? t("/v1/chat/completions + modalities:[\"image\"]，尺寸按宽高比给（1:1@2k）。")
              : t("/v1/images/generations，尺寸按像素给（1024x1024）。这条路的请求体里没有放输入图的位置，所以 edit_image 用不了。")}
        </span>
      </label>

      {/* 老版本把默认值写成 chat，装过的机器上那个值还在配置文件里。 */}
      {pinned.length > 0 && (
        <div className="mt-2 flex flex-wrap items-center gap-2 rounded-xl border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs">
          <span>
            {t(
              "已装的 {n} 家钉死在一条端点上（配置里存的值，换新版客户端不会自己变）。改成「自动」后会按模型挑端点，上游说走错了就当场换一条重试。",
              { n: pinned.length },
            )}
          </span>
          <button
            onClick={() => toAuto.mutate()}
            disabled={toAuto.isPending || install.isPending || remove.isPending}
            title={pinned.map((x) => TARGET_LABELS[x]).join(t("、"))}
            className="ml-auto rounded-lg bg-accent px-2.5 py-1 font-medium text-white hover:bg-accent/90 disabled:opacity-40"
          >
            {t("这 {n} 家改成自动", { n: pinned.length })}
          </button>
        </div>
      )}

      <label className="mt-3 flex flex-wrap items-center gap-2 text-sm">
        <span className="text-muted">{t("图存到哪")}</span>
        <TextInput
          mono
          small
          className="w-96"
          placeholder="~/.ccload-client/images"
          value={outDir}
          onChange={(e) => setDirPick(e.target.value)}
        />
        <span className="text-xs text-muted">{t("留空就是默认目录。工具回给模型的是绝对路径。")}</span>
      </label>

      <McpTargetList
        targets={IMAGE_TARGETS}
        rows={rows}
        loading={state.isPending}
        picked={picked}
        onPicked={setPicked}
        ready={!!model}
        notReadyHint={t("先在上面选一个生图模型")}
        busy={install.isPending || remove.isPending}
        onInstall={(ts) => install.mutate(ts)}
        onRemove={(ts) => remove.mutate(ts)}
      />
    </div>
  );
}

function countBy(
  matches: Map<string, { level: MatchLevel }>,
  level: MatchLevel,
): number {  let n = 0;
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


/// 把渠道的模型清单同步成上游现在的样子。
///
/// 为什么单独做一块而不是复用「上游校验」：校验只是**读**，读完告诉你哪些别名
/// 上游已经没有了；但那些条目还留在渠道里，ComboBox 和 Tier 绑定照样会把它们
/// 当候选推给你，点了就失败。真正删掉要靠内核的
/// `POST /admin/channels/models/refresh-batch`，而它默认的 `merge` 只增不删 ——
/// 必须显式用 `replace`。这个默认值坑过人，所以两种模式的差别写在按钮旁边，
/// 不藏进 tooltip。
function RefreshPanel({
  channels,
  onDone,
}: {
  channels: Channel[];
  onDone: (msg: string) => void;
}) {
  const t = useT();
  const qc = useQueryClient();
  const [mode, setMode] = useState<RefreshMode>("replace");
  const ids = channels.map((c) => c.id).filter((id): id is number => id !== undefined);

  const run = useMutation({
    mutationFn: () => api.channelsRefreshModels(ids, mode),
    onSuccess: (env) => {
      const r = env.data;
      const lines = (r?.results ?? []).map((it) => {
        const name = it.channel_name || `#${it.channel_id}`;
        if (it.status === "failed") return `${name}：${t("失败")} —— ${it.error ?? ""}`;
        if (it.status === "unchanged") return `${name}：${t("没有变化")}（${it.total}）`;
        const delta =
          mode === "replace"
            ? t("删掉 {n} 个", { n: it.removed ?? 0 })
            : t("新增 {n} 个", { n: it.added ?? 0 });
        return `${name}：${delta}，${t("现在共 {n} 个", { n: it.total })}`;
      });
      onDone(lines.join("\n"));
      // 渠道的模型变了，别名表、ComboBox 候选都要跟着重取。
      qc.invalidateQueries({ queryKey: ["channels"] });
    },
    onError: (e) => onDone(errText(e)),
  });

  return (
    <div className="mt-3 flex flex-wrap items-center gap-2 rounded-xl border border-border bg-surface-2/40 px-3 py-2">
      <span className="flex items-center gap-1.5 text-xs text-muted">
        <RefreshCw className="h-3.5 w-3.5" /> {t("同步渠道模型清单")}
      </span>
      <Select
        small
        className="w-56"
        aria-label={t("同步方式")}
        value={mode}
        onChange={(e) => setMode(e.target.value as RefreshMode)}
      >
        <option value="replace">{t("覆盖：删掉上游已经没有的")}</option>
        <option value="merge">{t("增量：只加新的，不删")}</option>
      </Select>
      <button
        onClick={() => run.mutate()}
        disabled={run.isPending || ids.length === 0}
        className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
      >
        {run.isPending
          ? t("同步中…")
          : t("同步 {n} 个渠道", { n: ids.length })}
      </button>
      <span className="basis-full text-[11px] text-muted/80">
        {mode === "replace"
          ? t("上游改过模型清单（比如去掉了一批旧名字）之后用这个。内核默认的「增量」只增不删，退役的模型会一直留在候选里。")
          : t("只把上游新增的模型加进来，渠道里已有的一个都不动。")}
      </span>
    </div>
  );
}
