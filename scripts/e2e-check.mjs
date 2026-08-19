#!/usr/bin/env node
// End-to-end check without launching the GUI: spawn the sidecar exactly as the
// Rust layer does, then exercise login → token → admin CRUD.
// Usage: node scripts/e2e-check.mjs [port]

import { spawn } from "node:child_process";
import { mkdtempSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const bin = join(root, "src-tauri", "binaries", "ccload");
const port = Number(process.argv[2] ?? 15799);
const pass = "e2e-" + Math.random().toString(36).slice(2, 10);
const base = `http://127.0.0.1:${port}`;
const dataDir = mkdtempSync(join(tmpdir(), "ccload-e2e-"));

if (!existsSync(bin)) {
  console.error("sidecar missing — run: node scripts/build-kernel.mjs");
  process.exit(1);
}

const child = spawn(bin, [], {
  cwd: dataDir,
  env: {
    ...process.env,
    CCLOAD_PASS: pass,
    PORT: String(port),
    SQLITE_PATH: join(dataDir, "ccload.db"),
    GIN_LOG: "false",
    TRUSTED_PROXIES: "none",
  },
  stdio: ["ignore", "pipe", "pipe"],
});
let kernelLog = "";
child.stdout.on("data", (d) => (kernelLog += d));
child.stderr.on("data", (d) => (kernelLog += d));

const fail = (msg) => {
  console.error("FAIL:", msg);
  console.error(kernelLog.split("\n").slice(-12).join("\n"));
  child.kill("SIGKILL");
  process.exit(1);
};

// Startup binds the listener only after two network fetches (~20s cold).
async function waitReady(timeoutMs = 90_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) fail(`kernel exited early (${child.exitCode})`);
    try {
      const r = await fetch(`${base}/health`);
      if (r.ok) return (Date.now() - start) / 1000;
    } catch {}
    await new Promise((r) => setTimeout(r, 400));
  }
  fail("/health never responded");
}

const start = Date.now();
const readyIn = await waitReady();
console.log(`✓ kernel ready in ${readyIn.toFixed(1)}s`);

const login = await fetch(`${base}/login`, {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ mode: "admin", password: pass }),
}).then((r) => r.json());
const token = login?.data?.token;
if (!token) fail(`login returned no token: ${JSON.stringify(login)}`);
console.log(`✓ admin login (token ${token.length} chars)`);

const admin = (method, path, body) =>
  fetch(`${base}/admin/${path}`, {
    method,
    headers: {
      authorization: `Bearer ${token}`,
      "content-type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
  }).then(async (r) => ({ status: r.status, json: await r.json() }));

const mk = await admin("POST", "auth-tokens", {
  description: "e2e-client",
  is_active: true,
});
if (!mk.json?.data?.token) fail(`token create failed: ${JSON.stringify(mk.json)}`);
console.log("✓ api token created (plaintext returned once)");

const ch = await admin("POST", "channels", {
  name: "e2e-anthropic",
  api_key: "sk-ant-e2e-fake",
  // This build takes `urls: [{url}]`, not the `url: "..."` string the README
  // documents. Exactly the kind of drift the thin-passthrough design absorbs.
  urls: [{ url: "https://api.anthropic.com" }],
  channel_type: "anthropic",
  priority: 10,
  enabled: true,
  models: [{ model: "claude-sonnet-4-6" }],
});
if (!ch.json?.success) fail(`channel create failed: ${JSON.stringify(ch.json)}`);
const chId = ch.json.data.id;
console.log(`✓ channel created (id=${chId}, http ${ch.status})`);

const list = await admin("GET", "channels");
if (!Array.isArray(list.json?.data) || list.json.data.length !== 1) {
  fail(`channel list unexpected: ${JSON.stringify(list.json)}`);
}
console.log("✓ channel list reflects it");

// The create response reports key_count 0; confirm the key really landed
// rather than being silently dropped.
const keys = await admin("GET", `channels/${chId}/keys`);
const keyCount = Array.isArray(keys.json?.data) ? keys.json.data.length : 0;
if (keyCount !== 1) {
  fail(`expected 1 stored api key, got ${keyCount}: ${JSON.stringify(keys.json)}`);
}
console.log("✓ api key persisted (create response's key_count=0 is stale, not a drop)");

const settings = await admin("GET", "settings");
const items = settings.json?.data ?? [];
const schemaOk = items.every((i) => "value_type" in i && "editable" in i);
if (!items.length || !schemaOk) fail("settings did not carry schema metadata");
console.log(`✓ ${items.length} settings carry schema metadata (value_type/editable)`);

child.kill("SIGTERM");
await new Promise((r) => setTimeout(r, 1500));
child.kill("SIGKILL");
console.log("\nALL CHECKS PASSED");
