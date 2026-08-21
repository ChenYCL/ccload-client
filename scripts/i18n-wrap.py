#!/usr/bin/env python3
"""把界面上的中文字面量包进 t(...)，并给需要的组件补上 useT()。

为什么用脚本而不是手改：这一轮有 200 多处，手改必然漏，而漏掉的那几处正好是
「中英文没分离全」的下一次 bug 报告。

安全边界（踩过的坑都写在这）：

  * **只改顶层组件里的行**。`t` 是 `useT()` 在组件里拿到的；模块作用域的常量表
    （TIER_LABELS / GROUPS / TOOL_LABELS）和小写命名的纯函数
    （visionBatch / hopVerdict / windowLabel）都没有 `t`。前者本来就是在使用处
    翻译的（`t(group.title)`），在定义处再包一层既编译不过、也把已经正确的做法
    改坏了。判据用 React 惯例：**首字母大写 = 组件**。
  * **`t(` 可能跨行**。`t(\\n  "中文",\\n)` 这种写法里，字面量那一行看不到
    `t(`，按单行判断会包成 `t(t("中文"))`。所以回看上一行。
  * 注释、import、类型定义一律不碰。

用法：python3 scripts/i18n-wrap.py [--write]
"""
import re
import sys
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
HAS_HAN = re.compile(r"[一-鿿]")
TARGET_DIRS = ["src", "src/pages", "src/components"]

# 顶层**函数**声明。首字母大写视为组件（React 惯例，本仓库全线遵守）。
#
# `const` 那一支必须能看出是函数（`= (` 或箭头），否则
# `const TOOL_LABELS: Record<string, string> = {` 这种大写常量表也会被当成组件，
# hook 就被插进对象字面量里了 —— 报一串 TS1005。
TOP_FUNC = re.compile(
    r"^(export\s+)?(?:function\s+(\w+)"
    r"|const\s+(\w+)\s*(?::[^=]*)?=\s*(?:function\b|\(|forwardRef|memo\b|async\b))"
)


def component_lines(lines):
    """返回「位于顶层组件函数体内」的行号集合（0-based）。

    用花括号配平找函数体范围。字符串里的花括号会让配平偏，但这些文件里
    JSX 的花括号远多于字符串里的，实测足够；判错的后果也只是漏改一行，
    由 typecheck 兜底。
    """
    inside = set()
    i = 0
    while i < len(lines):
        m = TOP_FUNC.match(lines[i])
        name = m and (m.group(2) or m.group(3))
        if not (name and name[0].isupper()):
            i += 1
            continue
        # 从声明行开始配平
        depth = 0
        started = False
        j = i
        while j < len(lines):
            depth += lines[j].count("{") - lines[j].count("}")
            if "{" in lines[j]:
                started = True
            if started and depth <= 0:
                break
            j += 1
        inside.update(range(i, min(j + 1, len(lines))))
        i = j + 1
    return inside


def wrap_attr(line):
    def sub(m):
        name, val = m.group(1), m.group(3)
        if not HAS_HAN.search(val):
            return m.group(0)
        return f'{name}={{t("{val}")}}'

    return re.sub(r'\b([A-Za-z][A-Za-z0-9]*)=(")((?:[^"\\]|\\.)*)"', sub, line)


def wrap_literals(line, prev_ends_with_t_open):
    out, i = [], 0
    while i < len(line):
        ch = line[i]
        if ch not in "\"'":
            out.append(ch)
            i += 1
            continue
        q, j = ch, i + 1
        while j < len(line):
            if line[j] == "\\":
                j += 2
                continue
            if line[j] == q:
                break
            j += 1
        if j >= len(line):
            out.append(line[i:])
            break
        val = line[i + 1 : j]
        prefix = "".join(out)
        # 已经是 t("…")：同一行里 t( 紧贴，或上一行以 t( 结尾（跨行写法）
        already = re.search(r"\bt\(\s*$", prefix) or (
            prev_ends_with_t_open and not prefix.strip()
        )
        if HAS_HAN.search(val) and not already:
            out.append(f"t({q}{val}{q})")
        else:
            out.append(line[i : j + 1])
        i = j + 1
    return "".join(out)


