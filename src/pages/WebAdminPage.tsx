//! Host for ccLoad's stock admin UI, rendered inside the main window.
//!
//! History, so we don't re-litigate it: iframe→blocked by the kernel's
//! hard-coded X-Frame-Options: DENY; iframe through a loopback header-
//! stripping proxy→works in Chromium but WKWebView renders cross-origin
//! sandboxed frames unreliably on macOS. What works is a **docked child
//! webview**: `window.add_child` puts a real webview over our placeholder
//! div. A child webview is a view, not a frame, so X-Frame-Options never
//! applies and the kernel loads from its own origin, exactly like the
//! standalone window (still available from the button below).
//!
//! 这个原生子 webview **画在页面之上**，不参与文档流 —— 所以这一页不放任何
//! 壳体自己的标题/说明：内核页面自带完整导航条（logo、菜单、版本、注销），
//! 再顶一行「内核后台」既重复又正好是被盖住的那一行。面板满幅铺开，壳体的
//! 出口按钮和管理密码收在底部一条里，那里子 webview 永远够不到。

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

const sameRect = (a: Rect, b: Rect) =>
  Math.abs(a.x - b.x) < 0.5 &&
  Math.abs(a.y - b.y) < 0.5 &&
  Math.abs(a.width - b.width) < 0.5 &&
  Math.abs(a.height - b.height) < 0.5;

export function WebAdminPage() {
  const t = useT();
  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  const settings = useQuery({ queryKey: ["app-settings"], queryFn: api.settingsGet });

  const data = kernel.data?.state === "running" ? kernel.data : null;
  const running = data !== null;
  const baseUrl = data?.base_url ?? null;

  const slotRef = useRef<HTMLDivElement>(null);
  // 已经**确认落到原生侧**的坐标。没确认就不许更新它 —— 见下面 inflight 那段。
  const lastRect = useRef<Rect | null>(null);
  const inflight = useRef(false);

  // 挂载 / 更新内嵌面板。StrictMode 双跑 effect 也没关系 —— show 是幂等的。
  useLayoutEffect(() => {
    const el = slotRef.current;
    if (!running || !el) return;
    const rect = rectOf(el);
    if (rect.width < 1 || rect.height < 1) return;
    inflight.current = true;
    void api
      .adminDockShow("channels.html", rect)
      .catch(console.error)
      .finally(() => {
        // 清空基准，逼下一帧按**当时**的真实布局再对一次。
        //
        // 这是内嵌面板盖住页面顶部的真正原因：add_child 是异步的，中间还夹着一
        // 次登录请求，几百毫秒。这段时间里布局还在动（字体、侧栏动画、查询到位
        // 后的重排），rAF 量到新 rect 就发 bounds —— 而那时 webview 还不存在，
        // 后端只能静默丢掉。旧代码把「发过了」直接记成基准，于是面板最终停在
        // 挂载瞬间那个过期坐标上，且**再也不会被纠正**（基准和现值一直相等）。
        lastRect.current = null;
        inflight.current = false;
      });
  }, [running]);

  // 布局跟踪：窗口 resize、侧栏折叠（宽度 transition 220ms）、字体加载都会改占
  // 位元素的位置。rAF 比 ResizeObserver 覆盖面广（纯位移它不管），每帧一次
  // getBoundingClientRect + 浅比较可以忽略不计。
  //
  // 只有后端回 true（面板真的挪了）才推进基准；被丢掉的那次下一帧自动重试。
  // inflight 保证同一时刻最多一条在飞，不会 60fps 轰炸主线程 —— 子 webview 的
  // set_bounds 要派发到主线程，轰它就是当年「白屏后冻结」的配方。
  useEffect(() => {
    if (!running) return;
    let raf = 0;
    const tick = () => {
      const el = slotRef.current;
      if (el && !inflight.current && document.visibilityState === "visible") {
        const rect = rectOf(el);
        // 窗口被藏起来时 rect 会塌成 0：那不是布局，别把面板挪成零尺寸。
        if (rect.width >= 1 && rect.height >= 1) {
          const prev = lastRect.current;
          if (!prev || !sameRect(prev, rect)) {
            inflight.current = true;
            void api
              .adminDockBounds(rect)
              .then((applied) => {
                if (applied) lastRect.current = rect;
              })
              .catch(() => {})
              .finally(() => {
                inflight.current = false;
              });
          }
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [running]);

  // 离开页面就藏起来（不销毁：回来时登录态和页面状态都还在）。反复建拆子
  // webview 正是当年「白屏后冻结」传说里最可疑的一环，不碰它。
  useEffect(() => {
    return () => {
      if (running) void api.adminDockHide().catch(() => {});
    };
  }, [running]);

  return (
    <div className="flex h-full flex-col">
      {running ? (
        /* 占位：负 margin 抵掉 <main> 的 px-[20px] py-6，让内核页面满幅铺开。
           这里必须保持空的 —— 真正的内容在原生层，画什么都会被盖住。 */
        <div ref={slotRef} className="-mx-[20px] -mt-6 min-h-0 flex-1 bg-surface" />
      ) : (
        <div className="flex min-h-0 flex-1 items-center justify-center">
          <p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm">
            {t("内核未运行。左下角「启动内核」后再打开管理界面。")}
          </p>
        </div>
      )}

      {/* 底部一条：出口按钮 + 管理密码。
          密码这段原来被 `mode === "managed"` 挡着，远端模式下整段不显示 —— 于是
          用户对着一个登录框，既不知道为什么要登、也不知道密码是什么。两种模式都
          要给，只是密码的来历不同。 */}
      <div className="mt-2 flex shrink-0 items-center gap-2 rounded-md border border-border bg-surface-2 px-3 py-1.5 text-xs">
        {settings.data && (
          <>
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
          </>
        )}
        <div className="ml-auto flex shrink-0 items-center gap-1.5">
          <button
            onClick={() => void api.openAdminWindow().catch(console.error)}
            disabled={!running}
            title={t("在独立窗口中打开")}
            className="rounded-md border border-border bg-surface-raised p-1.5 hover:bg-surface disabled:cursor-not-allowed disabled:opacity-50"
          >
            <Maximize2 className="h-3.5 w-3.5" />
          </button>
          {baseUrl && (
            <button
              onClick={() => api.openExternal(`${baseUrl}/web/`)}
              title={t("在浏览器中打开")}
              className="rounded-md border border-border bg-surface-raised p-1.5 hover:bg-surface"
            >
              <ExternalLink className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
