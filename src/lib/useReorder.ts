import { useRef, useState } from "react";

/// 竖向列表的 1:1 拖拽排序。
///
/// 用 Pointer Events 手写而不是 HTML5 drag-and-drop：后者要等系统生成拖拽影像、
/// 中途给不出连续反馈，松手前根本看不出会落到哪。这里按下即跟手，其余节点实时
/// 让位，松手落到当前看到的位置 —— 所见即所得。
/// `setPointerCapture` 让指针移出节点范围后仍然跟随，这在列表边缘尤其重要。
export function useReorder<T>(
  items: T[],
  onReorder: (next: T[]) => void,
  axis: "x" | "y" = "y",
) {
  const listRef = useRef<HTMLOListElement>(null);
  const [drag, setDrag] = useState<{ from: number; dy: number; step: number } | null>(null);

  /** 一行（或一列）占多宽/高（含间距）。节点等距，量前两个就够。 */
  const stride = () => {
    const el = listRef.current;
    const horizontal = axis === "x";
    if (!el || el.children.length < 2) {
      const box = el?.children[0]?.getBoundingClientRect();
      return (horizontal ? box?.width : box?.height) ?? 1;
    }
    const a = el.children[0].getBoundingClientRect();
    const b = el.children[1].getBoundingClientRect();
    return Math.max(1, horizontal ? b.left - a.left : b.top - a.top);
  };

  const start = (from: number) => (e: React.PointerEvent) => {
    // 只响应主键；右键/中键拖拽不是这个手势。
    if (e.button !== 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    e.preventDefault();
    const origin = axis === "x" ? e.clientX : e.clientY;
    const h = stride();
    setDrag({ from, dy: 0, step: 0 });

    const move = (ev: PointerEvent) => {
      const dy = (axis === "x" ? ev.clientX : ev.clientY) - origin;
      // 落点 = 位移换算成的格数，钳在列表范围内。
      const step = Math.max(
        -from,
        Math.min(items.length - 1 - from, Math.round(dy / h)),
      );
      setDrag({ from, dy, step });
    };
    const up = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      const dy = (axis === "x" ? ev.clientX : ev.clientY) - origin;
      const step = Math.max(
        -from,
        Math.min(items.length - 1 - from, Math.round(dy / h)),
      );
      setDrag(null);
      if (step !== 0) onReorder(moved(items, from, from + step));
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  /** 键盘也要能排序：拖拽是唯一手段的话，用不了指针的人就被挡在外面。 */
  const onKeyDown = (i: number) => (e: React.KeyboardEvent) => {
    const back = axis === "x" ? "ArrowLeft" : "ArrowUp";
    const fwd = axis === "x" ? "ArrowRight" : "ArrowDown";
    const dir = e.key === back ? -1 : e.key === fwd ? 1 : 0;
    if (dir === 0) return;
    const to = i + dir;
    if (to < 0 || to >= items.length) return;
    e.preventDefault();
    onReorder(moved(items, i, to));
  };

  /// 拖拽中，每个节点该往哪儿挪多少。被拖的那个跟手；其余的整行让位。
  const offsetOf = (i: number) => {
    if (!drag) return 0;
    const { from, dy, step } = drag;
    if (i === from) return dy;
    const to = from + step;
    if (from < to && i > from && i <= to) return -stride();
    if (from > to && i < from && i >= to) return stride();
    return 0;
  };

  return { listRef, drag, start, onKeyDown, offsetOf, axis };
}

export function moved<T>(items: T[], from: number, to: number): T[] {
  const next = [...items];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item);
  return next;
}

