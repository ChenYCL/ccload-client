import type { ReactNode } from "react";
import { AlertCircle, Inbox } from "lucide-react";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";

/// 每个数据块的三种状态（加载 / 空 / 出错）都长一个样。散落在各页面里手写会
/// 出现「有的地方转圈、有的地方白屏」，这里统一成骨架 + 文案。

export function Panel(props: {
  title: string;
  /** 标题右侧的说明，通常是数据来源或口径 */
  hint?: string;
  right?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <section className={cn("card bg-surface-raised p-4", props.className)}>
      <header className="flex items-baseline justify-between gap-3">
        <div className="flex items-baseline gap-2">
          <h2 className="t-title">{props.title}</h2>
          {props.hint && <span className="text-[11px] text-muted">{props.hint}</span>}
        </div>
        {props.right}
      </header>
      <div className="mt-3">{props.children}</div>
    </section>
  );
}

/** 骨架屏：宽度按行递减，读起来像正在成形的内容而不是一堆等宽条。 */
export function LoadingBlock({ lines = 3 }: { lines?: number }) {
  return (
    <div className="space-y-2" role="status" aria-label="加载中">
      {Array.from({ length: lines }).map((_, i) => (
        <div
          key={i}
          className="h-3.5 animate-pulse rounded bg-surface-2"
          style={{ width: `${100 - i * 12}%` }}
        />
      ))}
    </div>
  );
}

export function EmptyBlock({ text, hint }: { text: string; hint?: string }) {
  return (
    <div className="flex flex-col items-center gap-1.5 py-8 text-center">
      <Inbox className="h-5 w-5 text-muted/60" />
      <p className="text-sm text-muted">{text}</p>
      {hint && <p className="text-xs text-muted/70">{hint}</p>}
    </div>
  );
}

/** 错误一律走 errText —— 直接 String(e) 会渲染成 [object Object]。 */
export function ErrorBlock({ error }: { error: unknown }) {
  return (
    <div
      role="alert"
      className="flex items-start gap-2 rounded-xl border border-red-200 bg-red-50 px-3 py-2.5 text-sm text-red-700"
    >
      <AlertCircle className="mt-0.5 h-4 w-4 shrink-0" />
      <span className="break-all">{errText(error)}</span>
    </div>
  );
}

/**
 * 一个数据块的状态机。有错先报错，加载中给骨架，空了给空态，其余渲染内容 ——
 * 顺序固定，页面里就不会漏掉某一种。
 */
export function AsyncBlock(props: {
  isLoading: boolean;
  error: unknown;
  isEmpty: boolean;
  emptyText: string;
  emptyHint?: string;
  skeletonLines?: number;
  children: ReactNode;
}) {
  if (props.error) return <ErrorBlock error={props.error} />;
  if (props.isLoading) return <LoadingBlock lines={props.skeletonLines} />;
  if (props.isEmpty) return <EmptyBlock text={props.emptyText} hint={props.emptyHint} />;
  return <>{props.children}</>;
}
