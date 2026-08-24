import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Loader2, RefreshCw, Trash2 } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import { useT } from "../i18n";
import { ConfirmDialog } from "../components/extensions/ConfirmDialog";
import { Select, TextInput } from "../components/ui/Input";
import {
  filterSessions,
  fmtAgo,
  fmtBytes,
  fmtTokens,
  projectName,
  uniqueProjects,
  type SessionSort,
} from "../lib/sessionList";
import type { SessionInfo } from "../types";

/// 会话管理 —— 清太久没碰的 transcript。救援页是把撑爆的会话弄活，这一页是
/// 把用不着的删掉：~/.claude/projects 里几十 MB 一份的 jsonl 会一直堆着。
///
/// 不可恢复。Claude Code 没有回收站，所以点删除必须过二次确认。活着的会话
/// 后端会跳过 —— 进程里有内存态，删了它一落盘又写回来。

const AGE_OPTIONS = [0, 7, 14, 30, 60, 90] as const;

export function SessionManagePage() {
  const t = useT();
  const qc = useQueryClient();
  const [message, setMessage] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(() => new Set());
  const [confirming, setConfirming] = useState(false);

  const [query, setQuery] = useState("");
  const [project, setProject] = useState("");
  const [sort, setSort] = useState<SessionSort>("oldest");
  const [olderThan, setOlderThan] = useState(30);

  const sessions = useQuery({
    queryKey: ["sessions"],
    queryFn: api.sessionList,
    staleTime: Infinity,
  });

  const all = sessions.data ?? [];
  const projects = useMemo(() => uniqueProjects(all), [all]);
  const rows = useMemo(
    () => filterSessions(all, { query, project, sort, olderThanDays: olderThan }),
    [all, query, project, sort, olderThan],
  );

  const selectable = rows.filter((s) => !s.live);
  const selectedRows = selectable.filter((s) => selected.has(s.id));
  const allChecked = selectable.length > 0 && selectedRows.length === selectable.length;
  const selectedBytes = selectedRows.reduce((n, s) => n + Number(s.bytes), 0);

  const toggle = (id: string, on: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  };
  const toggleAll = (on: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      for (const s of selectable) {
        if (on) next.add(s.id);
        else next.delete(s.id);
      }
      return next;
    });
  };

  const remove = useMutation({
    mutationFn: (items: SessionInfo[]) => api.sessionDelete(items.map((s) => s.path)),
    onSuccess: (r) => {
      const parts = [
        t("已删除 {n} 个会话，腾出 {size}", {
          n: r.deleted,
          size: fmtBytes(r.bytes),
        }),
      ];
      if (r.skipped_live.length) {
        parts.push(t("跳过 {n} 个正在运行的", { n: r.skipped_live.length }));
      }
      if (r.errors.length) {
        parts.push(r.errors.join("\n"));
      }
      setMessage(parts.join("\n"));
      setSelected(new Set());
      setConfirming(false);
      qc.invalidateQueries({ queryKey: ["sessions"] });
    },
    onError: (e) => setMessage(errText(e)),
  });

  return (
    <div>
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="t-display">{t("会话管理")}</h1>
          <p className="mt-1 max-w-3xl text-sm text-muted">
            {t(
              "清太久没碰的 Claude Code 会话。删掉的文件不可恢复，救援留下的备份会一起清。正在运行的会话不会被动。",
            )}
          </p>
        </div>
        <button
          onClick={() => sessions.refetch()}
          disabled={sessions.isFetching}
          className="flex shrink-0 items-center gap-1 rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2 disabled:opacity-40"
        >
          <RefreshCw className={cn("h-4 w-4", sessions.isFetching && "animate-spin")} />
          {t("重新扫描")}
        </button>
      </div>

      <div className="mt-5 flex flex-wrap items-center gap-2">
        <TextInput
          small
          className="min-w-56 flex-1"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("搜名字、uuid 或路径")}
          aria-label={t("搜索会话")}
        />
        <Select
          small
          className="w-52 shrink-0"
          value={project}
          onChange={(e) => setProject(e.target.value)}
          aria-label={t("按项目筛选")}
        >
          <option value="">{t("全部项目（{n}）", { n: projects.length })}</option>
          {projects.map(([p, n]) => (
            <option key={p} value={p}>
              {p}（{n}）
            </option>
          ))}
        </Select>
        <Select
          small
          className="w-40 shrink-0"
          value={String(olderThan)}
          onChange={(e) => setOlderThan(Number(e.target.value))}
          aria-label={t("按最后改动筛选")}
        >
          {AGE_OPTIONS.map((d) => (
            <option key={d} value={d}>
              {d === 0 ? t("不限时间") : t("{n} 天前及更早", { n: d })}
            </option>
          ))}
        </Select>
        <Select
          small
          className="w-40 shrink-0"
          value={sort}
          onChange={(e) => setSort(e.target.value as SessionSort)}
          aria-label={t("排序")}
        >
          <option value="oldest">{t("最早改动")}</option>
          <option value="recent">{t("最近改动")}</option>
          <option value="size">{t("文件最大")}</option>
          <option value="peak">{t("峰值最大")}</option>
          <option value="current">{t("当前最大")}</option>
        </Select>
        {(query || project || olderThan !== 30) && (
          <button
            onClick={() => {
              setQuery("");
              setProject("");
              setOlderThan(30);
            }}
            className="shrink-0 whitespace-nowrap rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs text-muted hover:bg-surface-2"
          >
            {t("清除筛选")}
          </button>
        )}
        <span className="shrink-0 text-xs text-muted">
          {rows.length === all.length
            ? t("共 {n} 个会话", { n: all.length })
            : t("{shown} / {total}", { shown: rows.length, total: all.length })}
        </span>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        <label className="flex items-center gap-1.5 text-xs text-muted">
          <input
            type="checkbox"
            checked={allChecked}
            disabled={selectable.length === 0 || remove.isPending}
            onChange={(e) => toggleAll(e.target.checked)}
          />
          {t("全选当前列表")}
        </label>
        <button
          onClick={() => setConfirming(true)}
          disabled={remove.isPending || selectedRows.length === 0}
          className="flex items-center gap-1 rounded-lg bg-red-600 px-2.5 py-1 text-xs font-medium text-white shadow-sm hover:bg-red-700 disabled:opacity-40"
        >
          {remove.isPending ? (
            <Loader2 className="h-3 w-3 animate-spin" />
          ) : (
            <Trash2 className="h-3 w-3" />
          )}
          {selectedRows.length > 0
            ? t("删除选中（{n} · {size}）", {
                n: selectedRows.length,
                size: fmtBytes(selectedBytes),
              })
            : t("删除选中")}
        </button>
      </div>

      {rows.length === 0 && !sessions.isPending && (
        <p className="mt-6 text-sm text-muted">
          {all.length === 0
            ? t("没有找到任何会话。")
            : t("没有匹配的会话。换个关键词或清除筛选。")}
        </p>
      )}

      <ul className="mt-4 divide-y divide-border/60 rounded-xl border border-border">
        {rows.map((s) => (
          <li key={s.id} className="flex flex-wrap items-center gap-3 px-3 py-2.5">
            <input
              type="checkbox"
              checked={selected.has(s.id)}
              disabled={s.live || remove.isPending}
              onChange={(e) => toggle(s.id, e.target.checked)}
              aria-label={t("选中 {name}", { name: s.slug || s.id.slice(0, 8) })}
              className="shrink-0"
            />
            <span
              className={cn(
                "h-1.5 w-1.5 shrink-0 rounded-full",
                s.live ? "bg-emerald-500" : "bg-border",
              )}
              title={s.live ? t("正在运行") : t("已停止")}
            />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm" title={s.cwd}>
                {s.slug || s.id.slice(0, 8)}
                <span className="ml-2 text-xs text-muted">{projectName(s)}</span>
              </span>
              <span className="mt-0.5 block truncate font-mono text-[10px] text-muted/80">
                {s.id}
              </span>
            </span>
            <span className="shrink-0 text-right text-xs">
              <span className="block text-muted">{t("当前")}</span>
              <span className="font-mono">{fmtTokens(s.last_context)}</span>
            </span>
            <span className="hidden shrink-0 text-right text-xs text-muted sm:block">
              <span className="block">{fmtBytes(s.bytes)}</span>
              <span>{fmtAgo(s.modified_at, t)}</span>
            </span>
            {s.live ? (
              <span
                className="flex shrink-0 items-center gap-1 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-700"
                title={t("先退出那个 Claude Code 窗口 —— 进程里有内存态，现在改会被它盖回去")}
              >
                <AlertTriangle className="h-3 w-3" />
                {t("运行中")}
              </span>
            ) : null}
          </li>
        ))}
      </ul>

      {message && <p className="mt-4 whitespace-pre-line text-sm text-accent">{message}</p>}

      {confirming && (
        <ConfirmDialog
          title={t("确认删除 {n} 个会话？", { n: selectedRows.length })}
          body={
            <div className="space-y-2">
              <p>
                {t("将永久删除 {n} 个文件（约 {size}），救援留下的备份一并清掉。没有回收站。", {
                  n: selectedRows.length,
                  size: fmtBytes(selectedBytes),
                })}
              </p>
              <ul className="max-h-40 overflow-auto rounded-lg border border-border bg-surface-2 px-2 py-1.5 font-mono text-[11px]">
                {selectedRows.slice(0, 12).map((s) => (
                  <li key={s.id} className="truncate">
                    {s.slug || s.id.slice(0, 8)} · {projectName(s)} · {fmtBytes(s.bytes)}
                  </li>
                ))}
                {selectedRows.length > 12 && (
                  <li className="text-muted">
                    {t("… 还有 {n} 个", { n: selectedRows.length - 12 })}
                  </li>
                )}
              </ul>
            </div>
          }
          confirmText={t("删除选中")}
          pending={remove.isPending}
          error={remove.error}
          onConfirm={() => remove.mutate(selectedRows)}
          onCancel={() => {
            if (!remove.isPending) setConfirming(false);
          }}
        />
      )}
    </div>
  );
}
