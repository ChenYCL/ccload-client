import { useEffect } from "react";
import { createPortal } from "react-dom";

/// 所有模态的唯一出口。
///
/// 必须走 portal，不能就地渲染：`<main>` 上挂了 `scroll-edge` 的 `mask-image`，
/// 而带 mask / filter / transform / backdrop-filter 的元素会成为其**固定定位后代
/// 的包含块**。也就是说就地渲染时 `fixed inset-0` 是相对 `<main>` 而不是视口，
/// 弹窗会被主区裁掉一块（表现为被侧栏盖住），且加多少 z-index 都无效。
/// 挂到 body 之后它才真正相对视口，层级问题也一并消失。
///
/// 任何新的浮层都要用这里的 `Modal` 或 `Overlay`，不要再自绘 `fixed inset-0`。

/// Esc 关闭 + 打开期间锁背景滚动。两个模态原语共用。
function useModalChrome(onClose: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = prev;
    };
  }, [onClose]);
}

/// 自定义布局的浮层（抽屉、非居中面板）。只负责 portal + 遮罩 + Esc + 锁滚动，
/// 里面长什么样由调用方决定。
export function Overlay({
  onClose,
  className = "flex items-center justify-center p-6",
  children,
}: {
  onClose: () => void;
  /// 外层定位容器的类，默认居中。
  className?: string;
  children: React.ReactNode;
}) {
  useModalChrome(onClose);
  return createPortal(
    <div className={`fixed inset-0 z-[100] ${className}`}>{children}</div>,
    document.body,
  );
}

export function Modal({
  onClose,
  className = "max-w-2xl",
  children,
}: {
  onClose: () => void;
  /// 宽度类，默认中等宽度。
  className?: string;
  children: React.ReactNode;
}) {
  useModalChrome(onClose);

  return createPortal(
    <div
      className="animate-scrim fixed inset-0 z-[100] flex items-center justify-center bg-black/40 p-6"
      onMouseDown={(e) => {
        // 只在真正点到遮罩本身时关闭；从内容里开始的拖选滑到遮罩上不算。
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        onMouseDown={(e) => e.stopPropagation()}
        className={`animate-materialize material-modal max-h-[88vh] w-full overflow-y-auto rounded-2xl border border-border p-6 ${className}`}
      >
        {children}
      </div>
    </div>,
    document.body,
  );
}
