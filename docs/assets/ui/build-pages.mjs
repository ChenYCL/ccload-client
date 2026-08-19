#!/usr/bin/env node
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

const NAV = [
  { g: "监控", items: [
    { id: "dashboard", label: "总览" },
    { id: "logs", label: "实时日志" },
  ]},
  { g: "配置", items: [
    { id: "cli", label: "CLI 接管" },
    { id: "graph", label: "调度图" },
    { id: "fallback", label: "模型链" },
    { id: "models", label: "模型导入" },
    { id: "extensions", label: "扩展管理" },
  ]},
  { g: "系统", items: [
    { id: "web-admin", label: "内核后台" },
    { id: "settings", label: "设置" },
  ]},
];

const ICO = {
  dashboard: `<path d="M3 3h7v9H3zM14 3h7v5h-7zM14 12h7v9h-7zM3 16h7v5H3z"/>`,
  logs: `<path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/>`,
  cli: `<path d="M4 8l4 4-4 4M12 16h8"/>`,
  graph: `<path d="M3 17l6-6 4 4 8-8M14 7h7v7"/>`,
  fallback: `<path d="M6 3v12M6 15l4 4 4-4M18 21V9M18 9l-4-4-4 4"/>`,
  models: `<path d="M16 16h6v6h-6zM2 16h6v6H2zM9 2h6v6H9zM12 8v8M5 16v-4h14v4"/>`,
  extensions: `<path d="M12 2v6M8 6h8M4 10h16v10H4z"/>`,
  "web-admin": `<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 010 18M12 3a14 14 0 000 18"/>`,
  settings: `<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M2 12h3M19 12h3M4.9 19.1L7 17M17 7l2.1-2.1"/>`,
};

function icon(id) {
  return `<svg class="ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${ICO[id]}</svg>`;
}

