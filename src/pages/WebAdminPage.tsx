//! Launcher panel for ccLoad's stock admin UI in a standalone window.
//!
//! History, so we don't re-litigate it: iframe→blocked by the kernel's
//! hard-coded X-Frame-Options: DENY; iframe through a loopback header-
//! stripping proxy→works in Chromium but WKWebView renders cross-origin
//! sandboxed frames unreliably on macOS; child Webview in the main
//! window→blank, then froze the whole app. A separate top-level
//! WebviewWindow is the stable path: DENY only blocks framing, so the
//! kernel needs no proxy and no patching.

import { useQuery } from "@tanstack/react-query";
import { ExternalLink, RefreshCw } from "lucide-react";
import { api } from "../lib/api";
import { CopyButton } from "../components/ui/CopyButton";

type Page = { id: string; label: string; file: string; desc: string };

const PAGES: Page[] = [
  {
    id: "channels",
    label: "渠道管理",
    file: "channels.html",
    desc: "上游渠道、Key、模型与降级链配置",
  },
  {
    id: "tokens",
    label: "令牌管理",
    file: "tokens.html",
    desc: "CLI 接入用的 API 令牌与限额",
  },
  {
    id: "logs",
    label: "请求日志",
    file: "logs.html",
    desc: "实时请求流与错误排查",
  },
  {
    id: "stats",
    label: "用量统计",
    file: "stats.html",
    desc: "成本、Token 用量与渠道健康度",
  },
  {
    id: "settings",
    label: "内核设置",
    file: "settings.html",
    desc: "内核运行参数与系统设置",
  },
];

export function WebAdminPage() {
  const kernel = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus });
  const settings = useQuery({ queryKey: ["app-settings"], queryFn: api.settingsGet });

  const data = kernel.data?.state === "running" ? kernel.data : null;
  const running = data !== null;
  const baseUrl = data?.base_url ?? null;

  const open = (file: string) => void api.openAdminWindow(file).catch(console.error);

  return (
    <div>
      <div className="flex items-center justify-between">
        <div>
          <h1 className="t-display">内核后台</h1>
          <p className="mt-1 text-xs text-muted">
            ccLoad 自带的管理界面，在独立窗口中打开，字段随内核升级自动跟进。
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => open("channels.html")}
            title="重新打开管理窗口"
            className="rounded-lg border border-border bg-surface-raised p-2 hover:bg-surface-2"
          >
            <RefreshCw className="h-4 w-4" />
          </button>
          {baseUrl && (
            <button
              onClick={() => api.openExternal(`${baseUrl}/web/`)}
              title="在浏览器中打开"
              className="rounded-lg border border-border bg-surface-raised p-2 hover:bg-surface-2"
            >
              <ExternalLink className="h-4 w-4" />
            </button>
          )}
        </div>
      </div>

      {!running && (
        <p className="mt-4 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm">
          内核未运行。左下角「启动内核」后再打开管理界面。
        </p>
      )}

      {/* 这段提示原来被 `mode === "managed"` 挡着，远端模式下整段不显示 ——
          于是用户对着一个登录框，既不知道为什么要登、也不知道密码是什么。
          两种模式都要给，只是密码的来历不同。 */}
      {settings.data && (
        <div className="mt-4 rounded-md border border-border bg-surface-2 px-3 py-2 text-xs">
          <div className="flex items-center gap-2">
            <span className="text-muted">
              {settings.data.kernel.mode === "managed"
                ? "管理密码（本机生成）"
                : "管理密码（你在设置里填的 CCLOAD_PASS）"}
            </span>
            <code className="min-w-0 flex-1 truncate select-all font-mono text-accent">
              {settings.data.kernel.admin_password || "—"}
            </code>
            <CopyButton value={settings.data.kernel.admin_password ?? ""} />
          </div>
          <p className="mt-1.5 text-muted">
            打开管理窗口时会自动登录，正常情况下你看不到登录框。会话有效期 24
            小时，过期或密码不对时会退回登录页 —— 那时用上面这个密码手动登一次。
          </p>
        </div>
      )}

      <div className="mt-4 grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {PAGES.map((p) => (
          <button
            key={p.id}
            disabled={!running}
            onClick={() => open(p.file)}
            className="card p-4 text-left transition-colors hover:border-accent/60 hover:bg-surface-2 disabled:cursor-not-allowed disabled:opacity-50"
          >
            <div className="t-title">{p.label}</div>
            <div className="mt-1 text-xs text-muted">{p.desc}</div>
          </button>
        ))}
      </div>
    </div>
  );
}
