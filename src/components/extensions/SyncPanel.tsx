/// 同步面板：一处配置推到多个 CLI。这是整个扩展管理的主功能，所以它是行展开
/// 后的第一块内容，不是藏在二级菜单里的动作。
///
/// 后端逐目标独立成败，回来的是一个 `SyncOutcome[]`——绝不能折叠成一句「同步
/// 成功/失败」，五行结果就老老实实画五行。

import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { AlertCircle, Check, Minus } from "lucide-react";
import { api } from "../../lib/api";
import { errText } from "../../lib/err";
import type { CliTarget, SyncOutcome } from "../../types";
import { KIND_LABELS, type ExtensionGroup, type TargetSupport } from "./model";
import { WrittenFiles } from "./fields";
import { Select } from "../ui/Input";
import { useInvalidateExtensions } from "./useExtensions";

export function SyncPanel({
  group,
  supports,
}: {
  group: ExtensionGroup;
  supports: TargetSupport[];
}) {
  const invalidate = useInvalidateExtensions();
  // 默认勾上「支持但还没装」的那几家——同步最常见的意图就是补齐缺口，这样
  // 主操作只要一次点击；已装的要覆盖则需用户自己勾，免得手滑改掉在用的配置。
  const [picked, setPicked] = useState<CliTarget[]>(() =>
    supports.filter((s) => s.supported && !group.targets.includes(s.target)).map((s) => s.target),
  );
  const [source, setSource] = useState<CliTarget | "auto">("auto");

  const sync = useMutation({
    mutationFn: (v: { targets: CliTarget[]; source?: CliTarget }) =>
      api.extensionSync(group.kind, group.id, v.targets, v.source),
    onSuccess: (outcomes) => {
      invalidate();
      // 写成功的目标现在已经装上了，把勾选收敛到「还没补上的」，结果区就成了
      // 一份剩余工作清单而不是一份陈旧的选择。
      const done = outcomes.filter((o) => o.ok).map((o) => o.target);
      setPicked((prev) => prev.filter((t) => !done.includes(t)));
    },
  });

  const toggle = (t: CliTarget) =>
    setPicked((prev) => (prev.includes(t) ? prev.filter((x) => x !== t) : [...prev, t]));

  return (
    <div className="rounded-xl border border-border bg-surface p-3.5">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <span className="text-xs font-medium">同步到其他 CLI</span>
        <label className="flex items-center gap-1.5 text-[11px] text-muted">
          来源
          <Select
            small
            value={source}
            onChange={(e) => setSource(e.target.value as CliTarget | "auto")}
          >
            <option value="auto">自动（第一个装了它的）</option>
            {group.items.map((i) => (
              <option key={i.target} value={i.target}>
                {supports.find((s) => s.target === i.target)?.label ?? i.target}
              </option>
            ))}
          </Select>
        </label>
      </div>

      <div className="mt-2.5 grid gap-1.5 sm:grid-cols-2">
        {supports.map((s) => (
          <TargetChoice
            key={s.target}
            support={s}
            kindLabel={KIND_LABELS[group.kind]}
            installed={group.targets.includes(s.target)}
            checked={picked.includes(s.target)}
            onToggle={() => toggle(s.target)}
          />
        ))}
      </div>

      <div className="mt-3 flex items-center justify-between gap-3">
        <span className="text-[11px] text-muted">
          写入前自动快照；不支持的目标已置灰
        </span>
        <button
          disabled={picked.length === 0 || sync.isPending}
          onClick={() =>
            sync.mutate({ targets: picked, source: source === "auto" ? undefined : source })
          }
          className="shrink-0 rounded-lg bg-accent px-3.5 py-1.5 text-xs font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-40"
        >
          {sync.isPending ? "同步中…" : `同步到 ${picked.length} 个 CLI`}
        </button>
      </div>

      {/* 整个调用失败（比如没有任何 CLI 装了它，定不出来源）——这时一行结果都
          没有，与逐目标失败是两回事。 */}
      {sync.isError && (
        <p className="mt-2.5 break-all rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
          {errText(sync.error)}
        </p>
      )}
      {sync.data && (
        <ul className="mt-2.5 space-y-1">
          {sync.data.map((o) => (
            <OutcomeRow key={o.target} outcome={o} />
          ))}
        </ul>
      )}
    </div>
  );
}

function TargetChoice({
  support,
  kindLabel,
  installed,
  checked,
  onToggle,
}: {
  support: TargetSupport;
  kindLabel: string;
  installed: boolean;
  checked: boolean;
  onToggle: () => void;
}) {
  if (!support.supported) {
    return (
      <div
        className="flex items-start gap-2 rounded-lg border border-dashed border-border px-2.5 py-1.5 text-xs text-muted/60"
        title={`${support.label} 没有 ${kindLabel} 的配置位置`}
      >
        <Minus className="mt-0.5 h-3 w-3 shrink-0" />
        <span>
          <span className="line-through">{support.label}</span>
          <span className="ml-1.5 text-[10px]">不支持{kindLabel}</span>
        </span>
      </div>
    );
  }
  return (
    <label
      className={`flex cursor-pointer items-start gap-2 rounded-lg border px-2.5 py-1.5 text-xs ${
        checked ? "border-accent/50 bg-accent/10" : "border-border hover:bg-surface-2"
      }`}
    >
      <input
        type="checkbox"
        checked={checked}
        onChange={onToggle}
        className="mt-0.5 h-3.5 w-3.5 shrink-0"
      />
      <span className="min-w-0">
        <span className="font-medium">{support.label}</span>
        <span className={`ml-1.5 text-[10px] ${installed ? "text-amber-700" : "text-muted"}`}>
          {installed ? "已装 · 会覆盖" : "未装"}
        </span>
        {support.path && (
          <span className="block truncate font-mono text-[10px] text-muted">~/{support.path}</span>
        )}
      </span>
    </label>
  );
}

/// 三态：成功、跳过（后端对「目标就是来源」返回 ok=true 且带一句说明）、失败。
function OutcomeRow({ outcome }: { outcome: SyncOutcome }) {
  const skipped = outcome.ok && outcome.error !== null;
  const cls = skipped
    ? "bg-surface-2 text-muted"
    : outcome.ok
      ? "bg-emerald-50 text-emerald-700"
      : "bg-red-50 text-red-700";
  const Icon = skipped ? Minus : outcome.ok ? Check : AlertCircle;

  return (
    <li className={`flex items-start gap-2 rounded-lg px-2.5 py-1.5 text-xs ${cls}`}>
      <Icon className="mt-0.5 h-3.5 w-3.5 shrink-0" />
      <div className="min-w-0 flex-1">
        <span className="font-medium">{outcome.label}</span>
        {outcome.error && <span className="ml-1.5 break-all">{outcome.error}</span>}
        {outcome.ok && !outcome.error && (
          <span className="ml-1.5">已写入 {outcome.written.length} 个文件</span>
        )}
        <WrittenFiles files={outcome.written} />
      </div>
    </li>
  );
}
