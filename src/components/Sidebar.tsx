import { useState } from "react";
import {
  Activity,
  Server,
  ArrowUpCircle,
  ChevronLeft,
  ChevronRight,
  ExternalLink,
  Power,
  PowerOff,
  Blocks,
  Cable,
  FileCode,
  Gauge,
  Globe,
  LayoutDashboard,
  LifeBuoy,
  FolderOpen,
  Unlock,
  ScrollText,
  Settings,
  Shuffle,
  GitBranch,
  PackagePlus,
  Workflow,
} from "lucide-react";
import { useClientVersion, useUpdateCheck } from "../hooks/useUpdate";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { useI18n, useT } from "../i18n";
import { Languages } from "lucide-react";
import type { KernelStatus, Page } from "../types";
// 应用图标即产品 logo；`@icons` 指向 src-tauri/icons，见 vite.config.ts。
import logoUrl from "@icons/128x128.png";

// 渠道/令牌 的增删改都在「内核后台」里，用的是 ccLoad 自带的界面，所以这里不再
// 单列 —— 自绘表单只会是个字段更少的弱化版。只读的观测（总览/日志）才自绘。
//
// 八项平铺时找一个入口要从头扫到尾，所以按「你现在想干什么」分三组：先看发生了
// 什么（监控），再改它怎么跑（配置），最后才是环境本身（系统）。分组只用一行小
// 标题，不加折叠、不加图标 —— 八项而已，能收起来的东西不值得多一次点击。
const GROUPS: { title: string; items: { id: Page; label: string; icon: typeof Activity }[] }[] = [
  {
    title: "监控",
    items: [
      { id: "dashboard", label: "总览", icon: LayoutDashboard },
      { id: "logs", label: "实时日志", icon: ScrollText },
      { id: "usage", label: "订阅用量", icon: Gauge },
      { id: "sessions", label: "会话救援", icon: LifeBuoy },
      { id: "session-manage", label: "会话管理", icon: FolderOpen },
    ],
  },
  {
    title: "配置",
    items: [
      { id: "cli", label: "CLI 接管", icon: Cable },
      { id: "graph", label: "调度图", icon: Workflow },
      { id: "fallback", label: "模型链", icon: GitBranch },
      { id: "forced-route", label: "强制路由", icon: Shuffle },
      { id: "models", label: "模型导入", icon: PackagePlus },
      { id: "inject", label: "系统注入", icon: FileCode },
      { id: "unlock", label: "破禁", icon: Unlock },
      { id: "extensions", label: "扩展管理", icon: Blocks },
      { id: "node-services", label: "Node 服务", icon: Server },
    ],
  },
  {
    title: "系统",
    items: [
      { id: "web-admin", label: "内核后台", icon: Globe },
      { id: "settings", label: "设置", icon: Settings },
    ],
  },
];

/// 收起状态记在 localStorage：这是「工作台怎么摆」的偏好，重开应用还得是
/// 用户上次摆的样子，不该每次回到默认。
const COLLAPSE_KEY = "ccload.sidebar.collapsed";

