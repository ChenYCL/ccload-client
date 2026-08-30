import { useT } from "../i18n";
import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Play, Plus, RefreshCw, Server, Square, Trash2 } from "lucide-react";
import { api } from "../lib/api";
import { cn } from "../lib/cn";
import { errText } from "../lib/err";
import type { NodeService, NodeServiceStatus } from "../types";
import { AsyncBlock, Panel } from "../components/StateBlock";
import { Modal } from "../components/Modal";
import { TextInput } from "../components/ui/Input";
import { TEMPLATES } from "../lib/serviceTemplates";

/// 托管用户自己的 Node 常驻服务。
///
/// 用途窄而明确：**需要一个活着的端口**的东西。MCP over http/sse、自定义后端。
/// stdio 型 MCP 不用放这里 —— CLI 自己会拉起进程，托管它反而多一份僵尸风险。
///
/// 端口由用户填而不是我们随机分配：MCP 配置、CLI 配置里都要写死这个地址，
/// 随机端口等于每次重启都要重写一遍配置。

const POLL_MS = 3_000;

const TONE: Record<NodeServiceStatus["state"], { dot: string; label: string }> = {
  running: { dot: "bg-emerald-500", label: "运行中" },
  // 端口 listen 了不代表服务能用 —— 这两个状态必须分开显示。
  unhealthy: { dot: "bg-amber-500", label: "健康检查未通过" },
  exited: { dot: "bg-rose-500", label: "已退出" },
  stopped: { dot: "bg-muted/40", label: "未运行" },
};

function blank(): NodeService {
  return {
    id: "",
    entry: "",
    args: [],
    cwd: null,
    port: 4000,
    health_path: "/health",
    env: {},
    enabled: true,
  };
}

