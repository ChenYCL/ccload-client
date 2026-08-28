import { useEffect, useState } from "react";
import { useIsFetching, useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "./lib/api";
import type { Page } from "./types";
import { Sidebar } from "./components/Sidebar";
import { DashboardPage } from "./pages/DashboardPage";
import { LogsPage } from "./pages/LogsPage";
import { UsagePage } from "./pages/UsagePage";
import { WebAdminPage } from "./pages/WebAdminPage";
import { SettingsPage } from "./pages/SettingsPage";
import { CliPage } from "./pages/CliPage";
import { FallbackPage } from "./pages/FallbackPage";
import { ForcedRoutePage } from "./pages/ForcedRoutePage";
import { ModelsPage } from "./pages/ModelsPage";
import { InjectPage } from "./pages/InjectPage";
import { SessionsPage } from "./pages/SessionsPage";
import { SessionManagePage } from "./pages/SessionManagePage";
import { UnlockPage } from "./pages/UnlockPage";
import { ExtensionsPage } from "./pages/ExtensionsPage";
import { GraphPage } from "./pages/GraphPage";
import { NodeServicesPage } from "./pages/NodeServicesPage";
import { errText } from "./lib/err";
import { useT } from "./i18n";

/// 页面组件签名。多数页面不关心导航，但日志页要能把用户送到会话管理
/// （点一条日志上的会话名），所以统一给一个可选的 `onNavigate`。
type PageProps = { onNavigate?: (page: Page) => void };

const PAGES: Record<Page, (props: PageProps) => JSX.Element> = {
  dashboard: DashboardPage,
  logs: LogsPage,
  usage: UsagePage,
  "web-admin": WebAdminPage,
  settings: SettingsPage,
  cli: CliPage,
  fallback: FallbackPage,
  "forced-route": ForcedRoutePage,
  models: ModelsPage,
  inject: InjectPage,
  sessions: SessionsPage,
  "session-manage": SessionManagePage,
  unlock: UnlockPage,
  extensions: ExtensionsPage,
  graph: GraphPage,
  "node-services": NodeServicesPage,
};

/// 切页时右侧不要空着。
///
/// 「还没有东西可显示」的判据是 **data === undefined** —— 也就是这个 query 从来
/// 没成功过。不能用 `useIsFetching()` 的裸计数：内核状态 4s、日志 3s 都在后台
/// 轮询，那个数字几乎永远大于 0，转圈会停不下来。
///
/// 指示器叠在页面上方而不是替换页面：多数页面加载时已经能画出标题和按钮，
/// 把它们藏起来反而更空。`pointer-events-none`，数据一到就消失。
function LoadingVeil({ page }: { page: Page }) {
  const booting = useIsFetching({ predicate: (q) => q.state.data === undefined });
  const [show, setShow] = useState(false);
  const t = useT();

  // 迟 150ms 再出现：缓存命中时页面几乎瞬间就绪，转一下又消失比不转更闹。
  useEffect(() => {
    if (booting === 0) {
      setShow(false);
      return;
    }
    const id = setTimeout(() => setShow(true), 150);
    return () => clearTimeout(id);
  }, [booting, page]);

  if (!show) return null;
  return (
    <div
      className="pointer-events-none absolute inset-x-0 top-28 flex justify-center"
      role="status"
      aria-live="polite"
    >
      <span className="material-chrome animate-materialize flex items-center gap-2 rounded-full border border-border px-3.5 py-1.5 text-xs text-muted shadow-sm">
        <span className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-border border-t-accent" />
        {t("读取中…")}
      </span>
    </div>
  );
}

export function App() {
  const [page, setPage] = useState<Page>("dashboard");
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["kernel"], queryFn: api.kernelStatus, refetchInterval: 4000 });
  const start = useMutation({
    mutationFn: api.kernelStart,
    onSuccess: () => qc.invalidateQueries({ queryKey: ["kernel"] }),
  });
  const stop = useMutation({
    mutationFn: api.kernelStop,
    onSuccess: () => qc.invalidateQueries({ queryKey: ["kernel"] }),
  });

  const View = PAGES[page];
  return (
    <div className="flex h-full">
      <Sidebar
        page={page}
        onNavigate={setPage}
        status={status.data}
        onStart={() => start.mutate()}
        onStop={() => stop.mutate()}
        starting={start.isPending}
        stopping={stop.isPending}
      />
      {/* scroll-edge fades content into the chrome instead of a hard divider
          that is drawn whether or not anything is actually underneath it.
          左右各 20px：这些页面主体是宽表格，居中窄栏会在两侧留下两条什么都不
          放的空白（窗口越宽越明显）。 */}
      <main className="scroll-edge relative flex-1 overflow-auto px-[20px] py-6">
        {status.isError && (
          <div
            role="alert"
            className="mb-5 rounded-xl border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700"
          >
            {errText(status.error)}
          </div>
        )}
        {/* key 在这里其实不必要：换页就是换组件类型，React 本来就会整棵重建。
            留着是为了把这层意图写在脸上 —— 以后有人给 PAGES 加包装（比如 keep-alive）
            时不会以为换页会保留状态。 */}
        <View key={page} onNavigate={setPage} />
        <LoadingVeil page={page} />
      </main>
    </div>
  );
}
