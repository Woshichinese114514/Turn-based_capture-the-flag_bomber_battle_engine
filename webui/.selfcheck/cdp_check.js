#!/usr/bin/env node
/*
 * webui/.selfcheck/cdp_check.js —— 无头验收小工具（辅助脚本，不属于页面运行时）
 * -----------------------------------------------------------------------------
 * 设计意图：
 *   · 截图只能证明「画面上有像素」，证明不了「没有 JS 报错」「状态钩子正确」。
 *     这个脚本用 Chrome DevTools Protocol 打开页面，读取 window.__QFR_READY__ /
 *     __QFR_STATUS__ / __QFR_ERROR__ 与关键 DOM 计数，并把 console 错误收集下来。
 *   · 不引入任何 npm 依赖：用 Node 内置 WebSocket（Node 22+）直连 CDP。
 *   · 用法：HOME=<工作区临时目录> node webui/.selfcheck/cdp_check.js <相对 replay 路径> <tick>
 */
'use strict';

const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const ROOT = path.resolve(__dirname, '..', '..');           // 工作区根目录
const CHROMIUM = process.env.QFR_CHROMIUM || 'chromium';
const PORT = 9333;

// 参数：<replay 相对路径> [tick] [截图路径] [--nowrap] [--err=文本] [--script=文件]
// 用命名开关解析（而不是固定位置），避免「多加一个参数就全错位」这种调试陷阱。
const argv = process.argv.slice(2);
const positional = argv.filter((a) => !a.startsWith('--'));
const opt = (name) => {
  const hit = argv.find((a) => a === '--' + name || a.startsWith('--' + name + '='));
  if (!hit) return null;
  const eq = hit.indexOf('=');
  return eq === -1 ? true : hit.slice(eq + 1);
};
const rel = positional[0] || '../samples/demo_2p.jsonl';    // 相对 webui/ 的路径
const tick = positional[1] || '1';
const shotPath = positional[2] || '';
const errText = opt('err');                                 // --err=... 人为触发错误横幅
const WRAP = !!opt('nowrap');                               // --nowrap: 不追加 &tick=

const tmpHome = path.join(ROOT, '.tmp', 'run');
fs.mkdirSync(tmpHome, { recursive: true });

const url = 'file://' + path.join(ROOT, 'webui', 'index.html') + '?replay=' + rel +
  (WRAP ? '' : '&tick=' + tick) + (errText ? '&err=' + encodeURIComponent(String(errText)) : '');

const child = spawn(CHROMIUM, [
  '--headless=new', '--no-sandbox', '--disable-gpu', '--hide-scrollbars',
  '--allow-file-access-from-files', '--disable-dev-shm-usage',
  '--user-data-dir=' + path.join(tmpHome, 'udd-cdp'),
  '--remote-debugging-port=' + PORT,
  '--window-size=1440,900',
  url
], { env: Object.assign({}, process.env, { HOME: tmpHome }), stdio: ['ignore', 'pipe', 'pipe'] });

let stderr = '';
child.stderr.on('data', (d) => { stderr += d.toString(); });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function fetchJson(p) {
  // 用 curl 而不是 fetch：不受 Node 的 file/网络策略影响，且端口是本地 http。
  const { execFileSync } = require('child_process');
  const out = execFileSync('curl', ['-s', '--max-time', '5', 'http://127.0.0.1:' + PORT + p]);
  return JSON.parse(out.toString());
}

