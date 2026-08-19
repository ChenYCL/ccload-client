#!/usr/bin/env node
// Assert every markdown / HTML image in the shipped READMEs exists on disk
// and has a real image header. Reads README.md and README.zh-CN.md themselves,
// not copies. No network fetches; relative paths only.

import { readFileSync, statSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const MAGICS = [
  { name: "png", test: (b) => b.length >= 4 && b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47 },
  { name: "jpeg", test: (b) => b.length >= 2 && b[0] === 0xff && b[1] === 0xd8 },
  { name: "webp", test: (b) => b.length >= 12 && b.subarray(0, 4).toString("ascii") === "RIFF" && b.subarray(8, 12).toString("ascii") === "WEBP" },
  {
    name: "svg",
    test: (b) => {
      const head = b.subarray(0, 256).toString("utf8").replace(/^\uFEFF/, "").trimStart();
      return head.startsWith("<svg") || (head.startsWith("<?xml") && /<svg[\s>]/i.test(head));
    },
  },
];

export function extractImageTargets(markdown) {
  const targets = [];
  const md = /!\[[^\]]*]\(([^)\s]+)(?:\s+"[^"]*")?\)/g;
  const html = /<img\b[^>]*?\bsrc\s*=\s*["']([^"']+)["']/gi;
  let m;
  while ((m = md.exec(markdown))) targets.push(m[1]);
  while ((m = html.exec(markdown))) targets.push(m[1]);
  return targets;
}

export function classifyImage(buf) {
  return MAGICS.find((x) => x.test(buf))?.name ?? null;
}

export function checkReadmes(root) {
  const files = ["README.md", "README.zh-CN.md"];
  const results = [];
  const errors = [];
  for (const rel of files) {
    const readme = join(root, rel);
    if (!existsSync(readme)) {
      errors.push(`missing ${rel}`);
      continue;
    }
    const text = readFileSync(readme, "utf8");
    const targets = extractImageTargets(text);
    if (targets.length === 0) errors.push(`${rel} has no image references`);
    for (const raw of targets) {
      const item = { readme: rel, target: raw };
      if (/^https?:\/\//i.test(raw) || raw.startsWith("data:")) {
        item.error = "remote/data URI is not allowed — README images must be in-repo files";
        errors.push(`${rel}: ${item.error}: ${raw}`);
        results.push(item);
        continue;
      }
      const abs = resolve(dirname(readme), raw.split("#")[0].split("?")[0]);
      item.path = abs;
      if (!existsSync(abs)) {
        item.error = "file does not exist";
        errors.push(`${rel}: ${raw} does not exist`);
        results.push(item);
        continue;
      }
      const st = statSync(abs);
      item.bytes = st.size;
      if (st.size <= 0) {
        item.error = "empty file";
        errors.push(`${rel}: ${raw} is empty`);
        results.push(item);
        continue;
      }
      const buf = readFileSync(abs);
      item.kind = classifyImage(buf);
      if (!item.kind) {
        item.error = "unrecognized image header";
        errors.push(`${rel}: ${raw} is not PNG/JPEG/WebP/SVG`);
      }
      results.push(item);
    }
  }
  return { root, results, errors };
}

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..");
const isMain = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (isMain) {
  const { results, errors } = checkReadmes(repoRoot);
  for (const r of results) {
    const loc = `${r.readme} → ${r.target}`;
    if (r.error) console.log(`FAIL  ${loc}  ${r.error}`);
    else console.log(`OK    ${loc}  ${r.kind}  ${r.bytes} bytes`);
  }
  if (errors.length) {
    console.error(`\n${errors.length} error(s)`);
    process.exit(1);
  }
  console.log(`\n${results.length} image(s) in shipped READMEs, all valid`);
}
