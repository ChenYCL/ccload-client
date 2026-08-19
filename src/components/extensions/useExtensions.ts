/// 扩展管理的数据接线。5 个 CLI 的清单是 5 个独立查询而不是一个聚合命令：
/// 某家配置文件损坏时只让那一家报错，其余照常显示 —— 一个坏掉的 config.toml
/// 不该把整页变成错误页。

import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import type { CliTarget, ExtensionItem, ExtensionKind } from "../../types";
import { ALL_TARGETS, TARGET_LABELS } from "./model";

/// 所有扩展查询共用的前缀，写操作之后按前缀整体失效。
const LIST_KEY = "extensions-list";

export function useExtensionData() {
  const support = useQuery({
    queryKey: ["extensions-support"],
    queryFn: api.extensionsSupport,
    // 支持矩阵是编译进后端的常量表，没有过期一说。
    staleTime: Infinity,
  });

  const lists = useQueries({
    queries: ALL_TARGETS.map((target) => ({
      queryKey: [LIST_KEY, target],
      queryFn: () => api.extensionsList(target),
    })),
  });

  const items: ExtensionItem[] = [];
  const failures: { target: CliTarget; label: string; error: unknown }[] = [];
  lists.forEach((q, i) => {
    const target = ALL_TARGETS[i];
    if (q.error) failures.push({ target, label: TARGET_LABELS[target], error: q.error });
    if (q.data) items.push(...q.data);
  });

  return {
    support: support.data,
    items,
    /** 逐 CLI 的读取失败，页面顶部提示，不阻塞其余内容。 */
    failures,
    /** 首屏还没有任何数据可画 —— 已有数据的刷新不该退回骨架屏。 */
    isLoading: support.isLoading || lists.some((q) => q.isLoading),
    /** 只有支持矩阵挂了才是真正的整页错误：没有它连置灰都算不出来。 */
    error: support.error,
  };
}

/// 装 / 卸 / 同步都会改磁盘上的真实配置，改完必须重读清单，否则徽章会停在旧
/// 状态。写操作本身故意留在各自的组件里各建一份 mutation：结果要显示在触发它
/// 的那张卡片上，共用一个实例会让 A 行的失败出现在 B 行下面。
export function useInvalidateExtensions() {
  const qc = useQueryClient();
  return () => qc.invalidateQueries({ queryKey: [LIST_KEY] });
}

/// 编辑框的回填。没选中条目时不发请求。
export function useExtensionSpec(
  target: CliTarget | null,
  kind: ExtensionKind,
  id: string | null,
) {
  return useQuery({
    queryKey: ["extension-spec", target, kind, id],
    queryFn: () => api.extensionRead(target as CliTarget, kind, id as string),
    enabled: target !== null && id !== null,
  });
}
