import type { NodeService } from "../types";

/// 预置服务模板。
///
/// 新建服务的最大门槛不是表单，是「入口脚本里该写什么」。这三类覆盖了托管
/// 服务绝大多数真实用途；模板生成**可跑**的最小实现，用户在它上面改业务。
/// 脚本里的环境变量全部来自平台注入（CCLOAD_BASE_URL / CCLOAD_API_TOKEN），
/// 没有任何硬编码地址 —— 换内核、换 token 不用改脚本。
export type ServiceTemplate = {
  id: string;
  label: string;
  /** 一句话说清这个服务干什么、谁会连它。 */
  description: string;
  /** 应用模板后预填的端口。 */
  port: number;
  /** 可跑的最小入口脚本。 */
  script: string;
};

/// MCP over Streamable HTTP 的最小 hub:统一入口,把工具调用转给真正的
/// 后端(这里回显)。五家 CLI 的 mcp 配置都指向这个端口,共享一份状态 ——
/// stdio 型每客户端一进程、状态不共享的痛点就在这里被解决。
const MCP_HUB_JS = `// MCP Hub —— 五家 CLI 共享的本地 MCP 入口(Streamable HTTP)。
// CLI 的 mcp 配置指向 http://127.0.0.1:<PORT>/mcp 即可。
// 这是最小骨架:tools/list 与 tools/call 的回显实现。把 handler 换成
// 你真正的逻辑(检索/记忆/数据库),状态写在模块作用域里,所有 CLI 共享。
const http = require('http');
const TOOLS = [{
  name: 'echo',
  description: '回显输入。替换成你的第一个真工具。',
  inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
}];

http.createServer((req, res) => {
  if (req.method === 'GET' && req.url === '/health') { res.writeHead(200); return res.end('ok'); }
  if (req.method === 'POST' && req.url === '/mcp') {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      let msg; try { msg = JSON.parse(body); } catch { res.writeHead(400); return res.end('{}'); }
      // 通知(无 id)必须 202 且不带响应体 —— 回 200+body 会让官方 SDK 的
      // 客户端在 initialize 阶段直接断开,整个握手就废了。
      if (msg === null || typeof msg !== 'object' || Array.isArray(msg) || msg.id === undefined) {
        res.writeHead(msg && typeof msg === 'object' && !Array.isArray(msg) ? 202 : 400);
        return res.end();
      }
      const reply = (result) => {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result }));
      };
      const fail = (code, message) => {
        res.writeHead(200, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ jsonrpc: '2.0', id: msg.id, error: { code, message } }));
      };
      if (msg.method === 'initialize')
        return reply({ protocolVersion: '2025-03-26', capabilities: { tools: {} },
                       serverInfo: { name: 'ccload-hub', version: '0.1.0' } });
      if (msg.method === 'tools/list') return reply({ tools: TOOLS });
      if (msg.method === 'tools/call') {
        // 按名字分发:未知工具要回 -32602,别把回显结果报给一个不存在的工具。
        const tool = TOOLS.find((t) => t.name === msg.params?.name);
        if (!tool) return fail(-32602, 'unknown tool: ' + String(msg.params?.name));
        const text = msg.params?.arguments?.text ?? '';
        return reply({ content: [{ type: 'text', text: 'echo: ' + text }] });
      }
      fail(-32601, 'method not found: ' + String(msg.method));
    });
    return;
  }
  res.writeHead(404); res.end();
}).listen(Number(process.env.PORT), '127.0.0.1');
`;

