// 用 TypeScript 编译器 API 找 JSX 文本节点，把中文包进 {t("…")}。
//
// 为什么不用正则：上一轮的正则只认单行的 `>文本<`，于是漏掉了跨行的段落、
// 跟在元素后面的文本（`<Plus /> 新建`）、以及被 {expr} 断开的片段 ——
// 那正是英文界面里还在显示中文的那一批。JSX 文本节点的边界只有解析器知道。
//
// 只改 JsxText 节点，属性和字符串字面量由 scripts/i18n-wrap.py 管。
// 带 {expr} 的段落会被切成多个 JsxText，各自包一层 —— 顺序在中英文里恰好一致
// （「已选 {n} 个模型」→ "Selected {n} models"），不必引入带参数的模板。
//
// 用法：node scripts/i18n-jsx.mjs [--write]

import ts from "typescript";
import fs from "node:fs";
import path from "node:path";

const ROOT = path.resolve(import.meta.dirname, "..");
const WRITE = process.argv.includes("--write");
const HAN = /[一-鿿]/;

function walk(node, out) {
  if (node.kind === ts.SyntaxKind.JsxText) {
    const raw = node.getFullText();
    if (HAN.test(raw)) out.push(node);
  }
  node.forEachChild((c) => walk(c, out));
}

function collect(dir, acc = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) collect(p, acc);
    else if (e.name.endsWith(".tsx")) acc.push(p);
  }
  return acc;
}

let total = 0;
const keys = new Set();

for (const file of collect(path.join(ROOT, "src")).sort()) {
  const src = fs.readFileSync(file, "utf8");
  const sf = ts.createSourceFile(file, src, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const nodes = [];
  walk(sf, nodes);
  if (!nodes.length) continue;

  // 从后往前替换，避免偏移失效
  let out = src;
  let n = 0;
  for (const node of nodes.reverse()) {
    const start = node.getFullStart();
    const end = node.getEnd();
    const raw = src.slice(start, end);
    const text = raw.trim();
    if (!text || !HAN.test(text)) continue;
    // JSX 文本里的换行是排版，不是内容：折成单个空格再当 key。
    const key = text.replace(/\s+/g, " ").replace(/"/g, '\\"');
    // 保留原有的前后空白（决定单词之间有没有空格）
    const lead = raw.slice(0, raw.length - raw.trimStart().length);
    const tail = raw.slice(raw.trimEnd().length);
    out = out.slice(0, start) + lead + `{t("${key}")}` + tail + out.slice(end);
    keys.add(key.replace(/\\"/g, '"'));
    n++;
  }
  if (n) {
    total += n;
    console.log(`${String(n).padStart(4)}  ${path.relative(ROOT, file)}`);
    if (WRITE) fs.writeFileSync(file, out);
  }
}

fs.writeFileSync("/tmp/jsx-keys.json", JSON.stringify([...keys].sort(), null, 1));
console.log(`\nTOTAL JsxText nodes: ${total}  (${WRITE ? "WRITTEN" : "dry run"})`);
console.log(`keys -> /tmp/jsx-keys.json (${keys.size})`);
