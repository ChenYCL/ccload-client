import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronDown } from "lucide-react";
import { cn } from "../../lib/cn";
import { useT } from "../../i18n";
import { TextInput } from "./Input";

/// 可搜索的下拉输入。
///
/// 为什么不是 `<select>`：这些位置填的是**上游模型名**，而候选清单不一定全
/// —— 渠道刚建好还没配模型、上游临时上了个新模型、或者用户就是要填一个
/// 清单外的名字。所以它是 combobox 而不是 select：给候选、能搜、但**仍然
/// 接受自由输入**。硬变成 select 会把「清单里没有的模型」这条路堵死。
///
/// 为什么下拉要 portal 到 body：它的使用位置在 `Modal` 里，而 Modal 是
/// `overflow-y-auto` 的。绝对定位的浮层会被裁在模态框下边缘 —— 越靠近底部的
/// 那一层裁得越狠，正好是链最长时最需要它的时候。用 fixed + portal 就不受任何
/// 祖先的 overflow / transform 影响（同 AGENTS.md 里模态框那条）。
export function ComboBox({
  value,
  onChange,
  options,
  placeholder,
  className,
  emptyHint,
  "aria-label": ariaLabel,
}: {
  value: string;
  onChange: (v: string) => void;
  /** 候选项。可以为空 —— 那时它就是个普通输入框。 */
  options: string[];
  placeholder?: string;
  className?: string;
  /** 没有候选时下拉里显示的一句话，用来解释「为什么这里是空的」。 */
  emptyHint?: string;
  "aria-label"?: string;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  // 用户是否正在打字。没打字时下拉给**全部**候选，而不是拿已选值去过滤自己
  // —— 否则点开下拉只看得见当前这一项，等于没法换。
  const [typed, setTyped] = useState<string | null>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const [rect, setRect] = useState<DOMRect | null>(null);

  const query = typed ?? "";
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const uniq = [...new Set(options.filter(Boolean))];
    if (!q) return uniq;
    // 子串匹配就够：模型名是 `claude-fable-5-thinking-xhigh` 这种连字符串，
    // 用户记得住的往往是中间某一段（`thinking`），前缀匹配会把它挡在外面。
    return uniq.filter((o) => o.toLowerCase().includes(q));
  }, [options, query]);

  const place = () => {
    const el = inputRef.current;
    if (el) setRect(el.getBoundingClientRect());
  };
  useLayoutEffect(() => {
    if (open) place();
  }, [open, filtered.length]);
  useEffect(() => {
    if (!open) return;
    // 滚动/缩放时跟着走。fixed 浮层不会自己跟随，不监听就会飘在原地。
    const on = () => place();
    window.addEventListener("scroll", on, true);
    window.addEventListener("resize", on);
    return () => {
      window.removeEventListener("scroll", on, true);
      window.removeEventListener("resize", on);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (wrapRef.current?.contains(target)) return;
      if ((target as HTMLElement)?.closest?.("[data-combobox-pop]")) return;
      setOpen(false);
      setTyped(null);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const commit = (v: string) => {
    onChange(v);
    setTyped(null);
    setOpen(false);
    inputRef.current?.focus();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!open) {
        setOpen(true);
        return;
      }
      const d = e.key === "ArrowDown" ? 1 : -1;
      setActive((a) => (filtered.length ? (a + d + filtered.length) % filtered.length : 0));
      return;
    }
    if (e.key === "Enter" && open && filtered[active]) {
      e.preventDefault();
      commit(filtered[active]);
      return;
    }
    if (e.key === "Escape" && open) {
      e.preventDefault();
      setOpen(false);
      setTyped(null);
    }
  };

  // 往下放不下就翻到上面。链的最后一层永远贴着模态框底边，不翻的话下拉
  // 整个在视口外。
  const MAX_H = 240;
  const below = rect ? window.innerHeight - rect.bottom : 0;
  const flip = rect != null && below < Math.min(MAX_H, 160) && rect.top > below;

  return (
    <div ref={wrapRef} className={cn("relative", className)}>
      <TextInput
        ref={inputRef}
        mono
        role="combobox"
        aria-expanded={open}
        aria-autocomplete="list"
        aria-label={ariaLabel}
        value={typed ?? value}
        placeholder={placeholder}
        onChange={(e) => {
          setTyped(e.target.value);
          onChange(e.target.value);
          setActive(0);
          setOpen(true);
        }}
        onFocus={() => setOpen(true)}
        // 焦点已经在框里时再点一下不会触发 focus —— 用 Esc 关掉下拉之后再点
        // 输入框，只有 onFocus 的话什么都不会发生，表现是「点了没反应」。
        onClick={() => setOpen(true)}
        onKeyDown={onKeyDown}
        className="w-full pr-7"
      />
      <button
        type="button"
        tabIndex={-1}
        aria-label={open ? t("收起候选") : t("展开候选")}
        onMouseDown={(e) => {
          // preventDefault 保住输入框焦点：否则点箭头会先 blur 再 focus，
          // 下拉刚开就被 mousedown-outside 关掉，表现是「点了没反应」。
          e.preventDefault();
          setTyped(null);
          setActive(0);
          setOpen((o) => !o);
          inputRef.current?.focus();
        }}
        className="absolute right-1.5 top-1/2 -translate-y-1/2 rounded p-0.5 text-muted hover:text-content"
      >
        <ChevronDown className={cn("h-3.5 w-3.5 transition-transform", open && "rotate-180")} />
      </button>

      {open &&
        rect &&
        createPortal(
          <div
            data-combobox-pop
            role="listbox"
            style={{
              position: "fixed",
              left: rect.left,
              width: rect.width,
              ...(flip
                ? { bottom: window.innerHeight - rect.top + 4 }
                : { top: rect.bottom + 4 }),
              maxHeight: MAX_H,
            }}
            className="z-[200] overflow-y-auto rounded-xl border border-border bg-surface-raised py-1 shadow-[var(--shadow-raised)]"
          >
            {filtered.length === 0 ? (
              <div className="px-3 py-2 text-[11px] text-muted">
                {query.trim()
                  ? t("没有匹配的模型，可直接用你输入的名字")
                  : (emptyHint ?? t("没有候选模型"))}
              </div>
            ) : (
              filtered.map((o, i) => (
                <button
                  key={o}
                  type="button"
                  role="option"
                  aria-selected={o === value}
                  onMouseEnter={() => setActive(i)}
                  onClick={() => commit(o)}
                  className={cn(
                    "block w-full truncate px-3 py-1.5 text-left font-mono text-xs",
                    i === active ? "bg-accent/10 text-accent" : "hover:bg-surface-2",
                    o === value && i !== active && "text-accent",
                  )}
                  title={o}
                >
                  {o}
                </button>
              ))
            )}
          </div>,
          document.body,
        )}
    </div>
  );
}