/// webhook → 无头 CLI 的事件触发器。收到 POST /hook 就 spawn claude/codex,
/// LLM 流量走 CCLOAD_BASE_URL(平台注入),结果回 POST 给 body 里的 callback。
const WEBHOOK_JS = `// Webhook 触发器 —— 收到事件,起无头 CLI 会话处理,结果发回调地址。
// 触发:POST http://127.0.0.1:<PORT>/hook
//   body: {"prompt":"...","cli":"claude|codex|opencode|grok","callback":"https://..."}
// 模型流量自动走 ccload(env: CCLOAD_BASE_URL / CCLOAD_API_TOKEN 由平台注入)。
// 注意:这些 CLI 的接管配置里带着 ccload 的凭据 —— 只在跑你自己的任务时用。
const http = require('http');
const { spawn } = require('child_process');
// 四家 CLI 的无头调用形式(实测):
//   claude   -p <prompt>                      走 ANTHROPIC_BASE_URL
//   codex    exec --skip-git-repo-check <p>   走 model_providers.ccload
//   opencode run <prompt>                     走 provider.ccload
//   grok     -p <prompt>                      走 [model.ccload]
const CLIs = {
  claude:   (p) => ['claude',   ['-p', p]],
  codex:    (p) => ['codex',    ['exec', '--skip-git-repo-check', p]],
  opencode: (p) => ['opencode', ['run', p]],
  grok:     (p) => ['grok',     ['-p', p]],
};
// 只认自己的键:job.cli 是外部输入,'hasOwnProperty' 这类原型链名字会直接把
// 下面的函数查找炸出 TypeError,一个恶意 POST 就能把服务打挂。
function pickCli(name) {
  return Object.hasOwn(CLIs, name) ? CLIs[name] : CLIs.claude;
}

// 并发上限:每个无头 CLI 会话都在烧钱,事件风暴不该变成账单风暴。
// 排队而不拒绝 —— webhook 送达方通常有重试,丢任务比慢更糟。
const MAX_CONCURRENT = Number(process.env.MAX_CONCURRENT) || 3;
let running = 0;
const queue = [];

function launch(job) {
  running++;                                     // 计数只在 launch 一处增加
  const [bin, args] = pickCli(job.cli)(job.prompt ?? '');
  let child;
  let done = false;
  try {
    child = spawn(bin, args, {
      env: { ...process.env },   // CCLOAD_* 平台变量原样传给 CLI
      stdio: ['ignore', 'pipe', 'pipe'],         // 开着的 stdin 会让 claude 干等几秒
      timeout: 10 * 60 * 1000,
    });
  } catch (e) {
    console.error('[webhook] spawn failed:', e.message);
    finish({ code: 127, output: 'spawn failed: ' + e.message });
    return;
  }
  let out = '';
  child.stdout.on('data', (c) => { if (out.length < 256e3) out += c; });
  child.stderr.on('data', (c) => { if (out.length < 256e3) out += c; });
  // 'error'(spawn 失败)和 'close' 可能都发 —— done 标记保证只收一次尾。
  child.on('error', (e) => {
    console.error('[webhook] child error:', e.message);
    finish({ code: 127, output: 'child error: ' + e.message });
  });
  child.on('close', (code) => finish({ code, output: out }));

  function finish(result) {
    if (done) return;
    done = true;
    running--;
    if (queue.length) launch(queue.shift());     // launch 自己会 ++
    const cb = job.callback;
    if (!cb) return console.log('[webhook] done', result.code, String(result.output).slice(0, 500));
    fetch(cb, { method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(result) })
      .catch((e) => console.error('[webhook] callback failed:', e.message));
  }
}

http.createServer((req, res) => {
  if (req.method === 'GET' && req.url === '/health') { res.writeHead(200); return res.end('ok'); }
  if (req.method !== 'POST' || req.url !== '/hook') { res.writeHead(404); return res.end(); }
  let body = '';
  req.on('data', (c) => { if (body.length < 1e6) body += c; });
  req.on('end', () => {
    let job; try { job = JSON.parse(body); } catch { res.writeHead(400); return res.end('{}'); }
    if (job === null || typeof job !== 'object' || Array.isArray(job)) {
      res.writeHead(400); return res.end('{}');
    }
    if (running >= MAX_CONCURRENT) queue.push(job);
    else launch(job);
    res.writeHead(202, { 'content-type': 'application/json' });
    res.end('{"accepted":true}');
  });
}).listen(Number(process.env.PORT), '127.0.0.1');
`;

