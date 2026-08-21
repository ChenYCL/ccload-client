import { useT } from "../../i18n";
/// 表单原子。MCP 的 args/env/headers 都是「一堆可增删的行」，Skill/Agent 是大
/// 段 markdown，Hook 是几个单值 —— 共用的只有外观，所以这里只抽外观。

import type { ReactNode } from "react";
import { Plus, X } from "lucide-react";

import { TextInput } from "../ui/Input";

export function Field(props: {
  label: string;
  hint?: string;
  required?: boolean;
  children: ReactNode;
}) {
  const t = useT();
  return (
    <label className="block">
      <div className="flex items-baseline gap-1.5">
        <span className="text-xs font-medium text-content">{props.label}</span>
        {props.required && <span className="text-xs text-accent">{t("必填")}</span>}
        {props.hint && <span className="text-[11px] text-muted">{props.hint}</span>}
      </div>
      <div className="mt-1">{props.children}</div>
    </label>
  );
}

/// 键值对编辑器（MCP 的 env / headers）。空 key 的行允许存在，提交前过滤掉，
/// 否则用户敲第一个字符之前那一行就会被自己删掉。
export function KeyValueRows(props: {
  label: string;
  hint?: string;
  value: Record<string, string>;
  onChange: (v: Record<string, string>) => void;
}) {
  const t = useT();
  const rows = Object.entries(props.value);

  const rename = (oldKey: string, newKey: string) => {
    const next: Record<string, string> = {};
    for (const [k, v] of rows) next[k === oldKey ? newKey : k] = v;
    props.onChange(next);
  };

  return (
    <div>
      <div className="flex items-baseline gap-1.5">
        <span className="text-xs font-medium text-content">{props.label}</span>
        {props.hint && <span className="text-[11px] text-muted">{props.hint}</span>}
      </div>
      <div className="mt-1 space-y-1">
        {rows.map(([k, v], i) => (
          <div key={i} className="flex items-center gap-2">
            <TextInput
              mono
              value={k}
              onChange={(e) => rename(k, e.target.value)}
              placeholder="KEY"
              className="w-[42%] shrink-0"
            />
            <TextInput
              mono
              value={v}
              onChange={(e) => props.onChange({ ...props.value, [k]: e.target.value })}
              placeholder="value"
              className="flex-1"
            />
            <button
              type="button"
              aria-label={`删除 ${k || t("这一行")}`}
              onClick={() => {
                const next = { ...props.value };
                delete next[k];
                props.onChange(next);
              }}
              className="rounded-md border border-border p-1.5 text-muted hover:bg-surface-2 hover:text-red-600"
            >
              <X className="h-3 w-3" />
            </button>
          </div>
        ))}
      </div>
      <button
        type="button"
        onClick={() => props.onChange({ ...props.value, "": "" })}
        disabled={Object.keys(props.value).includes("")}
        className="mt-1.5 flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2 disabled:opacity-40"
      >
        <Plus className="h-3 w-3" />
        {t("添加一行")}
      </button>
    </div>
  );
}

/// 有序字符串列表（MCP 的 args）。顺序有意义，所以用下标而不是内容做 key。
export function StringListRows(props: {
  label: string;
  hint?: string;
  placeholder?: string;
  value: string[];
  onChange: (v: string[]) => void;
}) {
  const t = useT();
  return (
    <div>
      <div className="flex items-baseline gap-1.5">
        <span className="text-xs font-medium text-content">{props.label}</span>
        {props.hint && <span className="text-[11px] text-muted">{props.hint}</span>}
      </div>
      <div className="mt-1 space-y-1">
        {props.value.map((v, i) => (
          <div key={i} className="flex items-center gap-2">
            <span className="w-5 shrink-0 text-right font-mono text-[10px] text-muted">
              {i + 1}
            </span>
            <TextInput
              mono
              value={v}
              onChange={(e) => {
                const next = [...props.value];
                next[i] = e.target.value;
                props.onChange(next);
              }}
              placeholder={props.placeholder}
              className="flex-1"
            />
            <button
              type="button"
              aria-label={t("删除第 {n} 个参数", { n: i + 1 })}
              onClick={() => props.onChange(props.value.filter((_, j) => j !== i))}
              className="rounded-md border border-border p-1.5 text-muted hover:bg-surface-2 hover:text-red-600"
            >
              <X className="h-3 w-3" />
            </button>
          </div>
        ))}
      </div>
      <button
        type="button"
        onClick={() => props.onChange([...props.value, ""])}
        className="mt-1.5 flex items-center gap-1 rounded-lg border border-border bg-surface-raised px-2 py-1 text-xs hover:bg-surface-2"
      >
        <Plus className="h-3 w-3" />
        {t("添加参数")}
      </button>
    </div>
  );
}

/// 写盘结果：install 返回被写入的文件，skill/agent 覆盖时旧版本会以
/// 「…（已归档）」出现在同一个数组里，所以整串原样列出来最诚实。
export function WrittenFiles({ files }: { files: string[] }) {
  if (files.length === 0) return null;
  return (
    <ul className="mt-1 space-y-0.5">
      {files.map((f, i) => (
        <li key={i} className="break-all font-mono text-[10px] opacity-80">
          {f}
        </li>
      ))}
    </ul>
  );
}
