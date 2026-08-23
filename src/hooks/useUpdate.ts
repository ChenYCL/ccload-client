import { useEffect, useState } from "react";
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { getVersion } from "@tauri-apps/api/app";
import { api } from "../lib/api";
import type { CheckError, UpdateInfo } from "../types";

/// 壳体版本 + 「有没有新版」。侧栏和设置页共用一份，两处的结论必须一致 ——
/// 各查各的会出现「侧栏说有新版、设置页说已是最新」这种没法解释的画面。
/// 用的是同一个 queryKey，所以设置页按一下「检查更新」，侧栏那个按钮当场跟着变。

/// 一小时一轮。
///
/// 别改回「开窗时查一次就够」—— 那正是这个提示第一版没人见过它出现的原因，
/// 而且它错得比看上去更彻底：这个应用**关窗是隐藏不是退出**，点掉红灯再从
/// 托盘打开，webview 一直活着，React 没重新挂载，只在挂载时跑的查询自然也不会
/// 再跑。用户以为自己重启过了，其实那个进程已经连着跑了好几天。
///
/// 代价：GitHub 未认证配额是每小时 60 次/IP，这里占 1 次。跟版流水线本身也是
/// 每小时轮一次上游，比它更密没有意义 —— 再快也快不过上游被发现的速度。
const POLL_MS = 60 * 60 * 1000;

export type ClientVersion = { version: string; settled: boolean };

/// 打包后的真实壳体版本。Vite 注入的是 `package.json` 基座（`0.1.0`）；
/// beta 流水线会把完整 tag 戳进 `tauri.conf.json`，`getVersion()` 读的是那个，
/// 才能看到 `0.1.0-beta.20260823.1` 而不是截成基座。
///
/// `settled` 是「这个值已经是终值了」，不是「拿到了 Tauri 的版本」—— 纯 vite
/// 预览里 getVersion() 会失败，那时构建期注入值就是终值，同样算落地。
export function useClientVersion(): ClientVersion {
  const [state, setState] = useState<ClientVersion>({
    version: __CLIENT_VERSION__,
    settled: false,
  });
  useEffect(() => {
    void getVersion()
      .then((version) => setState({ version, settled: true }))
      .catch(() => {
        /* 纯 vite 预览没有 Tauri，继续用构建期注入值 */
        setState((s) => ({ ...s, settled: true }));
      });
  }, []);
  return state;
}

/// 完整的查询状态。设置页用它 —— 那里要能看见失败原因，也要能手动重查。
export function useUpdateQuery({
  version,
  settled,
}: ClientVersion): UseQueryResult<UpdateInfo, CheckError> {
  return useQuery<UpdateInfo, CheckError>({
    queryKey: ["client-update", version],
    queryFn: () => api.checkClientUpdate(version),
    staleTime: POLL_MS,
    refetchInterval: POLL_MS,
    // 全局默认是 false（对那些一开就固定的配置类查询是对的）。这一条相反：
    // 离开一天再回来，第一眼就该是最新的判断。有 staleTime 兜着，来回切窗口
    // 不会真的每次都打网络。而这个应用藏进托盘时 `document.hidden` 为真，
    // refetchInterval 会自动暂停，回到前台那一下正好由它补上。
    refetchOnWindowFocus: true,
    // 重试是必需的，不是保险丝。实测这条网络到 api.github.com 五次里挂两次
    // （SSL_ERROR_SYSCALL / connection timed out，且没有任何代理介入）。单打一
    // 次就等于四成概率永远查不到 —— 这个提示第一版没人见过它出现，主因就是它。
    // 三次机会把单轮失败率压到个位数，再加上每小时一轮，很快就会追上。
    //
    // 只重试**没连上**的那种。GitHub 答了话（限流 403、404）就别再打了：
    // 撞限流时重试只会把配额烧得更快，而 404 重试多少次都还是 404。
    // 这个判断靠后端给的 tag，不靠抠错误文本 —— reqwest 的传输错误里本来就
    // 可能带 `HTTP/2 stream error`，抠字符串会静默判反。
    retry: (n, err) => n < 2 && err?.kind === "transport",
    retryDelay: (n) => 2000 * 2 ** n,
    // 必须等 getVersion() 落地才查，光判 `version` 非空是不够的 ——
    // useClientVersion 的初值就是构建期注入的基座 0.1.0，那一瞬间它非空但是错的。
    // 不等的话每次开应用会打两次 GitHub（基座一次、真版本一次），白烧半小时才
    // 60 次的配额；而装着正式版的人更会先看到一次「有新 beta」再消失。
    enabled: settled,
  });
}

/// 侧栏用的安静版本。
///
/// **失败必须静默。** 断网、限流、公司网络挡了 api.github.com 都会走到这里，
/// 而侧栏那个只是锦上添花的提示 —— 在导航顶上常驻一条红字，比不提示更糟。
/// 所以这里只交出 `data`，调用方连渲染错误的机会都没有；要看失败原因去设置页。
export function useUpdateCheck(cv: ClientVersion): UpdateInfo | undefined {
  return useUpdateQuery(cv).data;
}