async function main() {
  // 等 /json/version 可用
  let ver = null;
  for (let i = 0; i < 60; i++) {
    try { ver = await fetchJson('/json/version'); break; } catch (e) { await sleep(200); }
  }
  if (!ver) throw new Error('chromium 调试端口未就绪：\n' + stderr.slice(-2000));

  const targets = await fetchJson('/json/list');
  const page = targets.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
  if (!page) throw new Error('找不到 page target：' + JSON.stringify(targets));

  const ws = new WebSocket(page.webSocketDebuggerUrl);
  const pending = new Map();
  const consoleErrors = [];
  let id = 0;

  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  ws.onmessage = (ev) => {
    let msg;
    try { msg = JSON.parse(ev.data); } catch (e) { return; }
    if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); return; }
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
      consoleErrors.push(msg.params.args.map((a) => a.value !== undefined ? a.value : a.description).join(' '));
    }
    if (msg.method === 'Runtime.exceptionThrown') {
      const d = msg.params.exceptionDetails;
      consoleErrors.push((d.exception && d.exception.description) || d.text);
    }
  };
  const send = (method, params) => new Promise((res) => {
    const mid = ++id;
    pending.set(mid, (m) => res(m.result || m.error));
    ws.send(JSON.stringify({ id: mid, method, params: params || {} }));
  });
  const evaluate = async (expr) => {
    const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
    if (r && r.exceptionDetails) return { __err: (r.exceptionDetails.exception || {}).description || r.exceptionDetails.text };
    return r && r.result ? r.result.value : null;
  };

  await send('Runtime.enable');
  await send('Page.enable');

  // 等 __QFR_READY__（最多 15s；用轮询而不是 virtual-time，避免和 XHR 抢时钟）
  let ready = false;
  for (let i = 0; i < 75; i++) {
    ready = await evaluate('window.__QFR_READY__ === true');
    if (ready === true) break;
    await sleep(200);
  }

  const probe = await evaluate(`(function () {
    var visible = function (el) { return !!(el && !el.hidden && el.offsetParent !== null); };
    var rows = document.querySelectorAll('#logList .log-row');
    var c = document.getElementById('canvas');
    return {
      ready: window.__QFR_READY__ === true,
      status: window.__QFR_STATUS__ || null,
      error: window.__QFR_ERROR__ || null,
      readyState: document.readyState,
      logRows: rows.length,
      logFirstText: rows.length ? rows[0].textContent : null,
      scoreCards: document.querySelectorAll('#scoreBoard .score-card').length,
      warningVisible: visible(document.getElementById('warningBar')),
      warningText: (function(){var e=document.getElementById('warningBar');return e&&!e.hidden?e.textContent:null;})(),
      errorVisible: visible(document.getElementById('errorBanner')),
      errorText: (function(){var e=document.getElementById('errorBanner');return e&&!e.hidden?e.textContent:null;})(),
      noticeText: (function(){var e=document.getElementById('noticeBar');return e&&!e.hidden?e.textContent:null;})(),
      canvasCss: c ? { w: c.getBoundingClientRect().width, h: c.getBoundingClientRect().height, px: c.width, py: c.height } : null,
      tickReadout: (function(){var e=document.getElementById('infoTick');return e?e.textContent:null;})(),
      statusText: (function(){var e=document.getElementById('statusText');return e?e.textContent:null;})(),
      headerFile: (function(){var e=document.getElementById('headerFile');return e?e.textContent:null;})(),
      canvasInk: (function () {
        // 采样画布：统计非背景色像素比例，证明「真的画了东西」而不是黑屏
        try {
          var d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
          var n = 0, total = 0;
          for (var i = 0; i < d.length; i += 4 * 97) { total++; if (d[i] > 60 || d[i+1] > 60 || d[i+2] > 90) n++; }
          return { sampled: total, nonBg: n, ratio: total ? +(n / total).toFixed(3) : 0 };
        } catch (e) { return { err: String(e) }; }
      })()
    };
  })()`);

  // 可选的「交互脚本」：第 6 个参数传给 node 时是 --script=<file>，在页面上求值一次
  // 用来模拟点按钮/键盘（拖拽与文件选择无法在无头里自动化，因此用状态入口验证等价路径）。
  let extra = null;
  const scriptPath = opt('script');
  if (scriptPath) {
    // 脚本文件本身就是「一个返回 Promise 的表达式」（形如 (async function(){...})()），
    // 直接求值才能拿到它 resolve 的值；不要再包一层函数，否则外壳没有 return。
    const code = fs.readFileSync(String(scriptPath), 'utf8');
    extra = await evaluate(code.trim());
  }

  let shot = null;
  if (shotPath) {
    const r = await send('Page.captureScreenshot', { format: 'png' });
    if (r && r.data) { fs.writeFileSync(shotPath, Buffer.from(r.data, 'base64')); shot = shotPath; }
  }

  const out = { url, ready, probe, extra, consoleErrors, shot };
  process.stdout.write(JSON.stringify(out, null, 2) + '\n');
  ws.close();
  child.kill('SIGKILL');
}

main().catch((e) => {
  process.stdout.write(JSON.stringify({ fatal: String(e && e.message || e) }, null, 2) + '\n');
  try { child.kill('SIGKILL'); } catch (err) { /* ignore */ }
  process.exit(1);
});
