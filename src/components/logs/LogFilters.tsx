import { AlertOctagon, RotateCcw } from "lucide-react";
import { cn } from "../../lib/cn";
import { Select } from "../ui/Input";
import type { LogsBootstrap } from "../../types";

/// 筛选栏。下拉的取值全部来自 GET /admin/logs/bootstrap —— 内核直接给出该时间
/// 范围内真实出现过的模型 / 渠道 / 状态码，比前端从当前这页日志里凑准确得多
/// （凑出来的选项会随着翻页变化，选了还可能筛出空结果）。

export type LogFilterState = {
  /** 空串 = 不筛。model / channel_id / status_code 三项由内核 SQL 侧过滤 */
  model: string;
  channelId: string;
  statusCode: string;
  /** 只看错误。内核 LogFilter 没有这个维度，只能在客户端筛，见 LogsPage 注释 */
  errorsOnly: boolean;
};

export const EMPTY_FILTERS: LogFilterState = {
  model: "",
  channelId: "",
  statusCode: "",
  errorsOnly: false,
};

export function hasAnyFilter(f: LogFilterState): boolean {
  return f.model !== "" || f.channelId !== "" || f.statusCode !== "" || f.errorsOnly;
}

export function LogFilters({
  value,
  onChange,
  bootstrap,
}: {
  value: LogFilterState;
  onChange: (next: LogFilterState) => void;
  bootstrap?: LogsBootstrap;
}) {
  const set = <K extends keyof LogFilterState>(k: K, v: LogFilterState[K]) =>
    onChange({ ...value, [k]: v });

  return (
    <div className="flex flex-wrap items-center gap-2">
      <Select
        small
        aria-label="按模型筛选"
        value={value.model}
        onChange={(e) => set("model", e.target.value)}
      >
        <option value="">全部模型</option>
        {(bootstrap?.models ?? []).map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
      </Select>

      <Select
        small
        aria-label="按渠道筛选"
        value={value.channelId}
        onChange={(e) => set("channelId", e.target.value)}
      >
        <option value="">全部渠道</option>
        {(bootstrap?.channels ?? []).map((c) => (
          <option key={c.id} value={String(c.id)}>
            {c.name}
          </option>
        ))}
      </Select>

      <Select
        small
        aria-label="按状态码筛选"
        value={value.statusCode}
        onChange={(e) => set("statusCode", e.target.value)}
      >
        <option value="">全部状态码</option>
        {(bootstrap?.status_codes ?? []).map((s) => (
          <option key={s} value={String(s)}>
            {s}
          </option>
        ))}
      </Select>

      <button
        type="button"
        aria-pressed={value.errorsOnly}
        onClick={() => set("errorsOnly", !value.errorsOnly)}
        className={cn(
          "flex items-center gap-1.5 rounded-lg border px-2.5 py-1 text-xs font-medium",
          value.errorsOnly
            ? "border-red-300 bg-red-50 text-red-700"
            : "border-border bg-surface-raised text-muted hover:text-content",
        )}
      >
        <AlertOctagon className="h-3.5 w-3.5" />
        只看错误
      </button>

      {hasAnyFilter(value) && (
        <button
          type="button"
          onClick={() => onChange(EMPTY_FILTERS)}
          className="flex items-center gap-1 rounded-lg px-2 py-1 text-xs text-muted hover:text-content"
        >
          <RotateCcw className="h-3.5 w-3.5" />
          清除
        </button>
      )}
    </div>
  );
}
