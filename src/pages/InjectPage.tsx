import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { AlertTriangle, Boxes, Check, Eye, FileText, Image as ImageIcon } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import { useT } from "../i18n";
import { ALL_TARGETS, TARGET_LABELS } from "../lib/targets";
import { TextArea, TextInput } from "../components/ui/Input";
import type { CliTarget, InjectSpec, InjectState, ToolNote } from "../types";

/// 系统注入。
///
/// 装了 `ccload-vision` 不等于模型会用它 —— 它只看得见工具名和一句描述，遇到
/// 用户贴图时会不会想起来调，全看运气；本来就不支持多模态的模型甚至不知道
/// 自己「看不见」。写进系统提示的一条规则比工具描述强得多。
///
/// 每个 CLI 都有一个启动时无条件读进系统提示的全局 markdown 文件，路径见后端
/// `system_inject::instructions_path`。这一页做的就是往那些文件里写一段带标记
/// 的块 —— **块外一个字节都不动**，因为 `~/.claude/CLAUDE.md` 里往往是用户攒了
/// 几个月的个人规则，抹掉是不可逆的。

/// Grok 明写每个规则文件截断到 10000 字符，别家没写上限。按最严的提醒，
/// 免得用户在 Grok 上被静默截断还不知道。
const SOFT_MAX_CHARS = 10_000;

/// 两份 spec 是不是同一件事。用来判断「改了还没写」，所以比的是**写出来会不会
/// 一样**，不是对象长得一样：空说明的扩展根本不写进去，顺序也不影响结果。
function sameSpec(a: InjectSpec, b: InjectSpec): boolean {
  if (a.vision !== b.vision || a.image !== b.image) return false;
  if (a.custom.trim() !== b.custom.trim()) return false;
  const key = (ts: ToolNote[]) =>
    ts
      .filter((x) => x.note.trim())
      .map((x) => `${x.name}\0${x.note.trim()}`)
      .sort()
      .join("\n");
  return key(a.tools) === key(b.tools);
}

/// 说明写了、服务器没装。
///
/// 这两段说明的全部作用是「让模型想起来调那个工具」，工具不存在时它一样会想起来 ——
/// 然后找不到，再自己编一条路（手写 SVG 冒充生成的图就是这么来的），比不写还坏。
/// 装在「模型导入」页而说明勾在这一页，两件事隔着一个页面，光看这里根本发现不了。
function MissingMcp({ server, panel }: { server: string; panel: string }) {
  const t = useT();
  return (
    <span className="mt-2 flex items-start gap-1.5 rounded-lg border border-amber-500/40 bg-amber-500/10 px-2.5 py-1.5 text-xs">
      <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-600" />
      <span>
        {t("{server} 还没装到任何 CLI —— 只写说明不装服务器，等于教模型去调一个不存在的工具。先到「模型导入」页最下面的「{panel}」装一下。", {
          server,
          panel,
        })}
      </span>
    </span>
  );
}

