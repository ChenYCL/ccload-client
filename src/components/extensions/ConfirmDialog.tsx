import { useT } from "../../i18n";
/// 二次确认。删除会改用户正在用的真实配置文件，所以不做「点了就没」的按钮。

import type { ReactNode } from "react";
import { AlertTriangle } from "lucide-react";
import { errText } from "../../lib/err";
import { Overlay } from "../Modal";

export function ConfirmDialog(props: {
  title: string;
  body: ReactNode;
  confirmText: string;
  pending: boolean;
  error: unknown;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const t = useT();
  return (
    // 走 Overlay/portal：就地渲染的 fixed 会被主区的 mask 裁掉，见 Modal.tsx。
    <Overlay onClose={props.onCancel} className="animate-scrim flex items-center justify-center bg-black/40 p-6">
      <div
        role="alertdialog"
        aria-modal="true"
        className="animate-materialize material-modal w-full max-w-md rounded-2xl border border-border p-5"
      >
        <div className="flex items-start gap-3">
          <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-amber-600" />
          <div className="min-w-0">
            <h2 className="t-title">{props.title}</h2>
            <div className="mt-1.5 text-sm text-muted">{props.body}</div>
          </div>
        </div>
        {props.error != null && (
          <p className="mt-3 break-all rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
            {errText(props.error)}
          </p>
        )}
        <div className="mt-4 flex justify-end gap-2">
          <button
            onClick={props.onCancel}
            className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2"
          >
            取消
          </button>
          <button
            disabled={props.pending}
            onClick={props.onConfirm}
            className="rounded-lg bg-red-600 px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-red-700 disabled:opacity-40"
          >
            {props.pending ? t("处理中…") : props.confirmText}
          </button>
        </div>
      </div>
    </Overlay>
  );
}