export function Sidebar(props: {
  page: Page;
  onNavigate: (p: Page) => void;
  status?: KernelStatus;
  onStart: () => void;
  onStop: () => void;
  starting: boolean;
  stopping: boolean;
}) {
  const t = useT();
  const { lang, setLang } = useI18n();
  const clientVersion = useClientVersion();
  const version = clientVersion.version;
  const update = useUpdateCheck(clientVersion);
  const running = props.status?.state === "running";
  const [collapsed, setCollapsed] = useState(
    () => localStorage.getItem(COLLAPSE_KEY) === "1",
  );
  const toggle = () => {
    setCollapsed((c) => {
      localStorage.setItem(COLLAPSE_KEY, c ? "0" : "1");
      return !c;
    });
  };

  return (
    // Heavier material than the content area: weight encodes hierarchy, and a
    // sidebar is a structural region rather than an interactive element.
    //
    // 宽度用 transition 而不是直接切：折叠是用户主动发起的动作，中间过程要看得
    // 见才知道内容是「收起来了」而不是「消失了」。曲线用设计系统的临界阻尼。
    // z-20：`<main>` 带 mask-image 会自成层叠上下文，DOM 里又在侧栏之后，
    // 不抬层的话骑在分隔线上的收起把手会被主区盖掉一半。
    <aside
      className={cn(
        "material-chrome group/aside relative z-20 flex shrink-0 flex-col border-r border-border",
        "transition-[width] duration-[220ms] ease-[cubic-bezier(0.32,0.72,0,1)]",
        collapsed ? "w-[4.25rem]" : "w-60",
      )}
    >
      {/* 收起把手放在分隔线中部：它调节的是这条边界本身，手就该落在边界上；
          纵向居中则让展开/收起两种宽度下的目标位置几乎不动，不用重新找。
          常驻可见，不做 hover 才浮现 —— 把手有一半探出侧栏之外，那半边不在
          hover 区里，于是「移过去按一下」经常是第一下只把它点亮、第二下才真的
          切换；而切换后宽度动画会把它从指针下挪走，hover 又掉了，看起来就是
          「要连点几下」。常驻显示把这个状态依赖整个去掉。 */}
      <button
        onClick={toggle}
        title={collapsed ? t("展开侧栏") : t("收起侧栏")}
        aria-label={collapsed ? t("展开侧栏") : t("收起侧栏")}
        aria-expanded={!collapsed}
        className={cn(
          "absolute right-0 top-1/2 z-10 flex h-11 w-[18px] -translate-y-1/2 translate-x-1/2",
          "items-center justify-center rounded-full border border-border bg-surface-raised",
          "text-muted shadow-sm transition-[color,background-color,border-color] duration-150",
          "hover:border-accent hover:bg-accent hover:text-white",
        )}
      >
        {collapsed ? (
          <ChevronRight className="h-3.5 w-3.5" />
        ) : (
          <ChevronLeft className="h-3.5 w-3.5" />
        )}
      </button>

      <div className={cn("flex items-center py-4", collapsed ? "justify-center px-2" : "gap-2.5 px-4")}>
        <div className="relative shrink-0">
          <img
            src={logoUrl}
            alt=""
            aria-hidden
            className="h-5 w-5 rounded-[5px]"
            title={collapsed ? `ccLoad v${version}` : undefined}
          />
          {/* 收起状态下没地方放按钮，改成 logo 角上一个点 —— 至少让人知道
              「展开看看」。位置贴着 logo 而不是浮在侧栏边缘，收起时侧栏只有
              一个图标宽，浮出去会被裁掉。 */}
          {collapsed && update?.available && (
            <span
              className="absolute -right-0.5 -top-0.5 h-2 w-2 rounded-full bg-accent ring-2 ring-surface-raised"
              title={t("有新版本 {v}", { v: update.latest })}
            />
          )}
        </div>
        {!collapsed && (
          <div className="min-w-0">
            <div className="t-title">ccLoad</div>
            {/* 完整版本单独一行：beta 形如 0.1.0-beta.20260820.2，跟标题挤同一
                行会被截成基座 0.1.0，用户就分不清装的是哪一包。底部那行是内核
                版本，两者互不牵连。 */}
            <div
              title={t("客户端版本")}
              className="mt-0.5 break-all font-mono text-[10px] leading-snug text-muted"
            >
              v{version}
            </div>
            {/* 有新版才出现。没有新版时**什么都不显示** —— 常驻一个「已是最新」
                占着两行，用户每天看它一百次，而它一年只有几天是有用的。
                点开浏览器就完了：我们不下载、不替换、不动磁盘，所以这里是个
                链接的行为，不是「安装」按钮，措辞也得对得上。 */}
            {update?.available && (
              <button
                onClick={() => void api.openExternal(update.url)}
                title={t("在浏览器里打开 {v} 的发布页", { v: update.latest })}
                className="mt-1.5 flex w-full items-center gap-1 rounded-md bg-accent/10 px-1.5 py-1 text-[10px] font-medium text-accent hover:bg-accent/20"
              >
                <ArrowUpCircle className="h-3 w-3 shrink-0" />
                <span className="min-w-0 flex-1 truncate text-left">
                  {update.prerelease ? t("有新 beta") : t("有新版本")}
                </span>
                <ExternalLink className="h-2.5 w-2.5 shrink-0 opacity-70" />
              </button>
            )}
          </div>
        )}
      </div>

      <nav className={cn("flex-1 space-y-4", collapsed ? "px-2" : "px-2.5")}>
        {GROUPS.map((group) => (
          <div key={group.title} className="space-y-0.5">
            {/* 收起时分组标题换成一条细分隔线：分组关系还在，只是不占字宽。 */}
            {collapsed ? (
              <div className="mx-2 mb-1.5 border-t border-border/70" />
            ) : (
              <div className="px-3 pb-1 text-[10px] font-medium uppercase tracking-wider text-muted/70">
                {t(group.title)}
              </div>
            )}
            {group.items.map((item) => (
              <button
                key={item.id}
                onClick={() => props.onNavigate(item.id)}
                aria-current={props.page === item.id ? "page" : undefined}
                // 收起后图标是唯一的线索，必须有原生 tooltip 兜底。
                title={collapsed ? t(item.label) : undefined}
                className={cn(
                  "flex w-full items-center rounded-lg py-2 text-sm",
                  collapsed ? "justify-center px-0" : "gap-2.5 px-3",
                  props.page === item.id
                    ? "bg-accent/12 font-medium text-accent"
                    : "text-muted hover:bg-surface-2 hover:text-content",
                )}
              >
                <item.icon className="h-4 w-4 shrink-0" />
                {!collapsed && t(item.label)}
              </button>
            ))}
          </div>
        ))}
      </nav>

      <div className={cn("border-t border-border text-xs", collapsed ? "p-2" : "p-3")}>
        {/* 语言开关放侧栏底部：它是「这个工作台怎么摆」的一部分，和内核状态、
            收起状态同属一类，不该藏进设置页第三屏。 */}
        <button
          onClick={() => setLang(lang === "zh-CN" ? "en" : "zh-CN")}
          title={t("语言")}
          aria-label={t("语言")}
          className={cn(
            "mb-2 flex items-center rounded-lg py-1.5 text-muted hover:bg-surface-2 hover:text-content",
            collapsed ? "w-full justify-center px-0" : "w-full gap-2 px-2",
          )}
        >
          <Languages className="h-3.5 w-3.5 shrink-0" />
          {!collapsed && <span className="text-[11px]">{lang === "zh-CN" ? t("中文") : "English"}</span>}
        </button>
        <StatusDot status={props.status} collapsed={collapsed} />
        {running ? (
          <button
            onClick={props.onStop}
            disabled={props.stopping}
            title={collapsed ? t("停止内核") : undefined}
            className={cn(
              "mt-2.5 w-full rounded-lg border border-border py-2 font-medium hover:border-red-300 hover:bg-red-50 hover:text-red-700 disabled:opacity-50",
              collapsed ? "px-0" : "px-2",
            )}
          >
            {collapsed ? (
              <PowerOff className="mx-auto h-4 w-4" />
            ) : props.stopping ? (
              t("停止中…")
            ) : (
              t("停止内核")
            )}
          </button>
        ) : (
          <button
            onClick={props.onStart}
            disabled={props.starting}
            title={collapsed ? t("启动内核") : undefined}
            className={cn(
              "mt-2.5 w-full rounded-lg bg-accent py-2 font-medium text-white shadow-sm hover:bg-accent/90 disabled:opacity-50",
              collapsed ? "px-0" : "px-2",
            )}
          >
            {collapsed ? <Power className="mx-auto h-4 w-4" /> : props.starting ? t("启动中…") : t("启动内核")}
          </button>
        )}
      </div>
    </aside>
  );
}