/// 定时分析:内建 cron,到点拉数据 → 问模型 → 推结果。
const CRON_JS = `// 定时分析 —— 内建 cron,到点跑一次「拉数据 → 问模型 → 推结果」。
// 计划:SCHEDULE env(简写 '@hourly'/'@daily' 或毫秒数);数据源和推送自己接。
// 模型调用走 ccload(CCLOAD_BASE_URL,OpenAI 兼容 /v1/chat/completions)。
const http = require('http');
const SCHEDULE = process.env.SCHEDULE || '@hourly';

const INTERVALS = { '@hourly': 3600e3, '@daily': 86400e3 };
function everyMs(expr) {
  if (INTERVALS[expr]) return INTERVALS[expr];
  return Number(expr) || 3600e3;                        // 也接受毫秒数
}

let inFlight = false;
async function analyze() {
  if (inFlight) return console.log('[cron] skip: 上一轮还没跑完');   // 防堆叠
  inFlight = true;
  try {
    await runOnce();
  } catch (e) {
    // 内核没起、上游 502、返回非 JSON —— 都不算致命。没有这个 catch,
    // setInterval 里的未捕获 rejection 会直接把进程带走(Node ≥15 默认行为)。
    console.error('[cron] run failed:', e.message);
  } finally {
    inFlight = false;
  }
}

async function runOnce() {
  // 1) 拉数据:换成你的数据源(截图、行情、日志文件…)
  // 2) 问模型:走 ccload,token 由平台注入
  const r = await fetch(new URL('/v1/chat/completions', process.env.CCLOAD_BASE_URL), {
    method: 'POST',
    headers: { 'content-type': 'application/json',
               authorization: 'Bearer ' + process.env.CCLOAD_API_TOKEN },
    body: JSON.stringify({ model: process.env.CCLOAD_MODEL || 'claude-opus-5',
      max_tokens: 512,
      messages: [{ role: 'user', content: '报告当前时间,一句话即可。' }] }),
  });
  const data = await r.json();
  const text = data.choices?.[0]?.message?.content ?? JSON.stringify(data).slice(0, 300);
  // 3) 推结果:换成 Telegram / Slack / 你的 webhook
  console.log('[cron]', new Date().toISOString(), text.slice(0, 300));
}

let timer = null;
http.createServer((req, res) => {
  if (req.url === '/health') { res.writeHead(200); return res.end('ok'); }
  if (req.url === '/run-now') { analyze().catch((e) => console.error(e)); res.writeHead(202); return res.end(); }
  res.writeHead(404); res.end();
}).listen(Number(process.env.PORT), '127.0.0.1', () => {
  console.log('[cron] schedule =', SCHEDULE);
  timer = setInterval(analyze, everyMs(SCHEDULE));
});
process.on('SIGTERM', () => { clearInterval(timer); process.exit(0); });
`;

function skeleton(id: string, port: number, body: string): NodeService & { script: string } {
  return {
    id,
    entry: "", // 由 UI 的「保存模板」弹窗让用户选落盘位置后填入
    args: [],
    cwd: null,
    port,
    health_path: "/health",
    env: {},
    enabled: true,
    script: body,
  };
}

export const TEMPLATES: (ServiceTemplate & { script: string })[] = [
  {
    ...skeleton("mcp-hub", 15801, MCP_HUB_JS),
    label: "MCP Hub",
    description:
      "五家 CLI 共享的本地 MCP 入口(Streamable HTTP)。各家 mcp 配置都指向这一个端口,状态共享 —— stdio 型每客户端一进程的痛点就在这解决。",
  },
  {
    ...skeleton("webhook-cli", 15802, WEBHOOK_JS),
    label: "Webhook → CLI",
    description:
      "收到 webhook 就起一个无头 claude/codex 会话处理,结果 POST 回回调地址。事件驱动自动化(Sentry/CI 告警自动修)的骨架。",
  },
  {
    ...skeleton("cron-analysis", 15803, CRON_JS),
    label: "定时分析",
    description:
      "内建 cron:到点拉数据、问模型、推结果。模型调用自动走 ccload 网关,花费记进总览的 CLI 消耗面板。",
  },
];
