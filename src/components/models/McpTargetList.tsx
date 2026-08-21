import { useT } from "../../i18n";
import { cn } from "../../lib/cn";
import { TARGET_LABELS } from "../../lib/targets";
import type { CliTarget } from "../../types";

/// 一个 CLI 上某个自带 MCP 的状态。视觉和生图两边的后端返回结构不完全一样
/// （生图多一个 `api`），所以在这里收敛成面板真正要显示的那几项。
export type McpTargetRow = {
  installed: boolean;
  /** 已装的话用的哪个模型。 */
  model: string | null;
  /** 装了但凭证过期。 */
  stale: boolean;
  /** 型号之外还要显示的一点信息，比如生图走的是 chat 还是 images。 */
  note?: string | null;
};

/// 「哪几家装了、装的什么、装 / 卸 / 重写」这一整块。
///
/// 抽出来是因为视觉和生图两个面板的这一段一模一样，而它承载的都是踩过的坑：
/// 状态读回磁盘而不是记按钮、一行只出现一个动作按钮、已装的能就地重写、凭证
/// 过期要单独标出来。复制一份的结果必然是只在其中一份上修 bug。
export function McpTargetList({
  targets,
  rows,
  loading,
  picked,
  onPicked,
  ready,
  notReadyHint,
  busy,
  onInstall,
  onRemove,
}: {
  targets: CliTarget[];
  rows: Map<CliTarget, McpTargetRow>;
  loading: boolean;
  picked: CliTarget[];
  onPicked: (next: CliTarget[]) => void;
  /** 配置齐不齐（主要是「选没选模型」）。不齐就禁掉所有安装动作。 */
  ready: boolean;
  /** 不齐时按钮的 title，告诉用户还差什么。 */
  notReadyHint: string;
  busy: boolean;
  onInstall: (ts: CliTarget[]) => void;
  onRemove: (ts: CliTarget[]) => void;
}) {
  const t = useT();
  return (
    <ul className="mt-3 divide-y divide-border/60 rounded-xl border border-border">
      {targets.map((tg) => {
        const st = rows.get(tg);
        const on = st?.installed === true;
        return (
          <li key={tg} className="flex items-center gap-3 px-3 py-2">
            <input
              type="checkbox"
              aria-label={t("选中 {name}", { name: TARGET_LABELS[tg] })}
              checked={picked.includes(tg)}
              onChange={() =>
                onPicked(
                  picked.includes(tg) ? picked.filter((x) => x !== tg) : [...picked, tg],
                )
              }
              className="h-3.5 w-3.5"
            />
            <span
              className={cn(
                "h-1.5 w-1.5 shrink-0 rounded-full",
                !on ? "bg-border" : st?.stale ? "bg-amber-500" : "bg-emerald-500",
              )}
            />
            <span className="text-sm">{TARGET_LABELS[tg]}</span>
            <span className="text-xs text-muted">
              {loading
                ? t("读取中…")
                : on
                  ? `已安装${st?.model ? ` · ${st.model}` : ""}${st?.note ? ` · ${st.note}` : ""}`
                  : t("未安装")}
            </span>
            {/* 装了但凭证过期，和「没装」是两回事：配置看着是好的，每次调用
                却都 401。不点破的话用户只会看到工具莫名其妙不工作。 */}
            {on && st?.stale && (
              <span
                className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-700"
                title={t("里面存的内核地址或令牌已经不是当前这个内核的了，重新安装即可修好")}
              >
                {t("凭证过期")}
              </span>
            )}
            <button
              onClick={() => (on ? onRemove([tg]) : onInstall([tg]))}
              disabled={busy || (!on && !ready)}
              title={!on && !ready ? notReadyHint : undefined}
              className={cn(
                "ml-auto rounded-lg border px-2.5 py-1 text-xs disabled:opacity-40",
                on
                  ? "border-border text-red-600 hover:bg-surface-2"
                  : "border-border bg-surface-raised hover:bg-surface-2",
              )}
            >
              {on ? t("移除") : t("安装")}
            </button>
            {/* 已装的也要能改模型/修凭证，否则只能先移除再装一遍。 */}
            {on && (
              <button
                onClick={() => onInstall([tg])}
                disabled={busy || !ready}
                title={
                  !ready ? notReadyHint : t("用当前选中的模型和内核凭证重写这一条")
                }
                className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
              >
                {t("重写")}
              </button>
            )}
          </li>
        );
      })}
      {picked.length > 0 && (
        <li className="flex items-center gap-2 bg-surface-2/60 px-3 py-2 text-xs">
          <span className="text-muted">
            {t("已选")} {picked.length} {t("个")}
          </span>
          <button
            onClick={() => onPicked([])}
            className="text-muted underline-offset-2 hover:underline"
          >
            {t("取消选择")}
          </button>
          <button
            onClick={() => onInstall(picked)}
            disabled={busy || !ready}
            title={!ready ? notReadyHint : undefined}
            className="ml-auto rounded-lg bg-accent px-2.5 py-1 font-medium text-white hover:bg-accent/90 disabled:opacity-40"
          >
            {t("批量安装")}
          </button>
          <button
            onClick={() => onRemove(picked)}
            disabled={busy}
            className="rounded-lg border border-border px-2.5 py-1 text-red-600 hover:bg-surface-2 disabled:opacity-40"
          >
            {t("批量移除")}
          </button>
        </li>
      )}
    </ul>
  );
}