export function InjectPage() {
  const t = useT();
  const [message, setMessage] = useState<string | null>(null);

  const states = useQuery({ queryKey: ["inject-state"], queryFn: api.injectState });
  const byTarget = useMemo(() => {
    const m = new Map<CliTarget, InjectState>();
    for (const s of states.data ?? []) m.set(s.target, s);
    return m;
  }, [states.data]);

  // 这两段说明教的是**自带的两个 MCP 服务器**怎么用。服务器一家都没装的时候
  // 把说明写进去，等于教模型调一个不存在的工具 —— 它会照着找，找不到，然后
  // 自己编一条路（手写 SVG 凑数就是这么来的）。所以得当场说清楚。
  const visionMcp = useQuery({ queryKey: ["vision-mcp-state"], queryFn: api.visionMcpState });
  const imageMcp = useQuery({ queryKey: ["image-mcp-state"], queryFn: api.imageMcpState });
  const visionMissing = !!visionMcp.data && !visionMcp.data.some((s) => s.installed);
  const imageMissing = !!imageMcp.data && !imageMcp.data.some((s) => s.installed);

  // 初值全空。真正的默认在下面那个 seeding effect 里定 —— 它要等 MCP 的安装
  // 状态回来才知道该勾哪个。在这里写死 `vision: true` 的后果是：一家都没装的
  // 机器上进页面就是勾着的，旁边黄条同时说着「还没装到任何 CLI」，按下写入就
  // 真写了一段教模型调不存在工具的说明。
  const [spec, setSpec] = useState<InjectSpec>({
    vision: false,
    image: false,
    tools: [],
    custom: "",
  });
  const [picked, setPicked] = useState<CliTarget[]>([]);

  // 已注入的块回显成界面状态。**解析在后端做**（`system_inject::parse_block`）：
  // 前端曾经的做法是「把某一段单独渲染一遍，再看块里包不包含这段文字」，那个
  // 判断在我们自己改一个字的那天就失效了 —— 用户机器上的块是上个版本写的，
  // 逐字对不上，于是勾选框显示成没勾，整段旧文字被当成用户手写内容，再按一次
  // 「更新」就写出一段旧的加一段新的。
  //
  // 没有任何已注入的块时（第一次来），按**服务器装没装**定初值：勾一段没装的
  // 服务器等于教模型去调一个不存在的工具，这个默认必须跟着真实状态走，不能拍脑袋。
  //
  // 只在第一次读到时填，之后不再覆盖，否则用户正在编辑时一次后台 refetch 就会
  // 把输入框冲掉。
  const [seeded, setSeeded] = useState(false);
  useEffect(() => {
    // 三份数据都要到齐：MCP 状态还没回来就先种，会把「装了的」误判成没装，
    // 而 seeded 是一次性的，判错了就一直错到用户手动去勾。
    if (seeded || !states.data || !visionMcp.data || !imageMcp.data) return;
    const existing = states.data.find((s) => s.injected && s.spec);
    if (existing?.spec) {
      setSpec(existing.spec);
    } else {
      setSpec((prev) => ({
        ...prev,
        vision: visionMcp.data.some((s) => s.installed),
        image: imageMcp.data.some((s) => s.installed),
      }));
    }
    setSeeded(true);
  }, [states.data, visionMcp.data, imageMcp.data, seeded]);

  // 五家 CLI 已装的扩展，按 id 去重并记下装在哪几家。
  //
  // 这一块的价值不是「再列一遍扩展管理」，而是：用户为 Claude Code 手写过的
  // 用法说明（「codegraph 要在 grep 之前用」这类）往往只存在于
  // ~/.claude/CLAUDE.md，另外四家一个字都看不到。在这里写一次，五家一起推。
  const installed = useQuery({
    queryKey: ["inject-installed"],
    queryFn: async () => {
      const per = await Promise.all(
        ALL_TARGETS.map(async (tg) => {
          try {
            const items = await Promise.all([
              api.extensionsList(tg, "mcp"),
              api.extensionsList(tg, "skill"),
            ]);
            return items.flat().map((i) => ({ id: i.id, kind: i.kind, target: tg }));
          } catch {
            // 某家配置读不动只影响它自己那份清单，其余照常列。
            return [];
          }
        }),
      );
      const byId = new Map<string, { id: string; kind: string; targets: CliTarget[] }>();
      for (const it of per.flat()) {
        // 自家的两个 MCP 不进这张表：上面已经各有一段专门的说明，列两次只会
        // 让人以为要写两遍说明。
        if (it.id === "ccload-vision" || it.id === "ccload-image") continue;
        const cur = byId.get(it.id) ?? { id: it.id, kind: it.kind, targets: [] };
        if (!cur.targets.includes(it.target)) cur.targets.push(it.target);
        byId.set(it.id, cur);
      }
      return [...byId.values()].sort((a, b) => a.id.localeCompare(b.id));
    },
  });

  const noteOf = (id: string) => spec.tools.find((t) => t.name === id)?.note ?? "";
  const setNote = (id: string, note: string) => {
    const rest = spec.tools.filter((t) => t.name !== id);
    setSpec({ ...spec, tools: note ? [...rest, { name: id, note }] : rest });
  };

  const preview = useQuery({
    // key 必须覆盖 spec 的**每一个**字段。漏掉 tools 的后果是：改工具说明时
    // 预览纹丝不动，用户以为没写进去 —— 而写入用的是 spec 本身，真按下「写入」
    // 又确实生效了，于是预览和实际不一致，比没有预览更坏。
    queryKey: [
      "inject-preview",
      spec.vision,
      spec.image,
      spec.custom,
      JSON.stringify(spec.tools),
    ],
    queryFn: () => api.injectPreview(spec),
  });

  const apply = useMutation({
    mutationFn: (targets: CliTarget[]) => api.injectApply(targets, spec),
    onSuccess: async (rs) => {
      setMessage(
        rs
          .map((r) =>
            r.ok
              ? `${TARGET_LABELS[r.target]}：${r.path ?? t("已写入")}`
              : `${TARGET_LABELS[r.target]}：${t("失败")} —— ${r.error}`,
          )
          .join("\n"),
      );
      await states.refetch();
    },
    onError: (e) => setMessage(errText(e)),
  });

  const remove = useMutation({
    mutationFn: (targets: CliTarget[]) =>
      api.injectApply(targets, { vision: false, image: false, tools: [], custom: "" }),
    onSuccess: async (rs) => {
      setMessage(
        rs
          .map((r) =>
            r.ok
              ? `${TARGET_LABELS[r.target]}：${t("已移除")}`
              : `${TARGET_LABELS[r.target]}：${t("失败")} —— ${r.error}`,
          )
          .join("\n"),
      );
      await states.refetch();
    },
    onError: (e) => setMessage(errText(e)),
  });

  const blockChars = preview.data?.length ?? 0;
  const empty = !spec.vision && !spec.image && !spec.custom.trim();
  const busy = apply.isPending || remove.isPending;

  // 上面勾的东西和某一家 CLI 文件里已有的不一样 —— 也就是「改了但还没写」。
  //
  // 这一页最容易被误解的地方就在这里：两个勾选框长得像开关，实际是在编辑「要写
  // 什么」，落盘要按下面每家那一行的「写入 / 更新」。装了几十个扩展的机器上那行
  // 按钮在两屏之外，勾完看不见任何反馈，下次回来又按文件里的内容回显 —— 从用户
  // 那边看就是「勾了没保存」。所以这里必须主动说一句，并且把动作端到眼前。
  const dirty = useMemo(
    () =>
      (states.data ?? [])
        .filter((s) => s.injected && s.spec && !sameSpec(s.spec, spec))
        .map((s) => s.target),
    [states.data, spec],
  );

  return (
    <div>
      <h1 className="t-display">{t("系统注入")}</h1>
      <p className="mt-1 text-sm text-muted">
        {t(
          "把一段受管的说明写进每个 CLI 的全局指令文件（CLAUDE.md / AGENTS.md / GEMINI.md），启动时无条件进系统提示。只替换我们自己那对标记之间的内容，块外一个字节都不动 —— 你原有的规则不会被覆盖。",
        )}
      </p>

      {/* 视觉那一段是这个功能的由来，单独成块讲清楚为什么需要它。 */}
      <div className="mt-5 card p-4">
        <label className="flex items-start gap-2.5">
          <input
            type="checkbox"
            checked={spec.vision}
            onChange={(e) => setSpec({ ...spec, vision: e.target.checked })}
            className="mt-0.5 h-4 w-4 shrink-0"
          />
          <span>
            <span className="flex items-center gap-1.5 text-sm font-medium">
              <Eye className="h-4 w-4 text-accent" />
              {t("告诉 CLI 怎么用视觉辅助 MCP")}
            </span>
            <span className="mt-1 block text-xs leading-relaxed text-muted">
              {t(
                "装上 ccload-vision 不等于模型会用它 —— 它只看得见工具名和一句描述，遇到图片会不会想起来调全看运气，而文本模型甚至不知道自己「看不见」。这段会明确告诉它：你看不见图片，遇到 [Image 1] 这种没有路径的占位符时必须调工具并把 image 设成对应编号，不要让用户把图另存一份。",
              )}
            </span>
            {visionMissing && (
              <MissingMcp server="ccload-vision" panel={t("视觉辅助 MCP")} />
            )}
          </span>
        </label>
      </div>

      {/* 生图是同一个问题的另一面：模型不会主动想到「这张图我自己就能画」。 */}
      <div className="mt-3 card p-4">
        <label className="flex items-start gap-2.5">
          <input
            type="checkbox"
            checked={spec.image}
            onChange={(e) => setSpec({ ...spec, image: e.target.checked })}
            className="mt-0.5 h-4 w-4 shrink-0"
          />
          <span>
            <span className="flex items-center gap-1.5 text-sm font-medium">
              <ImageIcon className="h-4 w-4 text-accent" />
              {t("告诉 CLI 怎么用生图 MCP")}
            </span>
            <span className="mt-1 block text-xs leading-relaxed text-muted">
              {t(
                "模型不会主动想到「这张图我自己就能画」，默认反应是让你去找别的工具或者拿 SVG 凑数。这段会告诉它：图标、精灵图、贴图、UI 草图都可以用 generate_image 直接生成，改图用 edit_image（原图不动），以及结果回来的是磁盘路径而不是图本身 —— 要看画成什么样得接着调 describe_image。",
              )}
            </span>
            {imageMissing && <MissingMcp server="ccload-image" panel={t("生图 MCP")} />}
          </span>
        </label>
      </div>

      {/* 勾选不等于写入。这句话必须挨着勾选框，不能只写在页面底部 —— 会看到底部
          的人本来就不会误会。 */}
      {dirty.length > 0 && (
        <div className="mt-3 flex flex-wrap items-center gap-2 rounded-xl border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs">
          <span>
            {t("上面的改动还没写进文件。已注入的 {n} 家里存的还是旧内容。", {
              n: dirty.length,
            })}
          </span>
          <button
            onClick={() => apply.mutate(dirty)}
            disabled={busy || empty}
            title={dirty.map((x) => TARGET_LABELS[x]).join(t("、"))}
            className="ml-auto rounded-lg bg-accent px-2.5 py-1 font-medium text-white hover:bg-accent/90 disabled:opacity-40"
          >
            {t("更新这 {n} 家", { n: dirty.length })}
          </button>
        </div>
      )}

      {(installed.data?.length ?? 0) > 0 && (
        // 默认折叠：这张表在装了几十个 skill 的机器上有两屏高，展开着会把下面
        // 真正的写入按钮顶出视野 —— 用户勾完两个框看不见任何动作，就以为勾选
        // 本身就是保存。写用法说明是可选的进阶动作，藏一层正合适。
        <details className="mt-3 card p-4">
          <summary className="flex cursor-pointer items-center gap-1.5 text-sm font-medium">
            <Boxes className="h-4 w-4 text-accent" />
            {t("已装的扩展")}
            <span className="text-xs font-normal text-muted">
              {t("{n} 个，可选填用法说明", { n: installed.data?.length ?? 0 })}
            </span>
            {/* 已经写了几条说明得在折叠状态下也看得见，否则用户不知道里面有内容。 */}
            {spec.tools.filter((x) => x.note.trim()).length > 0 && (
              <span className="rounded bg-accent/15 px-1.5 py-0.5 text-[10px] text-accent">
                {t("已写 {n} 条", { n: spec.tools.filter((x) => x.note.trim()).length })}
              </span>
            )}
          </summary>
          <p className="mt-1 text-xs leading-relaxed text-muted">
            {t(
              "MCP 的工具描述只说「它是干什么的」，不说「什么时候该想起它」。给一条你自己的判断标准，五家 CLI 一起生效 —— 不填的不会写进去。",
            )}
          </p>
          <ul className="mt-2 divide-y divide-border/60 rounded-xl border border-border">
            {installed.data?.map((it) => (
              <li key={it.id} className="flex items-center gap-2 px-3 py-2">
                <span className="w-40 shrink-0 truncate font-mono text-[11px]" title={it.id}>
                  {it.id}
                </span>
                <span className="w-12 shrink-0 text-[10px] text-muted">{it.kind}</span>
                {/* 装在哪几家：不同 CLI 装的扩展并不一致，写的说明却是五家共用的，
                    不标出来会让人以为某条说明只对某一家生效。 */}
                <span
                  className="w-16 shrink-0 text-[10px] text-muted"
                  title={it.targets.map((x) => TARGET_LABELS[x]).join(t("、"))}
                >
                  {t("{n}/5 家", { n: it.targets.length })}
                </span>
                <TextInput
                  small
                  value={noteOf(it.id)}
                  onChange={(e) => setNote(it.id, e.target.value)}
                  placeholder={t("什么时候用它，例如：改代码前先查调用链，比 grep 准")}
                  className="flex-1"
                />
              </li>
            ))}
          </ul>
        </details>
      )}

      <div className="mt-3 card p-4">
        <div className="flex items-center gap-1.5 text-sm font-medium">
          <FileText className="h-4 w-4 text-accent" />
          {t("你自己的规则")}
        </div>
        <p className="mt-1 text-xs text-muted">
          {t("原样写进块里，五个 CLI 共用同一份。留空则只写上面勾选的内容。")}
        </p>
        <TextArea
          value={spec.custom}
          onChange={(e) => setSpec({ ...spec, custom: e.target.value })}
          rows={6}
          placeholder={t("例如：永远用中文回答；提交前必须跑一遍测试。")}
          className="mt-2 font-mono text-xs"
        />
      </div>

      {/* 预览由后端渲染：视觉那段的工具名必须和 MCP 真正暴露的一致，
          前端再抄一份必然会漂 —— 漂了就是教模型调一个不存在的工具。 */}
      {!empty && preview.data && (
        <details className="mt-3 card p-4">
          <summary className="cursor-pointer text-sm font-medium">
            {t("预览将写入的内容")}
            <span className="ml-2 text-xs font-normal text-muted">
              {t("{n} 字符", { n: blockChars })}
            </span>
          </summary>
          <pre className="mt-2 max-h-80 overflow-auto whitespace-pre-wrap rounded-lg bg-surface-2 p-3 font-mono text-[11px] leading-relaxed">
            {preview.data}
          </pre>
        </details>
      )}

      {/* 表头。没有它的时候这张表只是五行东西，看不出「写入」才是这一页的动作。 */}
      <h2 className="mt-5 text-sm font-medium">
        {t("写到哪几家")}
        <span className="ml-2 text-xs font-normal text-muted">
          {t("上面勾的内容按下这一行的「写入 / 更新」才落到文件里")}
        </span>
      </h2>

      <ul className="mt-2 divide-y divide-border/60 rounded-xl border border-border">
        {ALL_TARGETS.map((tg) => {
          const st = byTarget.get(tg);
          // Grok 会把超长的规则文件截断，而截断是静默的 —— 注入后可能超限的
          // 那家必须提前说，否则用户只会发现规则「有时不生效」。
          const willOverflow =
            st != null && !empty && st.chars - (st.block?.length ?? 0) + blockChars > SOFT_MAX_CHARS;
          return (
            <li key={tg} className="flex items-center gap-3 px-3 py-2">
              <input
                type="checkbox"
                aria-label={`${t("选中")} ${TARGET_LABELS[tg]}`}
                checked={picked.includes(tg)}
                onChange={() =>
                  setPicked((p) => (p.includes(tg) ? p.filter((x) => x !== tg) : [...p, tg]))
                }
                className="h-3.5 w-3.5"
              />
              <span
                className={cn(
                  "h-1.5 w-1.5 shrink-0 rounded-full",
                  st?.injected ? "bg-emerald-500" : "bg-border",
                )}
              />
              <span className="w-28 shrink-0 text-sm">{TARGET_LABELS[tg]}</span>
              <span
                className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted"
                title={st?.path}
              >
                {st?.path ?? "…"}
              </span>
              <span className="shrink-0 text-xs text-muted">
                {states.isPending
                  ? t("读取中…")
                  : st?.injected
                    ? t("已注入")
                    : st?.exists
                      ? t("未注入")
                      : t("文件不存在")}
              </span>
              {/* 装着的是上个版本的措辞。不标出来的话用户没有任何线索知道该按
                  「更新」—— 界面显示「已注入」，而模型读到的还是旧说明。 */}
              {st?.outdated && (
                <span
                  className="shrink-0 rounded bg-sky-500/15 px-1.5 py-0.5 text-[10px] text-sky-700"
                  title={t(
                    "这段是旧版本写进去的，内容仍然生效。按「更新」会用当前措辞重写它 —— 先展开上面的预览看一眼要写什么。",
                  )}
                >
                  {t("旧版")}
                </span>
              )}
              {willOverflow && (
                <span
                  className="shrink-0 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-700"
                  title={t("Grok Build 会把规则文件截断到 10000 字符，超出的部分静默丢失")}
                >
                  {t("可能超长")}
                </span>
              )}
              <button
                onClick={() => (st?.injected ? remove.mutate([tg]) : apply.mutate([tg]))}
                disabled={busy || (!st?.injected && empty)}
                title={!st?.injected && empty ? t("先勾选或填写要注入的内容") : undefined}
                className={cn(
                  "shrink-0 rounded-lg border px-2.5 py-1 text-xs disabled:opacity-40",
                  st?.injected
                    ? "border-border text-red-600 hover:bg-surface-2"
                    : "border-border bg-surface-raised hover:bg-surface-2",
                )}
              >
                {st?.injected ? t("移除") : t("写入")}
              </button>
              {st?.injected && (
                <button
                  onClick={() => apply.mutate([tg])}
                  disabled={busy || empty}
                  title={t("用上面的内容重写这一段")}
                  className="shrink-0 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
                >
                  {t("更新")}
                </button>
              )}
            </li>
          );
        })}
        {picked.length > 0 && (
          <li className="flex items-center gap-2 bg-surface-2/60 px-3 py-2 text-xs">
            <span className="text-muted">{t("已选 {n} 个", { n: picked.length })}</span>
            <button
              onClick={() => setPicked([])}
              className="text-muted underline-offset-2 hover:underline"
            >
              {t("取消选择")}
            </button>
            <button
              onClick={() => apply.mutate(picked)}
              disabled={busy || empty}
              className="ml-auto flex items-center gap-1 rounded-lg bg-accent px-2.5 py-1 font-medium text-white hover:bg-accent/90 disabled:opacity-40"
            >
              <Check className="h-3.5 w-3.5" />
              {t("批量写入")}
            </button>
            <button
              onClick={() => remove.mutate(picked)}
              disabled={busy}
              className="rounded-lg border border-border px-2.5 py-1 text-red-600 hover:bg-surface-2 disabled:opacity-40"
            >
              {t("批量移除")}
            </button>
          </li>
        )}
      </ul>

      <p className="mt-3 text-[11px] text-muted/80">
        {t(
          "写入前会自动快照原文件，在「CLI 接管」页的备份列表里可以一键还原。注入的内容每次请求都会占 token，不需要的项就别勾。",
        )}
      </p>

      {message && <p className="mt-4 whitespace-pre-line text-sm text-accent">{message}</p>}
    </div>
  );
}
