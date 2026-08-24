//! 会话列表共用：格式化、筛选、排序。救援页和管理页扫的是同一批文件。

import type { Translate } from "../i18n";
import type { SessionInfo } from "../types";

export type SessionSort = "recent" | "oldest" | "peak" | "current" | "size";

export function projectName(s: SessionInfo): string {
  return s.cwd.split("/").pop() || s.cwd;
}

export function fmtTokens(n: number): string {
  if (n <= 0) return "—";
  if (n < 1000) return String(n);
  return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function fmtAgo(unixSec: number, t: Translate): string {
  if (!unixSec) return "";
  const sec = Math.floor(Date.now() / 1000) - unixSec;
  if (sec < 3600) return t("{n} 分钟前", { n: Math.max(1, Math.floor(sec / 60)) });
  if (sec < 86_400) return t("{n} 小时前", { n: Math.floor(sec / 3600) });
  return t("{n} 天前", { n: Math.floor(sec / 86_400) });
}

/// 项目下拉：按会话数降序，多的排前面。
export function uniqueProjects(all: SessionInfo[]): [string, number][] {
  const n = new Map<string, number>();
  for (const s of all) {
    const p = projectName(s);
    n.set(p, (n.get(p) ?? 0) + 1);
  }
  return [...n.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

export function filterSessions(
  all: SessionInfo[],
  opts: {
    query: string;
    project: string;
    sort: SessionSort;
    /** 只留「最后改动」早于这么多天的。0 = 不过滤。 */
    olderThanDays?: number;
  },
): SessionInfo[] {
  const q = opts.query.trim().toLowerCase();
  const cutoff =
    opts.olderThanDays && opts.olderThanDays > 0
      ? Math.floor(Date.now() / 1000) - opts.olderThanDays * 86_400
      : 0;
  const out = all.filter((s) => {
    if (opts.project && projectName(s) !== opts.project) return false;
    if (cutoff && s.modified_at > cutoff) return false;
    if (!q) return true;
    return (
      s.slug.toLowerCase().includes(q) ||
      s.id.toLowerCase().includes(q) ||
      s.cwd.toLowerCase().includes(q)
    );
  });
  const by: Record<SessionSort, (a: SessionInfo, b: SessionInfo) => number> = {
    recent: (a, b) => b.modified_at - a.modified_at,
    oldest: (a, b) => a.modified_at - b.modified_at,
    peak: (a, b) => b.peak_context - a.peak_context,
    current: (a, b) => b.last_context - a.last_context,
    size: (a, b) => Number(b.bytes) - Number(a.bytes),
  };
  return [...out].sort(by[opts.sort]);
}