export function NodeServicesPage() {
  const t = useT();
  const qc = useQueryClient();
  const [editing, setEditing] = useState<NodeService | null>(null);
  const [editingScript, setEditingScript] = useState<string | undefined>(undefined);
  const [picking, setPicking] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const list = useQuery({ queryKey: ["node-services"], queryFn: api.nodeServiceList });
  const status = useQuery({
    queryKey: ["node-service-status"],
    queryFn: api.nodeServiceStatus,
    refetchInterval: POLL_MS,
    placeholderData: (prev) => prev,
  });

  const statusOf = useMemo(() => {
    const m = new Map<string, NodeServiceStatus>();
    for (const s of status.data ?? []) m.set(s.id, s);
    return m;
  }, [status.data]);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["node-services"] });
    qc.invalidateQueries({ queryKey: ["node-service-status"] });
  };

  const save = useMutation({
    mutationFn: async ({ s, script }: { s: NodeService; script: string }) => {
      // 模板带来的脚本先落盘（已存在则轮新名字，不覆盖旧文件），再把路径填进 entry。
      let target = s;
      if (script.trim() && !s.entry.trim()) {
        const path = await api.nodeServiceWriteScript(s.id, script);
        target = { ...s, entry: path };
      }
      return api.nodeServiceSave(target);
    },
    onSuccess: () => {
      setEditing(null);
      setMessage(null);
      refresh();
    },
    onError: (e) => setMessage(errText(e)),
  });
  const del = useMutation({
    mutationFn: (id: string) => api.nodeServiceDelete(id),
    onSuccess: refresh,
    onError: (e) => setMessage(errText(e)),
  });
  const start = useMutation({
    mutationFn: (id: string) => api.nodeServiceStart(id),
    onSuccess: () => {
      setMessage(null);
      refresh();
    },
    // 起不来的原因（端口占用、脚本报错、健康检查超时）是这页最有价值的信息。
    onError: (e) => setMessage(errText(e)),
  });
  const stop = useMutation({
    mutationFn: (id: string) => api.nodeServiceStop(id),
    onSuccess: refresh,
    onError: (e) => setMessage(errText(e)),
  });

  const services = list.data ?? [];

  return (
    <div className="space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="t-display">{t("Node 服务")}</h1>
          <p className="mt-0.5 max-w-3xl text-sm text-muted">
            {t(
              "托管你自己的常驻 Node 服务：MCP over http/sse、自定义后端。跑的是 node <入口脚本>，端口通过 PORT 环境变量传给它。stdio 型 MCP 不用放这里 —— CLI 自己会拉起进程。",
            )}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            onClick={refresh}
            className="flex items-center gap-1.5 rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 text-xs hover:bg-surface-2"
          >
            <RefreshCw className={cn("h-3.5 w-3.5", status.isFetching && "animate-spin")} />
            {t("刷新")}
          </button>
          <button
            onClick={() => setPicking(true)}
            className="flex items-center gap-1 rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-accent/90"
          >
            <Plus className="h-4 w-4" />
            {t("新建服务")}
          </button>
        </div>
      </header>

      <div className="rounded-xl border border-border bg-surface-2/40 px-3.5 py-3 text-xs">
        <div className="font-medium">{t("每个服务会自动收到这些环境变量")}</div>
        <ul className="mt-1 space-y-0.5 font-mono text-muted">
          <li>CCLOAD_BASE_URL —— {t("CLI 该走的入口（代理开着指代理，否则指内核）")}</li>
          <li>CCLOAD_API_TOKEN —— {t("配套凭据；换内核/轮换后自动跟随，不用改脚本")}</li>
          <li>CCLOAD_CLIENT_BIN —— {t("本客户端二进制路径（想直接调生图 MCP 时用）")}</li>
        </ul>
        <p className="mt-1.5 text-amber-700">
          {t(
            "凭据只注入到运行中的服务进程：托管的脚本（以及它再启动的东西）都能读到。只托管你自己写的或信得过的脚本；服务配置文件里不存凭据。",
          )}
        </p>
      </div>

      {message && (
        <div className="card whitespace-pre-wrap bg-surface-raised px-4 py-3 text-sm text-rose-700">
          {message}
        </div>
      )}

      <Panel title={t("已配置的服务")} hint={t("状态每 3s 刷新一次")}>
        <AsyncBlock
          isLoading={list.isPending}
          error={list.error}
          isEmpty={services.length === 0}
          emptyText={t("还没有服务")}
          emptyHint={t("点右上角「新建服务」加一个。需要一个常驻端口的东西才放这里。")}
        >
          <div className="space-y-2">
            {services.map((s) => {
              const st = statusOf.get(s.id);
              const tone = TONE[st?.state ?? "stopped"];
              const live = st?.state === "running" || st?.state === "unhealthy";
              return (
                <div
                  key={s.id}
                  className="flex flex-wrap items-center gap-3 rounded-xl border border-border bg-surface-2/40 px-3 py-2.5"
                >
                  <span className={cn("h-2 w-2 shrink-0 rounded-full", tone.dot)} />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-baseline gap-2">
                      <span className="truncate font-medium">{s.id}</span>
                      <span className="font-mono text-xs text-muted">:{s.port}</span>
                      {!s.enabled && (
                        <span className="rounded bg-surface-raised px-1.5 py-px text-[11px] text-muted">
                          {t("不随启动")}
                        </span>
                      )}
                    </div>
                    <div className="truncate font-mono text-xs text-muted" title={s.entry}>
                      {s.entry}
                    </div>
                    <div className="text-xs text-muted">
                      {t(tone.label)}
                      {st?.message ? ` · ${st.message}` : ""}
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    {live ? (
                      <button
                        onClick={() => stop.mutate(s.id)}
                        className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
                      >
                        <Square className="h-3.5 w-3.5" />
                        {t("停止")}
                      </button>
                    ) : (
                      <button
                        onClick={() => start.mutate(s.id)}
                        disabled={start.isPending}
                        className="flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
                      >
                        <Play className="h-3.5 w-3.5" />
                        {t("启动")}
                      </button>
                    )}
                    <button
                      onClick={() => setEditing(s)}
                      className="rounded-lg border border-border bg-surface-raised px-2.5 py-1 text-xs hover:bg-surface-2"
                    >
                      {t("编辑")}
                    </button>
                    <button
                      onClick={() => del.mutate(s.id)}
                      title={t("删除并停止")}
                      className="rounded-lg border border-border bg-surface-raised p-1.5 text-muted hover:bg-surface-2 hover:text-rose-600"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        </AsyncBlock>
      </Panel>

      {picking && (
        <TemplatePicker
          onClose={() => setPicking(false)}
          onPick={(tpl) => {
            if (!tpl) {
              setEditing(blank());
              setEditingScript(undefined);
              return;
            }
            const base = blank();
            setEditing({ ...base, id: tpl.id, port: tpl.port });
            setEditingScript(tpl.script);
          }}
        />
      )}

      {editing && (
        <ServiceEditor
          initial={editing}
          existing={services}
          initialScript={editingScript}
          onCancel={() => setEditing(null)}
          onSave={(s, script) => save.mutate({ s, script })}
          saving={save.isPending}
        />
      )}
    </div>
  );
}

function ServiceEditor({
  initial,
  existing,
  initialScript,
  onCancel,
  onSave,
  saving,
}: {
  initial: NodeService;
  existing: NodeService[];
  /** 从模板进入时预填的脚本内容；空白服务为空。 */
  initialScript?: string;
  onCancel: () => void;
  onSave: (s: NodeService, script: string) => void;
  saving: boolean;
}) {
  const t = useT();
  const isNew = !existing.some((s) => s.id === initial.id);
  const [draft, setDraft] = useState<NodeService>(initial);
  const [scriptText, setScriptText] = useState(initialScript ?? "");
  // 环境变量按「一行一个 K=V」编辑 —— 比一堆输入框好用，也好粘贴。
  const [envText, setEnvText] = useState(
    Object.entries(initial.env ?? {})
      .map(([k, v]) => `${k}=${v}`)
      .join("\n"),
  );

  const idTaken = isNew && existing.some((s) => s.id === draft.id.trim());
  const portTaken = existing.some(
    (s) => s.id !== initial.id && s.port === draft.port,
  );
  const problem = !draft.id.trim()
    ? t("名字不能为空")
    : idTaken
      ? t("这个名字已经用过了")
      : !draft.entry.trim()
        ? t("入口脚本路径不能为空")
        : draft.port < 1
          ? t("端口不合法")
          : portTaken
            ? t("这个端口已经被另一条服务占了")
            : null;

  const commit = () => {
    const env: Record<string, string> = {};
    for (const line of envText.split("\n")) {
      const s = line.trim();
      if (!s || s.startsWith("#")) continue;
      const eq = s.indexOf("=");
      if (eq <= 0) continue;
      env[s.slice(0, eq).trim()] = s.slice(eq + 1).trim();
    }
    onSave(
      {
        ...draft,
        id: draft.id.trim(),
        entry: draft.entry.trim(),
        cwd: draft.cwd?.trim() || null,
        env,
      },
      scriptText,
    );
  };

  return (
    <Modal onClose={onCancel}>
      <div className="space-y-3">
        <h2 className="flex items-center gap-2 t-title">
          <Server className="h-4 w-4" />
          {isNew ? t("新建服务") : t("编辑 {id}", { id: initial.id })}
        </h2>
        <Field label={t("名字")} hint={t("列表和日志里的标识，建好之后别改")}>
          <TextInput
            value={draft.id}
            disabled={!isNew}
            onChange={(e) => setDraft({ ...draft, id: e.target.value })}
            placeholder="my-mcp"
          />
        </Field>

        <Field label={t("入口脚本")} hint={t("绝对路径，跑的是 node <这个文件>")}>
          <TextInput
            value={draft.entry}
            onChange={(e) => setDraft({ ...draft, entry: e.target.value })}
            placeholder="/Users/me/srv/index.js"
          />
        </Field>

        <Field
          label={t("端口")}
          hint={t("以 PORT 环境变量传给脚本。MCP/CLI 配置里写死的就是它，所以由你定。")}
        >
          <TextInput
            type="number"
            value={String(draft.port)}
            onChange={(e) => setDraft({ ...draft, port: Number(e.target.value) || 0 })}
          />
        </Field>

        <Field
          label={t("健康检查路径")}
          hint={t("留空表示不检查，进程起来就算成功。默认 /health —— 端口 listen 了不代表服务能用。")}
        >
          <TextInput
            value={draft.health_path ?? ""}
            onChange={(e) => setDraft({ ...draft, health_path: e.target.value })}
            placeholder="/health"
          />
        </Field>

        <Field label={t("工作目录")} hint={t("留空则取入口脚本所在目录，脚本里的相对 require 才找得到")}>
          <TextInput
            value={draft.cwd ?? ""}
            onChange={(e) => setDraft({ ...draft, cwd: e.target.value })}
            placeholder={t("（跟随入口脚本）")}
          />
        </Field>

        {isNew && (
          <Field
            label={t("入口脚本内容")}
            hint={t("从模板进来的骨架可以直接改；保存时会写入下面的入口路径。")}
          >
            <textarea
              value={scriptText}
              onChange={(e) => setScriptText(e.target.value)}
              rows={12}
              spellCheck={false}
              className="w-full rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 font-mono text-xs"
            />
          </Field>
        )}

        <Field
          label={t("环境变量")}
          hint={t(
            "一行一个 KEY=VALUE。PORT、CCLOAD_BASE_URL、CCLOAD_API_TOKEN 由平台自动带上；同名变量写在这里可覆盖平台值。",
          )}
        >
          <textarea
            value={envText}
            onChange={(e) => setEnvText(e.target.value)}
            rows={4}
            spellCheck={false}
            className="w-full rounded-lg border border-border bg-surface-raised px-2.5 py-1.5 font-mono text-xs"
            placeholder={"CCLOAD_URL=http://127.0.0.1:15777\nTOKEN=…"}
          />
        </Field>

        <label className="flex cursor-pointer items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={draft.enabled ?? true}
            onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })}
          />
          {t("随客户端启动")}
        </label>

        {problem && <p className="text-xs text-rose-600">{problem}</p>}

        <div className="flex justify-end gap-2 pt-1">
          <button
            onClick={onCancel}
            className="rounded-lg border border-border bg-surface-raised px-3 py-1.5 text-sm hover:bg-surface-2"
          >
            {t("取消")}
          </button>
          <button
            onClick={commit}
            disabled={!!problem || saving}
            className="rounded-lg bg-accent px-3.5 py-1.5 text-sm font-medium text-white hover:bg-accent/90 disabled:opacity-40"
          >
            {saving ? t("保存中…") : t("保存")}
          </button>
        </div>
      </div>
    </Modal>
  );
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <label className="block">
      <span className="text-sm font-medium">{label}</span>
      {hint && <span className="mt-0.5 block text-xs text-muted">{hint}</span>}
      <div className="mt-1">{children}</div>
    </label>
  );
}

