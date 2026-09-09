import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { Eye, Image as ImageIcon, RefreshCw } from "lucide-react";
import { useT } from "../i18n";
import { api } from "../lib/api";
import { fetchCatalog } from "../lib/modelCatalog";
import { ALL_TARGETS, TARGET_LABELS } from "../lib/targets";
import { Select, TextInput } from "../components/ui/Input";
import { McpTargetList, type McpTargetRow } from "../components/models/McpTargetList";
import { BridgeTable } from "../components/models/BridgeTable";
import type { CliTarget, ImageApi, RefreshMode } from "../types";
import { errText } from "../lib/err";
import { splitPinned } from "../lib/pins";

/// 模型桥接 —— 「CLI 里能选到什么，它到了内核会变成什么」。
///
/// # 两层，这一页管上面那层
///
/// ```text
///   CLI 配置里的名字 ──代理改写──► 内核别名 ──渠道 models[]──► 上游真实模型
///   （这一页）                      （模型路由页）
/// ```
///
/// 客户端本来就是代理（CLI 全指向 `cli_proxy`），所以**出口那一侧的名字由我们
/// 定**：写进 CLI 的可以是 `ccload-fast`，转发前换成内核认的 `grok-4.6`。这样
/// 内核那边换渠道、改别名都不用回头改五家 CLI 的配置。
///
/// 表里一行管三件事，都是逐行的：
///   * 写给谁 —— Claude Code 只有 5 个槽位，OpenCode 装得下全部；一张表推给所有
///     CLI 的老做法必然产生「勾了 83 个、Claude Code 写了 0 个」；
///   * 落到哪 —— 出口名 → 内核别名；
///   * 多大窗口、几成压缩 —— 跟着**落点**算：grok-4.6 是 500k、claude-opus-5 是
///     1M，同样 90% 落在两个分母上是 450k 和 900k。一份全局百分比表达不了。
///
/// 两个自带 MCP（视觉、生图）留在这一页下半截：它们也是「给 CLI 装能力」，
/// 5 家都能装，走的是通用 MCP 写入器。
const VISION_TARGETS: CliTarget[] = ALL_TARGETS;
const IMAGE_TARGETS: CliTarget[] = ALL_TARGETS;

type ChannelModel = { model?: string };
type Channel = {
  id?: number;
  name?: string;
  enabled?: boolean;
  models?: ChannelModel[];
};

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

  // 只看**启用中**的渠道。停用的渠道内核根本不会选它，把它的模型当成落点候选
  // 等于给用户一堆点了就报错的选项。
  const liveChannels = useMemo(
    () => (channels.data?.data ?? []).filter((c) => c.enabled !== false),
    [channels.data],
  );

  const aliases = useMemo(() => {
    const set = new Set<string>();
    for (const ch of liveChannels) {
      for (const m of ch.models ?? []) {
        // 钉住写进内核的私有别名（grok-4.6@ch21）是代理内部的名字，不是落点候选。
        if (m.model && !splitPinned(m.model)) set.add(m.model);
      }
    }
    return [...set].sort();
  }, [liveChannels]);

  const [message, setMessage] = useState<string | null>(null);

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
  const [visionPicked, setVisionPicked] = useState<CliTarget[]>([]);

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

  // 下拉里必须包含**当前已装的那个模型**，哪怕它已经不在渠道清单里了。受控
  // <select> 的 value 找不到对应 option 时浏览器渲染成空白 —— 那正是「明明装着
  // 模型，界面却显示占位符」的另一种成因。这里不按「是不是多模态」筛：第三方
  // 目录那一项经常缺，猜错会把用户真正能用的模型藏掉。
  const visionOptions = useMemo(
    () => [...new Set([...aliases, ...installedModels])].sort(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [aliases.join("\u0000"), installedModels.join("\u0000")],
  );

  return (
    <div>
      <header>
        <h1 className="t-display">{t("模型桥接")}</h1>
        <p className="mt-1 max-w-4xl text-sm text-muted">
          {t("CLI 里能选到哪些模型名，以及那个名字到了内核会变成什么。客户端本来就是代理，所以出口那一侧的名字由我们定：写进 CLI 的可以是 ccload-fast，转发前换成内核认的 grok-4.6 —— 内核那边换渠道、改别名，五家 CLI 的配置都不用动。每一行自己带着「写给哪几家 CLI」「多大窗口」「几成压缩」，因为这三件事本来就逐行不同。")}
        </p>
      </header>

      {catalog.isError && (
        <p className="mt-3 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs">
          {t("models.dev 拉取失败，上下文窗口暂用本地预设值（claude 20 万、gemini 100 万等），联网后重试。")}
        </p>
      )}

      <div className="mt-5">
        <BridgeTable
          aliases={aliases}
          catalog={catalog.data ?? null}
          onMessage={setMessage}
        />
      </div>

      {/* 上游改了模型清单之后，渠道里存的还是旧的那份 —— 内核不会自己发现
          「某个模型消失了」。这一块就是把它同步回来，也就让上面的落点候选
          收敛成上游真实存在的那些。 */}
      <RefreshPanel channels={liveChannels} onDone={setMessage} />

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

      {message && <p className="mt-4 whitespace-pre-wrap text-sm text-accent">{message}</p>}
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
    mutationFn: async () => {
      const env = await api.channelsRefreshModels(ids, mode);
      // 刷完必须把钉住的私有别名补回去。
      //
      // 私有别名（claude-opus-5@ch15）住在渠道的 models[] 里，而覆盖档就是拿上游
      // 返回的清单**整体替换**那张表 —— 上游当然不会返回我们编的 @ch15。没了之后
      // 钉住不报错，只是每条请求先挨一个 503（代理发私有别名 → 内核说没人服务它
      // → 用原名重发才成功）。日志里就是一对对的「503 首选 / 200」，0ms、$0，
      // 纯浪费一个往返，而且把日志刷满红色。
      const pins = await api.pinResync().catch((e) => [errText(e)]);
      return { env, pins };
    },
    onSuccess: ({ env, pins }) => {
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
      onDone([...lines, ...pins].join("\n"));
      // 渠道的模型变了，别名表、ComboBox 候选都要跟着重取。
      qc.invalidateQueries({ queryKey: ["channels"] });
      qc.invalidateQueries({ queryKey: ["pins"] });
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
