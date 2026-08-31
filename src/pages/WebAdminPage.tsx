//! Launcher + host for ccLoad's stock admin UI.
//!
//! History, so we don't re-litigate it: iframe→blocked by the kernel's
//! hard-coded X-Frame-Options: DENY; iframe through a loopback header-
//! stripping proxy→works in Chromium but WKWebView renders cross-origin
//! sandboxed frames unreliably on macOS. Two working paths remain:
//!   * **Docked child webview** (below) — `window.add_child` puts a real
//!     webview over our placeholder div. A child webview is a view, not a
//!     frame, so X-Frame-Options never applies and the kernel is loaded from
//!     its own origin, exactly like the standalone window.
//!   * **Standalone window** — the escape-hatch button in the header.

import { useEffect, useLayoutEffect, useRef } from "react";
import { useQuery } from "@tanstack/react-query";
import { ExternalLink, Maximize2 } from "lucide-react";
import { api } from "../lib/api";
import { CopyButton } from "../components/ui/CopyButton";
import { useT } from "../i18n";

type Rect = { x: number; y: number; width: number; height: number };

/// 占位元素 → 相对窗口内容区的逻辑坐标。
///
/// getBoundingClientRect 给的是视口坐标，而 `<main>` 就是全高的滚动容器、
/// App 根节点不滚，所以视口坐标 == 窗口内容区坐标，不必再加滚动偏移。
function rectOf(el: HTMLElement): Rect {
  const r = el.getBoundingClientRect();
  return { x: r.x, y: r.y, width: r.width, height: r.height };
}

export function WebAdminPage() {
  const t = useT();
  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  const settings = useQuery({ queryKey: ["app-settings"], queryFn: api.settingsGet });

  const data = kernel.data?.state === "running" ? kernel.data : null;
  const running = data !== null;
  const baseUrl = data?.base_url ?? null;

  const slotRef = useRef<HTMLDivElement>(null);
  // 同一个 rect 同时喂 show 和 bounds 修正；ref 里存上一次的值，没变就不打扰后端。
  const lastRect = useRef<Rect | null>(null);
  const sameRect = (a: Rect, b: Rect) =>
    Math.abs(a.x - b.x) < 0.5 &&
    Math.abs(a.y - b.y) < 0.5 &&
    Math.abs(a.width - b.width) < 0.5 &&
    Math.abs(a.height - b.height) < 0.5;

  // 挂载 / 更新内嵌面板。StrictMode 双跑 effect 也没关系 ——
  // show 是幂等的（同页只挪位置，不同页 navigate）。
  useLayoutEffect(() => {
    const el = slotRef.current;
    if (!running || !el) return;
    const rect = rectOf(el);
    if (rect.width < 1 || rect.height < 1) return;
    lastRect.current = rect;
    void api.adminDockShow("channels.html", rect).catch(console.error);
  }, [running]);

  // 布局跟踪：窗口 resize、侧栏折叠（宽度 transition 220ms）、字体加载都会
  // 改占位元素的位置。rAF 循环比 ResizeObserver 覆盖面广（位置变化它不管），
  // 而每帧 getBoundingClientRect + 浅比较的开销可以忽略。组件卸载时停止。
  useEffect(() => {
    if (!running) return;
    let raf = 0;
    const tick = () => {
      const el = slotRef.current;
      if (el) {
        const rect = rectOf(el);
        const prev = lastRect.current;
        if (!prev || !sameRect(prev, rect)) {
          lastRect.current = rect;
          void api.adminDockBounds(rect).catch(console.error);
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [running]);

  // 离开页面就藏起来（不销毁：回来时页面状态还在）。effect cleanup 在换页
  // 时必跑，比在 App 层挂路由钩子简单。
  useEffect(() => {
    return () => {
      if (running) void api.adminDockHide().catch(() => {});
    };
  }, [running]);

  return (
    <div className="flex h-full flex-col">
      {/* 标题行压到最矮 —— 网页内容区要尽量大,这行只占标题一圈的高度。 */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 items-baseline gap-3">
          <h1 className="t-display">{t("内核后台")}</h1>
          <p className="truncate text-xs text-muted">
            {t("ccLoad 自带的管理界面，字段随内核升级自动跟进。")}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            onClick={() => void api.openAdminWindow().catch(console.error)}
            disabled={!running}
            title={t("在独立窗口中打开")}
            className="rounded-lg border border-border bg-surface-raised p-2 hover:bg-surface-2 disabled:cursor-not-allowed disabled:opacity-50"
          >
            <Maximize2 className="h-4 w-4" />
          </button>
          {baseUrl && (
            <button
              onClick={() => api.openExternal(`${baseUrl}/web/`)}
              title={t("在浏览器中打开")}
              className="rounded-lg border border-border bg-surface-raised p-2 hover:bg-surface-2"
            >
              <ExternalLink className="h-4 w-4" />
            </button>
          )}
        </div>
      </div>

      {!running && (
        <p className="mt-4 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm">
          {t("内核未运行。左下角「启动内核」后再打开管理界面。")}
        </p>
      )}

      {/* 内嵌面板的占位。子 webview 悬在它上面 —— 这里必须保持空的背景：
          真正的内容在原生层，画什么都会被盖住。圆角是原生 webview 做不到的
          （它是矩形），所以占位本身也不要圆角，视觉一致。 */}
      <div
        ref={slotRef}
        className="mt-4 min-h-0 flex-1 bg-surface"
      />

      {/* 这段提示原来被 `mode === "managed"` 挡着，远端模式下整段不显示 ——
          于是用户对着一个登录框，既不知道为什么要登、也不知道密码是什么。
          两种模式都要给，只是密码的来历不同。挪到底部：它只在「登录坏了」
          时才被用到，不该常年占着内容区上面最贵的位置。 */}
      {settings.data && (
        <div className="mt-2 flex items-center gap-2 rounded-md border border-border bg-surface-2 px-3 py-1.5 text-xs">
          <span className="shrink-0 text-muted">
            {settings.data.kernel.mode === "managed"
              ? t("管理密码（本机生成）")
              : t("管理密码（你在设置里填的 CCLOAD_PASS）")}
          </span>
          <code className="min-w-0 flex-1 truncate select-all font-mono text-accent">
            {settings.data.kernel.admin_password || "—"}
          </code>
          <CopyButton value={settings.data.kernel.admin_password ?? ""} />
          <span className="shrink-0 text-muted">
            {t("自动登录；会话过期退回登录页时用它手动登一次。")}
          </span>
        </div>
      )}
    </div>
  );
}
