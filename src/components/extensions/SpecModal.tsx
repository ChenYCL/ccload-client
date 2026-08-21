import { useT } from "../../i18n";
/// 安装 / 编辑的模态。install 是**单目标**命令，所以这里只往一个 CLI 写；
/// 铺开到其他 CLI 是列表行里「同步」的活，两件事分开才说得清各自的成败。

import { useEffect, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { errText } from "../../lib/err";
import { ErrorBlock, LoadingBlock } from "../StateBlock";
import type { CliTarget, ExtensionKind } from "../../types";
import { KIND_LABELS, type TargetSupport } from "./model";
import { SpecForm } from "./SpecForm";
import { EMPTY_DRAFT, draftFromSpec, draftProblem, draftToSpec, type SpecDraft } from "./spec";
import { WrittenFiles } from "./fields";
import { useExtensionSpec, useInvalidateExtensions } from "./useExtensions";
import { Overlay } from "../Modal";

export type SpecModalMode =
  | { type: "create" }
  | { type: "edit"; target: CliTarget; id: string };

export function SpecModal(props: {
  kind: ExtensionKind;
  mode: SpecModalMode;
  supports: TargetSupport[];
  onClose: () => void;
}) {
  const t = useT();
  const { kind, mode, supports } = props;
  const editing = mode.type === "edit" ? mode : null;
  const invalidate = useInvalidateExtensions();

  const spec = useExtensionSpec(editing?.target ?? null, kind, editing?.id ?? null);
  const [draft, setDraft] = useState<SpecDraft | null>(
    mode.type === "create" ? EMPTY_DRAFT : null,
  );
  useEffect(() => {
    if (spec.data) setDraft(draftFromSpec(spec.data));
  }, [spec.data]);

  // 只存「用户明确选过的目标」；默认值每次渲染现算。useState 的初值只在首次
  // 渲染求一次，而 supports 来自一个异步查询 —— 首帧它是空数组，初值就永远
  // 停在 null，写入按钮再也点不亮（查询失败或慢一点就必然发生）。
  const [picked, setPicked] = useState<CliTarget | null>(null);
  const target: CliTarget | null =
    picked ?? editing?.target ?? supports.find((s) => s.supported)?.target ?? null;
  const setTarget = setPicked;

  const install = useMutation({
    mutationFn: (v: { target: CliTarget; draft: SpecDraft }) =>
      api.extensionInstall(v.target, kind, draftToSpec(v.draft, kind)),
    onSuccess: invalidate,
  });

  const problem = draft ? draftProblem(draft, kind) : t("加载中");
  const chosen = supports.find((s) => s.target === target);
  const title = editing
    ? `编辑 ${KIND_LABELS[kind]} · ${editing.id}`
    : `新建 ${KIND_LABELS[kind]}`;

  return (
    <Overlay onClose={props.onClose} className="animate-scrim flex items-center justify-center bg-black/40 p-6">
      <div className="animate-materialize material-modal flex max-h-full w-full max-w-2xl flex-col rounded-2xl border border-border">
        <div className="flex items-center justify-between border-b border-border px-6 py-4">
          <h2 className="t-title">{title}</h2>
          <button
            onClick={props.onClose}
            className="rounded-md border border-border px-2 py-1 text-sm hover:bg-surface-2"
          >
            {t("关闭")}
          </button>
        </div>

        <div className="scroll-edge flex-1 overflow-auto px-6 py-5">
          {editing && spec.isLoading && <LoadingBlock lines={5} />}
          {editing && spec.error && <ErrorBlock error={spec.error} />}

          {draft && (
            <>
              {!editing && (
                <div className="mb-5">
                  <div className="text-xs font-medium text-content">{t("装到哪个 CLI")}</div>
                  <p className="mt-0.5 text-[11px] text-muted">
                    {t("先装一处，再用列表里的「同步」推给其他 CLI。")}
                  </p>
                  <div className="mt-1.5 flex flex-wrap gap-2">
                    {supports.map((s) => (
                      <button
                        key={s.target}
                        type="button"
                        disabled={!s.supported}
                        title={
                          s.supported
                            ? `写入 ~/${s.path}`
                            : `${s.label} 没有 ${KIND_LABELS[kind]} 的配置位置`
                        }
                        onClick={() => setTarget(s.target)}
                        className={
                          !s.supported
                            ? "cursor-not-allowed rounded-lg border border-dashed border-border px-3 py-1.5 text-xs text-muted/50 line-through"
                            : target === s.target
                              ? "rounded-lg bg-accent/10 px-3 py-1.5 text-xs font-medium text-accent"
                              : "rounded-lg border border-border px-3 py-1.5 text-xs text-muted hover:bg-surface-2"
                        }
                      >
                        {s.label}
                      </button>
                    ))}
                  </div>
                  {chosen?.path && (
                    <p className="mt-1.5 font-mono text-[10px] text-muted">~/{chosen.path}</p>
                  )}
                </div>
              )}

              <SpecForm
                kind={kind}
                draft={draft}
                onChange={setDraft}
                idLocked={editing !== null}
              />
            </>
          )}
        </div>

        <div className="border-t border-border px-6 py-4">
          {install.isSuccess && (
            <div
              role="status"
              className="mb-3 rounded-lg bg-emerald-50 px-3 py-2 text-xs text-emerald-700"
            >
              {t("✓ 已写入")} {install.data.length} {t("个文件")}
              <WrittenFiles files={install.data} />
            </div>
          )}
          {install.isError && (
            <p className="mb-3 break-all rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
              {errText(install.error)}
            </p>
          )}
          <div className="flex items-center justify-between gap-3">
            <span className="text-[11px] text-muted">
              {problem && draft ? problem : t("写入前会自动快照，可在 CLI 接管页回滚")}
            </span>
            <button
              disabled={!draft || !target || problem !== null || install.isPending}
              onClick={() => draft && target && install.mutate({ target, draft })}
              className="shrink-0 rounded-lg bg-accent px-4 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
            >
              {install.isPending ? t("写入中…") : editing ? t("保存") : t("安装")}
            </button>
          </div>
        </div>
      </div>
    </Overlay>
  );
}