/// 模板选择弹窗。
///
/// 新建的第一步不是空表单而是模板：三类骨架覆盖托管服务的绝大多数用途，
/// 选完直接进编辑器，脚本可改、保存时自动落盘并填好 entry。空白服务放在
/// 列表最后，仍然可选 —— 模板是加速，不是围墙。
function TemplatePicker({
  onPick,
  onClose,
}: {
  onPick: (tpl: { id: string; port: number; script: string } | null) => void;
  onClose: () => void;
}) {
  const t = useT();
  return (
    <Modal onClose={onClose}>
      <div className="space-y-3">
        <h2 className="t-title">{t("从模板新建")}</h2>
        <div className="space-y-2">
          {TEMPLATES.map((tpl) => (
            <button
              key={tpl.id}
              onClick={() => {
                onPick({ id: tpl.id, port: tpl.port, script: tpl.script });
                onClose();
              }}
              className="block w-full rounded-xl border border-border bg-surface-2/40 px-3.5 py-3 text-left hover:bg-surface-2"
            >
              <div className="flex items-baseline gap-2">
                <span className="font-medium">{t(tpl.label)}</span>
                <span className="font-mono text-xs text-muted">:{tpl.port}</span>
              </div>
              <p className="mt-0.5 text-xs text-muted">{t(tpl.description)}</p>
            </button>
          ))}
          <button
            onClick={() => {
              onPick(null);
              onClose();
            }}
            className="block w-full rounded-xl border border-dashed border-border px-3.5 py-2.5 text-left text-sm text-muted hover:bg-surface-2"
          >
            {t("空白服务（自己写脚本）")}
          </button>
        </div>
      </div>
    </Modal>
  );
}
