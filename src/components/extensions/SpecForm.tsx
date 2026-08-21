import { useT } from "../../i18n";
/// 按 kind 换一套字段的表单。四类扩展在配置文件里的形状差得很远，硬做成一个
/// 通用键值编辑器只会让每一类都难填 —— 所以这里就是四个分支。

import type { ExtensionKind } from "../../types";
import { HOOK_EVENTS } from "./model";
import type { SpecDraft } from "./spec";
import { Field, KeyValueRows, StringListRows } from "./fields";
import { Select, TextArea, TextInput } from "../ui/Input";

export function SpecForm(props: {
  kind: ExtensionKind;
  draft: SpecDraft;
  onChange: (d: SpecDraft) => void;
  /** 编辑已装条目时名称就是主键，改名等于新建，所以锁死。 */
  idLocked: boolean;
}) {
  const t = useT();
  const { draft, kind } = props;
  const set = (patch: Partial<SpecDraft>) => props.onChange({ ...draft, ...patch });

  return (
    <div className="space-y-4">
      {kind !== "hook" && (
        <Field
          label={t("名称（id）")}
          required
          hint={
            kind === "mcp"
              ? t("配置里的服务器名")
              : kind === "skill"
                ? t("技能目录名")
                : t("文件名（不含 .md）")
          }
        >
          <TextInput
            mono
            value={draft.id}
            disabled={props.idLocked}
            onChange={(e) => set({ id: e.target.value })}
            placeholder="my-extension"
          />
        </Field>
      )}

      {kind !== "hook" && (
        <Field label={t("描述")} hint={t("可留空")}>
          <TextInput
            value={draft.description}
            onChange={(e) => set({ description: e.target.value })}
            placeholder={t("一句话说明它是干什么的")}
          />
        </Field>
      )}

      {kind === "mcp" && <McpFields draft={draft} set={set} />}
      {(kind === "skill" || kind === "agent") && (
        <Field
          label={t("正文（markdown）")}
          required
          hint={t("开头带 --- frontmatter 就原样写入，否则用名称 + 描述合成一份最小的")}
        >
          <TextArea
            mono
            value={draft.body}
            onChange={(e) => set({ body: e.target.value })}
            rows={14}
            placeholder={t("---\nname: my-skill\ndescription: …\n---\n\n正文…")}
            className="leading-relaxed"
          />
        </Field>
      )}
      {kind === "hook" && <HookFields draft={draft} set={set} />}
    </div>
  );
}

function McpFields({
  draft,
  set,
}: {
  draft: SpecDraft;
  set: (p: Partial<SpecDraft>) => void;
}) {
  const t = useT();
  return (
    <>
      <div>
        <div className="text-xs font-medium text-content">{t("传输方式")}</div>
        {/* 后端的 McpTransport 只有 stdio / http 两个变体 —— SSE 类型的服务器
            也走 http 这一支，填它的 URL 即可。 */}
        <div className="mt-1 flex gap-2">
          {(["stdio", "http"] as const).map((tr) => (
            <button
              key={tr}
              type="button"
              onClick={() => set({ transport: tr })}
              aria-pressed={draft.transport === tr}
              className={
                draft.transport === tr
                  ? "rounded-lg bg-accent/10 px-3 py-1.5 text-xs font-medium text-accent"
                  : "rounded-lg border border-border px-3 py-1.5 text-xs text-muted hover:bg-surface-2"
              }
            >
              {tr === "stdio" ? t("stdio（本地进程）") : t("http / sse（远端 URL）")}
            </button>
          ))}
        </div>
      </div>

      {draft.transport === "stdio" ? (
        <>
          <Field label="command" required hint={t("可执行文件，如 npx")}>
            <TextInput
              mono
              value={draft.command}
              onChange={(e) => set({ command: e.target.value })}
              placeholder="npx"
            />
          </Field>
          <StringListRows
            label="args"
            hint={t("一行一个参数，顺序即命令行顺序")}
            placeholder="-y"
            value={draft.args}
            onChange={(args) => set({ args })}
          />
          <KeyValueRows
            label="env"
            hint={t("传给该进程的环境变量")}
            value={draft.env}
            onChange={(env) => set({ env })}
          />
        </>
      ) : (
        <>
          <Field label="url" required>
            <TextInput
              mono
              value={draft.url}
              onChange={(e) => set({ url: e.target.value })}
              placeholder="https://example.com/mcp"
            />
          </Field>
          <KeyValueRows
            label="headers"
            hint={t("如 Authorization")}
            value={draft.headers}
            onChange={(headers) => set({ headers })}
          />
        </>
      )}

      <label className="flex items-center gap-2 text-xs">
        <input
          type="checkbox"
          checked={draft.enabled}
          onChange={(e) => set({ enabled: e.target.checked })}
          className="h-3.5 w-3.5 accent-accent"
        />
        <span>{t("启用（取消勾选会把 enabled: false 写进配置）")}</span>
      </label>
    </>
  );
}

function HookFields({
  draft,
  set,
}: {
  draft: SpecDraft;
  set: (p: Partial<SpecDraft>) => void;
}) {
  const t = useT();
  return (
    <>
      <Field
        label="event"
        required
        hint={t("规范事件名，写入时按目标 CLI 翻译（Gemini 的 PreToolUse 叫 BeforeTool）")}
      >
        <Select
          className="w-full"
          value={draft.event}
          onChange={(e) => set({ event: e.target.value as SpecDraft["event"] })}
        >
          {HOOK_EVENTS.map((ev) => (
            <option key={ev} value={ev}>
              {ev}
            </option>
          ))}
        </Select>
      </Field>
      <Field label="matcher" hint={t("匹配哪些工具，如 Bash|Write；留空等于全部")}>
        <TextInput
          mono
          value={draft.matcher}
          onChange={(e) => set({ matcher: e.target.value })}
          placeholder="*"
        />
      </Field>
      <Field label={t("命令")} required hint={t("事件触发时执行的 shell 命令")}>
        <TextArea
          mono
          value={draft.hookCommand}
          onChange={(e) => set({ hookCommand: e.target.value })}
          rows={3}
          placeholder="~/.claude/hooks/lint.sh"
        />
      </Field>
      <Field label={t("超时（秒）")} hint={t("留空用 CLI 默认值")}>
        <TextInput
          value={draft.timeout}
          onChange={(e) => set({ timeout: e.target.value })}
          inputMode="numeric"
          placeholder="60"
          className="w-32"
        />
      </Field>
      {/* 后端按「事件 + 命令」定位一条 hook，改了命令就是另一条，旧的不会被
          带走 —— 与其事后解释，不如在这里先讲清楚。 */}
      <p className="rounded-lg bg-surface-2 px-3 py-2 text-[11px] text-muted">
        hook 在配置里没有名字，后端用「事件 + 命令」认它。改动命令等于新增一条，
        原来那条要回列表里单独删除。
      </p>
    </>
  );
}
