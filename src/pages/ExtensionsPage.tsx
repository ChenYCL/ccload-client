import { useT } from "../i18n";
/// 扩展管理：MCP / Skill / Agent / Hook 在 5 个 CLI 之间的统一视图。
///
/// 页面按「一行一个扩展、右侧徽章标出它装在哪几家」组织，而不是「一个 CLI 一
/// 块、块里列它自己的扩展」——后者只是把五个配置文件并排放，看不出重合，也就
/// 没有统一管理可言。

import { useMemo, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { Plus, Search } from "lucide-react";
import { api } from "../lib/api";
import { errText } from "../lib/err";
import type { ExtensionItem, ExtensionKind } from "../types";
import { AsyncBlock, ErrorBlock } from "../components/StateBlock";
import { TextInput } from "../components/ui/Input";
import {
  KIND_LABELS,
  KIND_TABS,
  groupByExtension,
  matchesQuery,
  targetSupportFor,
} from "../components/extensions/model";
import { ExtensionRow } from "../components/extensions/ExtensionRow";
import { ConfirmDialog } from "../components/extensions/ConfirmDialog";
import { SpecModal, type SpecModalMode } from "../components/extensions/SpecModal";
import {
  useExtensionData,
  useInvalidateExtensions,
} from "../components/extensions/useExtensions";

export function ExtensionsPage() {
  const t = useT();
  const [kind, setKind] = useState<ExtensionKind>("mcp");
  const [query, setQuery] = useState("");
  const [modal, setModal] = useState<SpecModalMode | null>(null);
  const [pendingRemove, setPendingRemove] = useState<ExtensionItem | null>(null);

  const data = useExtensionData();
  const invalidate = useInvalidateExtensions();

  const supports = useMemo(() => targetSupportFor(data.support, kind), [data.support, kind]);
  const groups = useMemo(
    () => groupByExtension(data.items, kind).filter((g) => matchesQuery(g, query)),
    [data.items, kind, query],
  );

  const remove = useMutation({
    mutationFn: (item: ExtensionItem) => api.extensionRemove(item.target, item.kind, item.id),
    onSuccess: () => {
      invalidate();
      setPendingRemove(null);
    },
  });

  const total = useMemo(
    () => groupByExtension(data.items, kind).length,
    [data.items, kind],
  );

  return (
    <div>
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="t-display">{t("扩展管理")}</h1>
          <p className="mt-1 text-sm text-muted">
            {t("MCP / Skill / Agent / Hook 一处配置，推给装了它们的每一个 CLI —— 各家的原生 格式（JSON、TOML、markdown 目录）由后端转换，写入前自动快照。")}
          </p>
        </div>
        <button
          onClick={() => setModal({ type: "create" })}
          className="flex shrink-0 items-center gap-1.5 rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90"
        >
          <Plus className="h-4 w-4" />
          {t("新建")}
        </button>
      </div>

      <div className="mt-5 flex flex-wrap items-center gap-2">
        {KIND_TABS.map((tab) => (
          <button
            key={tab.id}
            onClick={() => setKind(tab.id)}
            aria-current={kind === tab.id ? "page" : undefined}
            title={tab.hint}
            className={
              kind === tab.id
                ? "rounded-lg bg-accent/10 px-3.5 py-1.5 text-sm font-medium text-accent"
                : "rounded-lg border border-border bg-surface-raised px-3.5 py-1.5 text-sm text-muted hover:bg-surface-2"
            }
          >
            {tab.label}
          </button>
        ))}
        <div className="relative ml-auto">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted" />
          <TextInput
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("搜索名称或描述")}
            className="w-56 pl-8"
          />
        </div>
      </div>

      <p className="mt-2 text-xs text-muted">
        {KIND_TABS.find((t) => t.id === kind)?.hint} ·{" "}
        {supports.filter((s) => s.supported).length}/{supports.length} {t("个 CLI 支持")}
        {KIND_LABELS[kind]}
        {supports.some((s) => !s.supported) &&
          ` · ${supports
            .filter((s) => !s.supported)
            .map((s) => s.label)
            .join("、")}不支持`}
      </p>

      {/* 某个 CLI 的配置读不出来（比如 config.toml 语法错）只影响它自己那份
          清单，其余照常展示 —— 一个坏文件不该把整页变成错误页。 */}
      {data.failures.length > 0 && (
        <div
          role="alert"
          className="mt-4 space-y-1 rounded-xl border border-amber-200 bg-amber-50 px-4 py-3 text-xs text-amber-800"
        >
          {data.failures.map((f) => (
            <div key={f.target} className="break-all">
              {t("读取")} {f.label} {t("的扩展清单失败，下面的列表里不含它：")}{errText(f.error)}
            </div>
          ))}
        </div>
      )}

      <div className="mt-4">
        {data.error ? (
          <ErrorBlock error={data.error} />
        ) : (
          <AsyncBlock
            isLoading={data.isLoading}
            error={null}
            isEmpty={groups.length === 0}
            emptyText={
              query
                ? `没有匹配「${query}」的${KIND_LABELS[kind]}`
                : `还没有任何${KIND_LABELS[kind]}`
            }
            emptyHint={
              query
                ? total > 0
                  ? `清空搜索可看到全部 ${total} 项`
                  : undefined
                : t("点右上角「新建」装一个，或先在某个 CLI 里装好再回来同步")
            }
            skeletonLines={5}
          >
            <ul className="space-y-2">
              {groups.map((g) => (
                <ExtensionRow
                  key={g.id}
                  group={g}
                  supports={supports}
                  onEdit={(target, id) => setModal({ type: "edit", target, id })}
                  onRemove={setPendingRemove}
                />
              ))}
            </ul>
          </AsyncBlock>
        )}
      </div>

      {modal && (
        <SpecModal
          kind={kind}
          mode={modal}
          supports={supports}
          onClose={() => setModal(null)}
        />
      )}

      {pendingRemove && (
        <ConfirmDialog
          title={`删除 ${KIND_LABELS[pendingRemove.kind]}？`}
          body={
            <>
              {t("将从")}{" "}
              <span className="font-medium text-content">
                {supports.find((s) => s.target === pendingRemove.target)?.label ??
                  pendingRemove.target}
              </span>{" "}
              {t("移除")} <span className="font-mono text-content">{pendingRemove.id}</span>{t("。 这会改动你正在用的配置：")}
              <span className="mt-1 block break-all font-mono text-[11px]">
                {pendingRemove.source}
              </span>
              {t("装在其他 CLI 上的同名扩展不受影响。")}
            </>
          }
          confirmText={t("删除")}
          pending={remove.isPending}
          error={remove.isError ? remove.error : null}
          onConfirm={() => remove.mutate(pendingRemove)}
          onCancel={() => {
            remove.reset();
            setPendingRemove(null);
          }}
        />
      )}
    </div>
  );
}
