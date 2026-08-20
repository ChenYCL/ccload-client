import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Check, Eye, FileText } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import { useT } from "../i18n";
import { ALL_TARGETS, TARGET_LABELS } from "../lib/targets";
import { TextArea } from "../components/ui/Input";
import type { CliTarget, InjectSpec, InjectState } from "../types";

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

export function InjectPage() {
  const t = useT();
  const [message, setMessage] = useState<string | null>(null);

  const states = useQuery({ queryKey: ["inject-state"], queryFn: api.injectState });
  const byTarget = useMemo(() => {
    const m = new Map<CliTarget, InjectState>();
    for (const s of states.data ?? []) m.set(s.target, s);
    return m;
  }, [states.data]);

  const [spec, setSpec] = useState<InjectSpec>({ vision: true, custom: "" });
  const [picked, setPicked] = useState<CliTarget[]>([]);

  // 视觉那段的权威文本。用它把已注入的块拆回「生成的」和「用户写的」两半 ——
  // 前端自己按标题猜边界会在后端改一个字时就错位，而错位的后果是把生成内容
  // 当成用户输入再写一遍，块越滚越长。
  const visionText = useQuery({
    queryKey: ["inject-preview", "vision-only"],
    queryFn: () => api.injectPreview({ vision: true, custom: "" }),
    staleTime: Infinity,
  });

  // 已注入的块回显成用户自己那段。只在第一次读齐时填，之后不再覆盖 ——
  // 否则用户正在编辑时一次后台 refetch 就会把输入框冲掉。
  const [seeded, setSeeded] = useState(false);
  useEffect(() => {
    if (seeded || !states.data || visionText.data === undefined) return;
    const existing = states.data.find((s) => s.injected && s.block);
    if (existing?.block) {
      const hasVision = existing.block.includes(visionText.data);
      setSpec({
        vision: hasVision,
        custom: (hasVision
          ? existing.block.replace(visionText.data, "")
          : existing.block
        ).trim(),
      });
    }
    setSeeded(true);
  }, [states.data, visionText.data, seeded]);

  const preview = useQuery({
    queryKey: ["inject-preview", spec.vision, spec.custom],
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
      api.injectApply(targets, { vision: false, custom: "" }),
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
  const empty = !spec.vision && !spec.custom.trim();
  const busy = apply.isPending || remove.isPending;

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
                "装上 ccload-vision 不等于模型会用它 —— 它只看得见工具名和一句描述，遇到图片会不会想起来调全看运气，而文本模型甚至不知道自己「看不见」。这段会明确告诉它：你看不见图片，遇到图片必须调这四个工具，并说清各自的分工（看图 / 逐字抄 / 比对 / 截屏）。",
              )}
            </span>
          </span>
        </label>
      </div>

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

      <ul className="mt-4 divide-y divide-border/60 rounded-xl border border-border">
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