function StatusDot({ status, collapsed }: { status?: KernelStatus; collapsed?: boolean }) {
  const t = useT();
  const running = status?.state === "running";
  const color =
    status?.state === "running"
      ? "bg-emerald-500"
      : status?.state === "starting"
        ? "bg-amber-500"
        : status?.state === "failed"
          ? "bg-red-500"
          : "bg-zinc-400";
  const label =
    status?.state === "running"
      ? `${t("运行中")} · ${status.version}`
      : status?.state === "starting"
        ? t("启动中（约 20s）")
        : status?.state === "failed"
          ? status.message
          : t("已停止");
  return (
    <div
      className={cn(
        "flex items-start gap-2 text-muted",
        collapsed && "justify-center",
      )}
      // 收起时文字没了，状态只剩一个圆点，必须靠 tooltip 说清楚。
      title={collapsed ? label : undefined}
    >
      <span className="relative mt-1.5 flex h-2 w-2 shrink-0">
        {/* Status is ongoing, so it gets an ongoing signal — but only while
            actually running, and slow enough not to nag. */}
        {running && (
          <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-500 opacity-60" />
        )}
        <span className={cn("relative inline-flex h-2 w-2 rounded-full", color)} />
      </span>
      {!collapsed && <span className="break-all">{label}</span>}
    </div>
  );
}