def wrap_jsx_text(line):
    def sub(m):
        lead, text, tail = m.group(1), m.group(2), m.group(3)
        if not HAS_HAN.search(text) or any(c in text for c in "{}<"):
            return m.group(0)
        stripped = text.strip()
        pre = text[: len(text) - len(text.lstrip())]
        post = text[len(text.rstrip()) :]
        return f'{lead}{pre}{{t("{stripped}")}}{post}{tail}'

    return re.sub(r"(>)([^<>{}]*[一-鿿][^<>{}]*)(<)", sub, line)


def ensure_hook(lines, path):
    """给用了 t 却没声明 t 的顶层组件补 `const t = useT();`，并补 import。"""
    changed = False
    i = 0
    while i < len(lines):
        m = TOP_FUNC.match(lines[i])
        name = m and (m.group(2) or m.group(3))
        if not (name and name[0].isupper()):
            i += 1
            continue
        depth, started, j = 0, False, i
        while j < len(lines):
            depth += lines[j].count("{") - lines[j].count("}")
            if "{" in lines[j]:
                started = True
            if started and depth <= 0:
                break
            j += 1
        body = lines[i : j + 1]
        text = "\n".join(body)
        if re.search(r"\bt\(", text) and "useT()" not in text:
            # 找**函数体**的那个 `{`，不是参数解构或 props 类型字面量的 `{`。
            # 判据：圆括号已经配平（参数表结束）之后出现的第一个 `{`。
            # 之前按「第一个 `{`」找，结果把 hook 插进了 props 的类型标注里，
            # 报出一串 TS1131 —— 这一步没有捷径。
            paren, k, body = 0, i, None
            while k <= j:
                for ch in lines[k]:
                    if ch == "(":
                        paren += 1
                    elif ch == ")":
                        paren -= 1
                    elif ch == "{" and paren == 0:
                        body = k
                        break
                if body is not None:
                    break
                k += 1
            if body is None:
                i = j + 1
                continue
            indent = " " * (len(lines[i]) - len(lines[i].lstrip()) + 2)
            lines.insert(body + 1, f"{indent}const t = useT();")
            changed = True
            j += 1
        i = j + 1

    if changed:
        src = "\n".join(lines)
        if "useT" not in src.split("\n\n")[0] and 'from "../i18n"' not in src and 'from "./i18n"' not in src and 'from "../../i18n"' not in src:
            depth = len(path.relative_to(ROOT / "src").parts) - 1
            rel = "./i18n" if depth == 0 else "../" * depth + "i18n"
            # 插在文件最前面。原先按「最后一条以 import 开头的行」找，
            # 而多行 import 的续行并不以 import 开头 —— 于是插进了语句中间。
            lines.insert(0, f'import {{ useT }} from "{rel}";')
    return changed


def process(path):
    src = path.read_text(encoding="utf-8")
    lines = src.split("\n")
    allowed = component_lines(lines)
    in_block = False
    n = 0
    for idx, line in enumerate(lines):
        s = line.strip()
        if in_block:
            if "*/" in s:
                in_block = False
            continue
        if s.startswith(("/*", "{/*")):
            if "*/" not in s:
                in_block = True
            continue
        if s.startswith(("//", "///", "*")) or s.startswith("import "):
            continue
        if idx not in allowed or not HAS_HAN.search(line):
            continue
        prev = ""
        p = idx - 1
        while p >= 0 and not lines[p].strip():
            p -= 1
        if p >= 0:
            prev = lines[p].rstrip()
        new = wrap_jsx_text(line)
        new = wrap_attr(new)
        new = wrap_literals(new, prev.endswith("t("))
        if new != line:
            lines[idx] = new
            n += 1
    if n:
        ensure_hook(lines, path)
    return "\n".join(lines), n


def main():
    write = "--write" in sys.argv
    total = 0
    seen = set()
    for d in TARGET_DIRS:
        for path in sorted((ROOT / d).rglob("*.tsx")):
            if path in seen or "/i18n/" in str(path):
                continue
            seen.add(path)
            new_src, n = process(path)
            if not n:
                continue
            total += n
            print(f"{n:4d}  {path.relative_to(ROOT)}")
            if write:
                path.write_text(new_src, encoding="utf-8")
    print(f"\nTOTAL lines: {total}  ({'WRITTEN' if write else 'dry run'})")


if __name__ == "__main__":
    main()
