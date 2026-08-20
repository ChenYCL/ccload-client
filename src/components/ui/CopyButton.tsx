import { useState } from "react";
import { cn } from "../../lib/cn";

/// 「复制」按钮。
///
/// 抽出来是因为同一段剪贴板 + 「已复制」回执逻辑本来在设置页的 CopyRow 里内联着，
/// 而管理密码、内核后台那两处也要用 —— 各抄一份就会各自漂移（回执时长、禁用条件、
/// 空值处理）。
export function CopyButton({
  value,
  className,
  label = "复制",
}: {
  value: string;
  className?: string;
  /** 图标位紧张的地方可以传更短的文案。 */
  label?: string;
}) {
  const [copied, setCopied] = useState(false);

  return (
    <button
      type="button"
      disabled={!value}
      onClick={() => {
        navigator.clipboard.writeText(value).then(() => {
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1200);
        });
      }}
      className={cn(
        "shrink-0 rounded-md border px-2 py-1 text-[11px]",
        copied
          ? "border-emerald-500/40 bg-emerald-50 text-emerald-700"
          : "border-border hover:bg-surface-2 disabled:opacity-40",
        className,
      )}
    >
      {copied ? "已复制" : label}
    </button>
  );
}
