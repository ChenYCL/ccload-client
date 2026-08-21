import type { Translate } from "../../i18n";
/// 表单草稿 ↔ `ExtensionSpec` 的转换。表单里所有字段都是「有值」的（字符串用
/// 空串、数字用文本），提交时才按 kind 裁成后端要的形状 —— 给 MCP 发一个 body
/// 或者给 Skill 发一个 transport 都只会让配置文件里多出没人看的字段。

import type { ExtensionKind, ExtensionSpec, HookEvent, McpTransport } from "../../types";

export type SpecDraft = {
  id: string;
  description: string;
  transport: McpTransport;
  command: string;
  args: string[];
  env: Record<string, string>;
  url: string;
  headers: Record<string, string>;
  enabled: boolean;
  body: string;
  event: HookEvent;
  matcher: string;
  hookCommand: string;
  /** 文本而不是 number：空串要能表达「不设置超时」，0 和空是两回事。 */
  timeout: string;
};

export const EMPTY_DRAFT: SpecDraft = {
  id: "",
  description: "",
  transport: "stdio",
  command: "",
  args: [],
  env: {},
  url: "",
  headers: {},
  enabled: true,
  body: "",
  event: "PreToolUse",
  matcher: "",
  hookCommand: "",
  timeout: "",
};

export function draftFromSpec(spec: ExtensionSpec): SpecDraft {
  return {
    ...EMPTY_DRAFT,
    id: spec.id,
    description: spec.description ?? "",
    transport: spec.transport ?? "stdio",
    command: spec.command ?? "",
    args: spec.args ?? [],
    env: spec.env ?? {},
    url: spec.url ?? "",
    headers: spec.headers ?? {},
    // null 表示配置里没写 enabled，那就是启用状态。
    enabled: spec.enabled ?? true,
    body: spec.body ?? "",
    event: spec.event ?? "PreToolUse",
    matcher: spec.matcher ?? "",
    hookCommand: spec.hookCommand ?? "",
    timeout: spec.timeout != null ? String(spec.timeout) : "",
  };
}

/// 空 key 的行是编辑过程中的正常中间态（用户还没敲字），提交时丢掉。
function cleanMap(m: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(m).filter(([k]) => k.trim() !== ""));
}

export function draftToSpec(draft: SpecDraft, kind: ExtensionKind): ExtensionSpec {
  const description = draft.description.trim();
  switch (kind) {
    case "mcp":
      return {
        id: draft.id.trim(),
        description: description || null,
        transport: draft.transport,
        enabled: draft.enabled,
        ...(draft.transport === "stdio"
          ? {
              command: draft.command.trim(),
              args: draft.args.filter((a) => a.trim() !== ""),
              env: cleanMap(draft.env),
            }
          : { url: draft.url.trim(), headers: cleanMap(draft.headers) }),
      };
    case "skill":
    case "agent":
      return {
        id: draft.id.trim(),
        description: description || null,
        body: draft.body,
      };
    case "hook":
      return {
        // hook 在配置文件里没有名字，后端的 id 是「事件 + 命令哈希」推导出来
        // 的，write_hook 根本不读 spec.id —— 但 validate 要求它非空，所以新建
        // 时拿事件名顶上，编辑时用读回来的真 id。
        id: draft.id.trim() || draft.event,
        event: draft.event,
        matcher: draft.matcher.trim() || null,
        hookCommand: draft.hookCommand.trim(),
        timeout: draft.timeout.trim() ? Number(draft.timeout) : null,
      };
  }
}

/// 前端预校验，规则照抄后端 `validate` —— 这里拦下来只是为了让「保存」按钮
/// 能提前置灰并说明缺什么，真正的把关仍然在后端。
export function draftProblem(
  draft: SpecDraft,
  kind: ExtensionKind,
  t: Translate,
): string | null {
  if (kind !== "hook") {
    const id = draft.id.trim();
    if (!id) return t("名称不能为空");
    if (/[/\\]/.test(id) || id.includes("..") || id.startsWith("."))
      return t("名称不能包含 / \\ .. 或以 . 开头");
  }
  switch (kind) {
    case "mcp":
      if (draft.transport === "stdio" && !draft.command.trim())
        return t("stdio 类型的 MCP 必须填 command");
      if (draft.transport === "http" && !draft.url.trim())
        return t("http 类型的 MCP 必须填 url");
      return null;
    case "skill":
    case "agent":
      if (!draft.body.trim()) return t("正文（markdown）不能为空");
      return null;
    case "hook":
      if (!draft.hookCommand.trim()) return t("必须填要执行的命令");
      if (draft.timeout.trim() && !/^\d+$/.test(draft.timeout.trim()))
        return t("超时必须是非负整数（秒）");
      return null;
  }
}