function shell(active, title, inner) {
  const nav = NAV.map((g) => {
    const items = g.items.map((it) =>
      `<div class="item${it.id === active ? " on" : ""}">${icon(it.id)}${it.label}</div>`,
    ).join("");
    return `<div style="margin-bottom:16px"><div class="gt">${g.g}</div>${items}</div>`;
  }).join("");

  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <title>${title}</title>
  <link rel="stylesheet" href="./shell.css" />
</head>
<body>
  <div class="win">
    <div class="chrome"><i class="dot r"></i><i class="dot y"></i><i class="dot g"></i><span>ccLoad</span></div>
    <div class="app">
      <aside class="aside">
        <div class="brand">
          <svg class="mark" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="22 12 18 12 15 21 9 3 6 12 2 12"/></svg>
          <div><div class="t">ccLoad</div><small>desktop client</small></div>
        </div>
        <nav>${nav}</nav>
        <div class="foot">
          <div class="lang">${icon("settings").replace("settings", "settings")}<span style="font-size:11px">中文</span></div>
          <div class="status"><span class="ping"></span>运行中 · v4.6.37-beta.1</div>
        </div>
      </aside>
      <main>${inner}</main>
    </div>
  </div>
</body>
</html>
`;
}

const pages = {
  dashboard: [
    "总览",
    `
    <div class="row" style="margin-bottom:16px">
      <div>
        <h1 class="t-display">总览</h1>
        <p class="lede">全部数字来自内核 Admin API 的真实字段，客户端只做聚合，不做估算。</p>
      </div>
      <div style="display:flex;gap:10px;align-items:center">
        <span style="background:rgb(16 185 129 / 0.12);color:#047857;border-radius:999px;padding:4px 10px;font-size:12px;font-weight:500">2 个请求进行中</span>
        <div class="card" style="display:flex;overflow:hidden">
          <div style="padding:6px 12px;background:rgb(var(--accent)/0.12);color:rgb(var(--accent));font-size:13px;font-weight:500">本日</div>
          <div style="padding:6px 12px;font-size:13px;color:rgb(var(--muted))">昨日</div>
          <div style="padding:6px 12px;font-size:13px;color:rgb(var(--muted))">本周</div>
        </div>
      </div>
    </div>
    <div style="display:grid;grid-template-columns:repeat(4,1fr);gap:12px">
      ${[
        ["请求数", "1,284", "成功 1,261 · 失败 23"],
        ["成功率", "98.2%", "23 次失败待排查"],
        ["近一分钟 RPM", "14", "今日峰值 41"],
        ["费用", "$18.40", "本日累计"],
      ].map(([k, v, s]) => `<div class="card" style="padding:14px"><div class="muted" style="font-size:12px">${k}</div><div style="font-size:28px;font-weight:600;letter-spacing:-0.03em">${v}</div><div class="muted" style="font-size:11px">${s}</div></div>`).join("")}
    </div>
    <div class="card" style="margin-top:16px;padding:14px 16px">
      <div class="t-title">请求量与成功率</div>
      <svg viewBox="0 0 640 120" style="width:100%;height:120px;margin-top:8px">
        <polyline fill="none" stroke="#4F52DD" stroke-width="2" points="0,80 40,72 80,76 120,58 160,62 200,44 240,50 280,38 320,42 360,30 400,36 440,28 480,34 520,22 560,26 600,18 640,24"/>
        <polyline fill="none" stroke="#22c55e" stroke-width="1.5" opacity=".7" points="0,20 80,18 160,22 240,16 320,19 400,15 480,17 560,14 640,16"/>
      </svg>
    </div>
    <div style="display:grid;grid-template-columns:1fr 1fr;gap:16px;margin-top:16px">
      <div class="card" style="padding:14px 16px">
        <div class="t-title">渠道健康</div>
        ${[["Anthropic","98%","12.1s"],["OpenAI","96%","18.4s"],["Gemini","91%","24.0s"]].map(([n,r,t]) =>
          `<div class="row" style="padding:8px 0;border-top:1px solid rgb(var(--border));margin-top:8px"><span>${n}</span><span class="muted" style="font-size:13px">成功率 ${r} · 均延迟 ${t}</span></div>`).join("")}
      </div>
      <div class="card" style="padding:14px 16px">
        <div class="t-title">模型消耗</div>
        ${[["claude-opus-5","$11.20"],["gpt-5.4","$4.80"],["gemini-3-pro","$2.40"]].map(([n,c]) =>
          `<div class="row" style="padding:8px 0;border-top:1px solid rgb(var(--border));margin-top:8px"><span class="mono" style="font-size:13px">${n}</span><span>${c}</span></div>`).join("")}
      </div>
    </div>
  `],
  logs: [
    "实时日志",
    `
    <div class="row" style="margin-bottom:14px">
      <div>
        <h1 class="t-display">实时日志</h1>
        <p class="lede">进行中 1.5s、历史 2.5s 轮询；窗口切走时自动暂停。</p>
      </div>
      <div class="btn-a" style="font-size:12px">实时开</div>
    </div>
    <div class="card" style="padding:12px 14px;margin-bottom:12px">
      <div class="t-title">进行中</div>
      <div class="row" style="margin-top:10px;font-size:13px">
        <span class="mono">claude-opus-5</span>
        <span class="muted">Anthropic · 流式 · 12s</span>
      </div>
      <div class="row" style="margin-top:8px;font-size:13px">
        <span class="mono">gpt-5.4</span>
        <span class="muted">OpenAI · 流式 · 4s</span>
      </div>
    </div>
    <div class="card" style="overflow:hidden">
      <div style="padding:10px 14px;border-bottom:1px solid rgb(var(--border));display:flex;gap:8px;font-size:12px;color:rgb(var(--muted))">
        <span class="btn" style="padding:3px 8px">全部模型</span>
        <span class="btn" style="padding:3px 8px">全部渠道</span>
        <span class="btn" style="padding:3px 8px">全部状态码</span>
        <span class="btn" style="padding:3px 8px">只看错误</span>
      </div>
      <table style="width:100%;border-collapse:collapse;font-size:13px">
        <thead><tr class="muted" style="text-align:left">
          <th style="padding:8px 14px;font-weight:500">时间</th>
          <th style="padding:8px;font-weight:500">状态</th>
          <th style="padding:8px;font-weight:500">模型</th>
          <th style="padding:8px;font-weight:500">渠道</th>
          <th style="padding:8px 14px;font-weight:500">费用</th>
        </tr></thead>
        <tbody>
          ${[
            ["14:21:08","200","claude-opus-5","Anthropic","$0.042"],
            ["14:20:51","200","gpt-5.4","OpenAI","$0.018"],
            ["14:20:12","429","gemini-3-pro","Gemini","—"],
            ["14:19:44","200","claude-opus-5","Anthropic","$0.031"],
            ["14:19:03","200","claude-opus-5","Anthropic","$0.028"],
          ].map(([t,s,m,c,cost]) => `<tr style="border-top:1px solid rgb(var(--border))">
            <td style="padding:8px 14px" class="mono muted">${t}</td>
            <td style="padding:8px" class="${s==="200"?"ok":"bad"}">${s}</td>
            <td style="padding:8px" class="mono">${m}</td>
            <td style="padding:8px">${c}</td>
            <td style="padding:8px 14px">${cost}</td>
          </tr>`).join("")}
        </tbody>
      </table>
    </div>
  `],
  cli: [
    "CLI 接管",
    `
    <div class="row">
      <div>
        <h1 class="t-display">CLI 接管</h1>
        <p class="lede">把各 CLI 的配置指到内核。写入前自动快照；不确定时先开沙箱，不碰真实配置。</p>
      </div>
      <div class="btn">快照历史</div>
    </div>
    <div style="margin-top:18px;display:flex;flex-direction:column;gap:12px">
      ${[
        ["Claude Code", "~/.claude/settings.json", "已指向本机网关", true],
        ["Codex", "~/.codex/config.toml", "已指向本机网关", true],
        ["Gemini CLI", "~/.gemini/settings.json", "尚未接管", false],
        ["Grok Build", "~/.grok", "已指向本机网关", true],
        ["OpenCode", "~/.config/opencode/opencode.json", "尚未接管", false],
      ].map(([n, p, st, on]) => `
        <div class="card" style="padding:14px 16px">
          <div class="row">
            <div>
              <div style="font-weight:600">${n}</div>
              <div class="mono muted" style="font-size:12px">${p}</div>
              <div style="margin-top:4px;font-size:12px" class="${on?"ok":"muted"}">${st}</div>
            </div>
            <div class="${on?"btn":"btn-a"}">${on?"重新写入":"写入"}</div>
          </div>
        </div>`).join("")}
    </div>
  `],
  graph: [
    "调度图",
    `
    <div class="row">
      <div>
        <h1 class="t-display">调度图</h1>
        <p class="lede">把「哪种活用哪家的哪个模型」配成一张表。CLI 只认档位别名，换家、重试、冷却交给内核。</p>
      </div>
      <div style="display:flex;gap:8px"><div class="btn">已保存</div><div class="btn-a">应用</div></div>
    </div>
    <div style="display:grid;grid-template-columns:1fr 1fr;gap:16px;margin-top:16px">
      <div class="card" style="padding:14px 16px">
        <div class="t-title">供应商</div>
        ${[["Anthropic","优先"],["OpenAI","其次"],["Gemini","兜底"]].map(([n,r],i) =>
          `<div class="row" style="margin-top:10px;padding-top:10px;border-top:1px solid rgb(var(--border))">
            <span>${i+1}. ${n}</span><span class="muted" style="font-size:13px">${r}</span>
          </div>`).join("")}
      </div>
      <div class="card" style="padding:14px 16px">
        <div class="t-title">档位</div>
        ${[["opus","claude-opus-5"],["sonnet","claude-sonnet-5"],["gpt","gpt-5.4"],["flash","gemini-3-flash"]].map(([t,m]) =>
          `<div class="row" style="margin-top:10px;padding-top:10px;border-top:1px solid rgb(var(--border))">
            <span class="mono">${t}</span><span class="mono muted" style="font-size:13px">${m}</span>
          </div>`).join("")}
      </div>
    </div>
    <div class="card" style="margin-top:16px;padding:12px 16px;font-size:13px;color:#047857;background:rgb(16 185 129 / 0.08);border-color:rgb(16 185 129 / 0.25)">校验通过，可以应用。不会写入互相矛盾的优先级。</div>
  `],
  fallback: [
    "模型链",
    `
    <div class="row">
      <div>
        <h1 class="t-display">模型链</h1>
        <p class="lede">把一条 fallback 链写成按优先级递减的渠道，内核选择器会自动走完。</p>
      </div>
      <div class="btn-a">新建链</div>
    </div>
    <div style="margin-top:18px;display:flex;flex-direction:column;gap:12px">
      <div class="card" style="padding:16px">
        <div style="font-weight:600">opus-stack</div>
        <div style="margin-top:10px;display:flex;align-items:center;gap:8px;font-size:13px">
          <span class="btn" style="background:rgb(var(--accent)/0.1);border-color:transparent;color:rgb(var(--accent))">claude-opus-5 · Anthropic</span>
          <span class="muted">→</span>
          <span class="btn">kimi-k2 · Moonshot</span>
          <span class="muted">→</span>
          <span class="btn">gpt-5.4 · OpenAI</span>
        </div>
        <div style="margin-top:12px;display:flex;gap:8px"><div class="btn" style="font-size:12px">应用</div><div class="btn" style="font-size:12px">编辑</div></div>
      </div>
      <div class="card" style="padding:16px">
        <div style="font-weight:600">daily-flash</div>
        <div style="margin-top:10px;display:flex;align-items:center;gap:8px;font-size:13px">
          <span class="btn" style="background:rgb(var(--accent)/0.1);border-color:transparent;color:rgb(var(--accent))">gemini-3-flash · Gemini</span>
          <span class="muted">→</span>
          <span class="btn">claude-haiku-4.5 · Anthropic</span>
        </div>
        <div style="margin-top:12px;display:flex;gap:8px"><div class="btn" style="font-size:12px">应用</div><div class="btn" style="font-size:12px">编辑</div></div>
      </div>
    </div>
  `],
  models: [
    "模型导入",
    `
    <div class="row">
      <div>
        <h1 class="t-display">模型导入</h1>
        <p class="lede">从内核渠道聚合别名，追加进各 CLI 的模型目录。不动你当前选中的模型。</p>
      </div>
      <div class="btn-a">导入到 2 个 CLI</div>
    </div>
    <div style="margin-top:12px;display:flex;gap:8px;align-items:center;font-size:13px">
      <span class="muted">写入到</span>
      <span class="btn" style="background:rgb(var(--accent)/0.12);border-color:transparent;color:rgb(var(--accent));font-weight:500">Claude Code</span>
      <span class="btn" style="background:rgb(var(--accent)/0.12);border-color:transparent;color:rgb(var(--accent));font-weight:500">Codex</span>
      <span class="btn">OpenCode</span>
      <span class="muted" style="margin-left:8px">已选 4/6 个模型</span>
    </div>
    <div class="card" style="margin-top:14px;overflow:hidden">
      <table style="width:100%;border-collapse:collapse;font-size:13px">
        <tr class="muted" style="text-align:left">
          <th style="padding:8px 14px;font-weight:500"></th>
          <th style="padding:8px;font-weight:500">别名</th>
          <th style="padding:8px;font-weight:500">上下文</th>
          <th style="padding:8px 14px;font-weight:500">Claude 槽位</th>
        </tr>
        ${[
          [true,"claude-opus-5","1,000,000","opus"],
          [true,"claude-sonnet-5","1,000,000","sonnet"],
          [true,"gpt-5.4","1,000,000","不绑定"],
          [true,"gemini-3-pro","1,000,000","不绑定"],
          [false,"kimi-k2","256,000","不绑定"],
          [false,"glm-5","200,000","不绑定"],
        ].map(([on,n,w,t]) => `<tr style="border-top:1px solid rgb(var(--border))">
          <td style="padding:8px 14px">${on?"☑":"☐"}</td>
          <td style="padding:8px" class="mono">${n}</td>
          <td style="padding:8px" class="mono muted">${w}</td>
          <td style="padding:8px 14px">${t}</td>
        </tr>`).join("")}
      </table>
    </div>
  `],
  extensions: [
    "扩展管理",
    `
    <div class="row">
      <div>
        <h1 class="t-display">扩展管理</h1>
        <p class="lede">MCP / Skill / Agent / Hook 一处配置，推给装了它们的每一个 CLI。</p>
      </div>
      <div class="btn-a">新建</div>
    </div>
    <div style="margin-top:14px;display:flex;gap:8px">
      <span class="btn" style="background:rgb(var(--accent)/0.1);border:0;color:rgb(var(--accent));font-weight:500">MCP</span>
      <span class="btn">Skill</span>
      <span class="btn">Agent</span>
      <span class="btn">Hook</span>
    </div>
    <div style="margin-top:14px;display:flex;flex-direction:column;gap:10px">
      ${[
        ["filesystem", "读写工作区文件", ["Claude Code", "Codex"]],
        ["github", "仓库、Issue、PR", ["Claude Code"]],
        ["ccload-vision", "把图片交给内核里的多模态模型", ["Claude Code", "OpenCode"]],
      ].map(([id, d, where]) => `
        <div class="card" style="padding:14px 16px">
          <div class="row">
            <div>
              <div class="mono" style="font-weight:600">${id}</div>
              <div class="muted" style="font-size:13px">${d}</div>
            </div>
            <div style="display:flex;gap:6px">${where.map((w) => `<span class="btn" style="font-size:11px;padding:3px 8px">${w}</span>`).join("")}</div>
          </div>
        </div>`).join("")}
    </div>
  `],
  "web-admin": [
    "内核后台",
    `
    <div>
      <h1 class="t-display">内核后台</h1>
      <p class="lede">ccLoad 自带的管理界面，在独立窗口中打开，字段随内核升级自动跟进。</p>
    </div>
    <div style="margin-top:18px;display:grid;grid-template-columns:1fr 1fr 1fr;gap:12px">
      ${[
        ["渠道管理", "上游渠道、Key、模型与降级链配置"],
        ["令牌管理", "CLI 接入用的 API 令牌与限额"],
        ["请求日志", "实时请求流与错误排查"],
        ["用量统计", "成本、Token 用量与渠道健康度"],
        ["内核设置", "内核运行参数与系统设置"],
      ].map(([t,d]) => `<div class="card" style="padding:16px"><div class="t-title">${t}</div><div class="muted" style="font-size:12px;margin-top:4px">${d}</div></div>`).join("")}
    </div>
  `],
  settings: [
    "设置",
    `
    <h1 class="t-display">设置</h1>
    <div class="card" style="margin-top:16px;padding:16px">
      <div class="t-title">连接方式</div>
      <p class="lede">本机托管一份内核，或接到已经在跑的实例。</p>
      <div style="margin-top:12px;display:flex;gap:8px">
        <span class="btn" style="background:rgb(var(--accent)/0.12);border-color:transparent;color:rgb(var(--accent));font-weight:500">本机托管</span>
        <span class="btn">连接远端</span>
      </div>
      <div class="row" style="margin-top:14px;font-size:14px"><span class="muted">监听端口</span><span class="mono">15722</span></div>
    </div>
    <div class="card" style="margin-top:12px;padding:16px">
      <div class="t-title">CLI 写入走沙箱</div>
      <p class="lede">打开后接管只写 ~/.ccload-client/sandbox/，不碰真实配置。</p>
      <div style="margin-top:10px;width:40px;height:22px;border-radius:999px;background:rgb(var(--accent));position:relative">
        <div style="width:18px;height:18px;border-radius:50%;background:#fff;position:absolute;top:2px;right:2px"></div>
      </div>
    </div>
    <div class="card" style="margin-top:12px;padding:16px">
      <div class="t-title">接入地址</div>
      <p class="lede">第三方工具可以直接填这些入口，协议转换在内核里完成。</p>
      ${[["Anthropic 规范","http://127.0.0.1:15722"],["OpenAI 规范","http://127.0.0.1:15722/v1"],["Gemini 规范","http://127.0.0.1:15722/v1beta"]].map(([k,v]) =>
        `<div class="row" style="margin-top:8px;padding:8px 10px;border:1px solid rgb(var(--border));border-radius:8px">
          <span style="font-size:13px">${k}</span><span class="mono muted" style="font-size:12px">${v}</span>
        </div>`).join("")}
    </div>
  `],
};

for (const [id, [title, inner]] of Object.entries(pages)) {
  writeFileSync(join(here, `page-${id}.html`), shell(id, title, inner));
  console.log("wrote", `page-${id}.html`);
}
