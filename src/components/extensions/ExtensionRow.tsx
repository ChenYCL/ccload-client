/// 一行一个扩展，右侧 5 个徽章标出它装在哪几个 CLI 上——「同一个 id 装在多处」
/// 一眼就能看出来，这正是统一管理相对于逐个 CLI 翻配置文件的意义。
///
/// 展开后先给同步（主功能），再给逐 CLI 的编辑 / 删除。

import { useState } from "react";
import { ChevronDown, ChevronRight, Pencil, Trash2 } from "lucide-react";
import type { CliTarget, ExtensionItem } from "../../types";
import { ALL_TARGETS, TARGET_SHORT, type ExtensionGroup, type TargetSupport } from "./model";
import { SyncPanel } from "./SyncPanel";

export function ExtensionRow(props: {
  group: ExtensionGroup;
  supports: TargetSupport[];
  onEdit: (target: CliTarget, id: string) => void;
  onRemove: (item: ExtensionItem) => void;
}) {
  const [open, setOpen] = useState(false);
  const { group, supports } = props;
  const supportedCount = supports.filter((s) => s.supported).length;

  return (
    <li className="card overflow-hidden">
      <button
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        className="flex w-full items-center gap-3 px-4 py-3 text-left hover:bg-surface-2"
      >
        {open ? (
          <ChevronDown className="h-4 w-4 shrink-0 text-muted" />
        ) : (
          <ChevronRight className="h-4 w-4 shrink-0 text-muted" />
        )}
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="truncate text-sm font-medium">{group.label}</span>
            {group.label !== group.id && (
              <span className="shrink-0 truncate font-mono text-[10px] text-muted">{group.id}</span>
            )}
          </div>
          {group.description && (
            <div className="mt-0.5 truncate text-xs text-muted">{group.description}</div>
          )}
        </div>
        <div className="hidden shrink-0 sm:block">
          <TargetBadges installed={group.targets} supports={supports} />
        </div>
        <span className="shrink-0 text-[11px] tabular-nums text-muted">
          {group.targets.length}/{supportedCount}
        </span>
      </button>

      {open && (
        <div className="space-y-3 border-t border-border px-4 py-3.5">
          <SyncPanel group={group} supports={supports} />
          <div>
            <div className="mb-1.5 text-xs font-medium">已装位置</div>
            <ul className="space-y-1.5">
              {group.items.map((item) => (
                <InstallRow
                  key={item.target}
                  item={item}
                  label={supports.find((s) => s.target === item.target)?.label ?? item.target}
                  onEdit={() => props.onEdit(item.target, item.id)}
                  onRemove={() => props.onRemove(item)}
                />
              ))}
            </ul>
          </div>
        </div>
      )}
    </li>
  );
}

/// 三种状态各有各的画法：装了 = 实心，支持但没装 = 空心虚线，不支持 = 划掉。
/// 只用颜色区分在「更高对比度」偏好下会糊成一片，所以形状也一起变。
function TargetBadges({
  installed,
  supports,
}: {
  installed: CliTarget[];
  supports: TargetSupport[];
}) {
  return (
    <div className="flex gap-1">
      {ALL_TARGETS.map((t) => {
        const s = supports.find((x) => x.target === t);
        const has = installed.includes(t);
        const cls = has
          ? "bg-accent/10 text-accent border-accent/25"
          : s?.supported
            ? "border-dashed border-border text-muted/70"
            : "border-transparent bg-surface-2 text-muted/40 line-through";
        const title = has
          ? `已装在 ${s?.label ?? t}`
          : s?.supported
            ? `${s.label} 支持但未安装`
            : `${s?.label ?? t} 不支持这类扩展`;
        return (
          <span
            key={t}
            title={title}
            className={`rounded-md border px-1.5 py-0.5 text-[10px] font-medium ${cls}`}
          >
            {TARGET_SHORT[t]}
          </span>
        );
      })}
    </div>
  );
}

function InstallRow({
  item,
  label,
  onEdit,
  onRemove,
}: {
  item: ExtensionItem;
  label: string;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const [rawOpen, setRawOpen] = useState(false);
  return (
    <li className="rounded-xl border border-border bg-surface-raised px-3 py-2">
      <div className="flex items-center gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-xs font-medium">{label}</span>
            {!item.enabled && (
              <span className="rounded-full bg-surface-2 px-1.5 py-0.5 text-[10px] text-muted">
                已停用
              </span>
            )}
          </div>
          <div className="mt-0.5 break-all font-mono text-[10px] text-muted">{item.source}</div>
        </div>
        <button
          onClick={onEdit}
          className="flex shrink-0 items-center gap-1 rounded-lg border border-border px-2 py-1 text-[11px] hover:bg-surface-2"
        >
          <Pencil className="h-3 w-3" />
          编辑
        </button>
        <button
          onClick={onRemove}
          className="flex shrink-0 items-center gap-1 rounded-lg border border-border px-2 py-1 text-[11px] text-red-600 hover:bg-surface-2"
        >
          <Trash2 className="h-3 w-3" />
          删除
        </button>
      </div>
      {/* detail 是这条目在配置文件里的原始片段。手工排过版的展示总会漏字段，
          原样打出来反而是最可靠的「它到底写了什么」。 */}
      <button
        onClick={() => setRawOpen(!rawOpen)}
        className="mt-1 text-[10px] text-muted hover:text-content"
      >
        {rawOpen ? "收起原始配置" : "查看原始配置"}
      </button>
      {rawOpen && (
        <pre className="mt-1 max-h-48 overflow-auto rounded-lg bg-surface-2 p-2 font-mono text-[10px] leading-relaxed">
          {JSON.stringify(item.detail, null, 2)}
        </pre>
      )}
    </li>
  );
}
