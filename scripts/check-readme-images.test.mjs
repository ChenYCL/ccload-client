import assert from "node:assert/strict";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { checkReadmes, classifyImage, extractImageTargets } from "./check-readme-images.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

test("extractImageTargets reads markdown and <img> src", () => {
  const md = 'intro ![logo](docs/assets/logo.png "Logo")\n<img src="docs/assets/hero.png" alt="hero" />';
  assert.deepEqual(extractImageTargets(md), [
    "docs/assets/logo.png",
    "docs/assets/hero.png",
  ]);
});

test("classifyImage recognizes PNG / JPEG / WebP / SVG headers", () => {
  assert.equal(classifyImage(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a])), "png");
  assert.equal(classifyImage(Buffer.from([0xff, 0xd8, 0xff, 0xe0])), "jpeg");
  assert.equal(
    classifyImage(Buffer.from("RIFF....WEBP", "ascii").subarray(0, 12)),
    "webp",
  );
  assert.equal(classifyImage(Buffer.from('<svg xmlns="http://www.w3.org/2000/svg">')), "svg");
  assert.equal(classifyImage(Buffer.from("not-an-image")), null);
});

test("shipped README.md and README.zh-CN.md point at real image files", () => {
  const { results, errors } = checkReadmes(root);
  assert.equal(errors.length, 0, errors.join("\n"));
  const byReadme = new Map();
  for (const item of results) {
    assert.ok(item.bytes > 0, `${item.target} empty`);
    assert.ok(item.kind, `${item.target} has no image header`);
    const list = byReadme.get(item.readme) ?? [];
    list.push(item.target);
    byReadme.set(item.readme, list);
  }
  assert.ok((byReadme.get("README.md") ?? []).length >= 2);
  assert.ok((byReadme.get("README.zh-CN.md") ?? []).length >= 2);
});
