#!/usr/bin/env node
/**
 * tools/shot.js —— Lead 自有的「无头浏览器取证」工具（CDP 版截图 + 状态断言）。
 *
 * 为什么需要这个工具（设计意图，非复述代码）：
 * 验收阶段 5 最初直接用 `chromium --headless --screenshot=...` 截图当证据，实测发现
 * 该路径存在**合成时序怪癖**：回放加载后由 JS 插入的告警横幅（`#warningBar`）虽然已经
 * `hidden=false` 且 `offsetParent !== null`（`--dump-dom` 可见、webui 自己的 CDP 截图也
 * 能看到 1249 个告警色像素），但 CLI 截图里它一个像素都没有（见 out/accept 诊断产物
 * vm_budget20.png / vm_compositor.png）。于是「横幅可见」无法用 CLI 截图证明。
 *
 * 本工具改走 DevTools Protocol：等页面自己上报就绪（`window.__QFR_READY__`），先对
 * `window.__QFR_STATUS__` 做**结构化断言**（渲染帧数、错误数、告警条数），再让浏览器
 * 自己回传合成后的画面（`Page.captureScreenshot`）。这样拿到的既是像素证据，也是状态
 * 证据；断言失败时退出码为 1，可以直接当验收关卡。
 *
 * 依赖：只用 Node 内置能力（`fetch` + 全局 `WebSocket`，Node ≥ 22 均有），不装任何 npm 包。
 * 用法：
 *   node tools/shot.js --url file:///.../index.html?replay=... --out out/shot.png \
 *        [--width 1440] [--height 900] [--wait-ms 15000] \
 *        [--expect-ready] [--require-warnings] [--require-status] [--allow-errors]
 * 退出码：0 = 全部断言通过；1 = 断言失败或超时；2 = 用法/环境错误。
 */

'use strict';

const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

/* ----------------------------- 参数解析 ----------------------------- */

/** 解析 `--key value` / `--flag` 形式的参数；未知参数直接报错，避免「拼错了却没断言」。 */
function parseArgs(argv) {
  const opts = {
    url: null,
    out: null,
    width: 1440,
    height: 900,
    waitMs: 15000,
    settleMs: 600,
    expectReady: true,
    requireWarnings: false,
    requireStatus: true,
    allowErrors: false,
    fixedViewport: false,
    chrome: process.env.CHROME || '',
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    switch (arg) {
      case '--url': opts.url = argv[++i]; break;
      case '--out': opts.out = argv[++i]; break;
      case '--width': opts.width = Number(argv[++i]); break;
      case '--height': opts.height = Number(argv[++i]); break;
      case '--wait-ms': opts.waitMs = Number(argv[++i]); break;
      case '--settle-ms': opts.settleMs = Number(argv[++i]); break;
      case '--chrome': opts.chrome = argv[++i]; break;
      case '--expect-ready': opts.expectReady = true; break;
      case '--no-expect-ready': opts.expectReady = false; break;
      case '--require-warnings': opts.requireWarnings = true; break;
      case '--require-status': opts.requireStatus = true; break;
      case '--no-require-status': opts.requireStatus = false; break;
      case '--allow-errors': opts.allowErrors = true; break;
      case '--fixed-viewport': opts.fixedViewport = true; break;
      default:
        throw new Error(`未知参数：${arg}`);
    }
  }
  if (!opts.url || !opts.out) throw new Error('必须提供 --url 与 --out');
  return opts;
}

/* --------------------------- 最小 CDP 客户端 --------------------------- */

/**
 * 极简 CDP 会话：只做 id/方法/结果三元组与事件回调，够用即可。
 * 之所以自己写而不是用 puppeteer：本项目禁止引入渲染/浏览器库，且这只服务于验收。
 */
class CdpSession {
  constructor(wsUrl) {
    this.wsUrl = wsUrl;
    this.nextId = 1;
    this.pending = new Map();
    this.listeners = new Map();
  }

  connect() {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(this.wsUrl);
      this.ws = ws;
      ws.addEventListener('open', () => resolve());
      ws.addEventListener('error', (err) => reject(new Error(`WebSocket 连接失败：${err.message || err.type}`)));
      ws.addEventListener('message', (event) => {
        let msg;
        try {
          msg = JSON.parse(typeof event.data === 'string' ? event.data : String(event.data));
        } catch {
          return; // 非法帧直接忽略：验收工具不该因为一条脏消息崩掉
        }
        if (msg.id && this.pending.has(msg.id)) {
          const { resolve, reject } = this.pending.get(msg.id);
          this.pending.delete(msg.id);
          if (msg.error) reject(new Error(`CDP ${msg.error.message}`));
          else resolve(msg.result);
          return;
        }
        if (msg.method && this.listeners.has(msg.method)) {
          for (const fn of this.listeners.get(msg.method)) fn(msg.params);
        }
      });
    });
  }

  send(method, params = {}) {
    const id = this.nextId++;
    const payload = JSON.stringify({ id, method, params });
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(payload);
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`CDP 调用超时：${method}`));
        }
      }, 20000);
    });
  }

  on(method, fn) {
    if (!this.listeners.has(method)) this.listeners.set(method, []);
    this.listeners.get(method).push(fn);
  }

  /**
   * 在页面里求值并把结果按值带回。
   * `awaitPromise: true` 让页面里的 Promise 能直接用；表达式必须自带兜底，避免抛异常时
   * 只能拿到 undefined 而看不出原因（抛错时返回 {error: 文本}）。
   */
  async evaluate(expression) {
    const res = await this.send('Runtime.evaluate', {
      expression: `(() => { try { return { ok: true, value: (${expression}) }; } catch (e) { return { ok: false, error: String(e && e.message || e) }; } })()`,
      returnByValue: true,
      awaitPromise: true,
    });
    if (res.exceptionDetails) return { ok: false, error: res.exceptionDetails.text || 'unknown' };
    return res.result ? res.result.value : { ok: false, error: '空的 CDP 结果' };
  }

  close() {
    try { this.ws.close(); } catch { /* 关闭失败无所谓：进程即将退出 */ }
  }
}

/* ------------------------------ 辅助函数 ------------------------------ */

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** 找一个可用的本地端口：用 Node 自己开一个临时监听再关掉，避免和别的进程撞车。 */
function pickPort() {
  return new Promise((resolve, reject) => {
    const net = require('node:net');
    const srv = net.createServer();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const port = srv.address().port;
      srv.close(() => resolve(port));
    });
  });
}

function resolveChrome(explicit) {
  if (explicit) return explicit;
  const candidates = ['chromium', 'chromium-browser', 'google-chrome', 'google-chrome-stable'];
  for (const name of candidates) {
    const found = require('node:child_process').execSync(`command -v ${name} || true`, { encoding: 'utf8' }).trim();
    if (found) return found;
  }
  throw new Error('找不到 chromium/google-chrome，可用 --chrome 或 CHROME 环境变量指定');
}

/** 轮询 /json/list 直到出现页面目标（浏览器刚起来时列表可能是空的）。 */
async function waitForTarget(port, deadline) {
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}/json/list`);
      const list = await res.json();
      const page = list.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
      if (page) return page;
    } catch {
      // 端口还没起来，继续等
    }
    await sleep(120);
  }
  throw new Error('等待 chromium 调试目标超时');
}

/* -------------------------------- 主流程 -------------------------------- */

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const chrome = resolveChrome(opts.chrome);

  // 工作区内的可写 HOME 与 user-data-dir：本机 `$HOME/.cargo` 只读的同类坑在 chromium 上
  // 表现为 "Failed to create headless user data directory container"，必须显式给可写路径。
  const root = process.cwd();
  const tmp = path.join(root, '.tmp');
  fs.mkdirSync(tmp, { recursive: true });
  const profile = path.join(tmp, `shot_udd_${process.pid}`);
  const home = path.join(tmp, `shot_home_${process.pid}`);
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(path.dirname(path.resolve(opts.out)), { recursive: true });

  const port = await pickPort();
  const args = [
    '--headless=new',
    '--disable-gpu',
    '--no-sandbox',
    '--hide-scrollbars',
    '--allow-file-access-from-files',
    '--disable-dev-shm-usage',
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profile}`,
    `--window-size=${opts.width},${opts.height}`,
    'about:blank',
  ];
  const child = spawn(chrome, args, {
    stdio: ['ignore', 'ignore', 'pipe'],
    env: { ...process.env, HOME: home },
  });
  let stderr = '';
  if (child.stderr) child.stderr.on('data', (buf) => { stderr += String(buf); });

  const failures = [];
  let status = null;
  let warningVisible = null;
  let bannerDiag = null;
  let ready = false;
  let pageError = null;

  try {
    const deadline = Date.now() + opts.waitMs;
    const target = await waitForTarget(port, deadline);
    const cdp = new CdpSession(target.webSocketDebuggerUrl);
    await cdp.connect();
    const pageErrors = [];
    cdp.on('Runtime.exceptionThrown', (p) => {
      pageErrors.push((p.exceptionDetails && p.exceptionDetails.text) || '页面异常');
    });
    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    // 视口默认交给 `--window-size`：实测 `Emulation.setDeviceMetricsOverride` 会在页面加载后
    // 触发一次重新布局，把 flex 列里的提示横幅挤成 19px 高（正常约 60px）并被裁掉，
    // 于是「版本不匹配警告」在截图里消失——这不是 UI 缺陷，而是度量覆盖造成的布局差异。
    // 需要严格像素尺寸时才用 `--fixed-viewport` 打开它。
    if (opts.fixedViewport) {
      await cdp.send('Emulation.setDeviceMetricsOverride', {
        width: opts.width, height: opts.height, deviceScaleFactor: 1, mobile: false,
      });
    }
    await cdp.send('Page.navigate', { url: opts.url });

    // 页面自己会在解析完成后置 window.__QFR_READY__（webui-spec §7 约定的无头钩子）。
    while (Date.now() < deadline) {
      const probe = await cdp.evaluate('({ ready: !!window.__QFR_READY__, error: window.__QFR_ERROR__ || null })');
      if (probe.ok) {
        if (probe.value && probe.value.error && !opts.expectReady) { pageError = probe.value.error; break; }
        if (probe.value && probe.value.ready) { ready = true; break; }
      }
      await sleep(150);
    }

    if (opts.expectReady && !ready) failures.push('超时：页面未上报 window.__QFR_READY__');
    if (!opts.expectReady && !pageError) {
      const probe = await cdp.evaluate('window.__QFR_ERROR__ || null');
      pageError = probe.ok ? probe.value : null;
    }

    const statusProbe = await cdp.evaluate('window.__QFR_STATUS__ ? JSON.parse(JSON.stringify(window.__QFR_STATUS__)) : null');
    status = statusProbe.ok ? statusProbe.value : null;
    const warnProbe = await cdp.evaluate(
      "(() => { const el = document.getElementById('warningBar'); return !!(el && !el.hidden && el.offsetParent !== null); })()",
    );
    warningVisible = warnProbe.ok ? warnProbe.value : null;
    // 诊断信息：截图里「横幅看不到」时，这几项能立刻区分「DOM 没显示」「被样式藏了」
    // 「其实显示正常、只是没进合成表面」三种完全不同的原因，避免误判成 UI 缺陷。
    const diagProbe = await cdp.evaluate(
      `(() => { const el = document.getElementById('warningBar');
        if (!el) return { present: false };
        const cs = getComputedStyle(el); const r = el.getBoundingClientRect();
        const parent = el.parentElement;
        const pcs = parent ? getComputedStyle(parent) : null;
        const main = document.querySelector('main');
        return { present: true, hidden: !!el.hidden, opacity: cs.opacity, display: cs.display,
                 visibility: cs.visibility, position: cs.position, top: Math.round(r.top),
                 height: Math.round(r.height), width: Math.round(r.width),
                 scrollY: Math.round(window.scrollY), innerH: window.innerHeight,
                 children: el.children.length, textLen: el.textContent.length,
                 parentTag: parent ? parent.tagName : null, parentOverflow: pcs ? pcs.overflow : null,
                 parentDisplay: pcs ? pcs.display : null, bodyH: document.body.getBoundingClientRect().height,
                 mainH: main ? Math.round(main.getBoundingClientRect().height) : null,
                 ulH: (() => { const u = el.querySelector('ul'); return u ? Math.round(u.getBoundingClientRect().height) : null; })() }; })()`,
    );
    bannerDiag = diagProbe.ok ? diagProbe.value : null;

    if (opts.requireStatus) {
      if (!status) failures.push('缺少 window.__QFR_STATUS__');
      else {
        if (!(status.rendered > 0)) failures.push(`rendered=${status.rendered}，Canvas 没有画出任何帧`);
        if (!opts.allowErrors && Array.isArray(status.errors) && status.errors.length > 0) {
          failures.push(`status.errors 非空：${status.errors.join(' | ')}`);
        }
      }
    }
    if (opts.requireWarnings) {
      const count = status && Array.isArray(status.warnings) ? status.warnings.length : 0;
      if (count < 1) failures.push('期望出现版本/格式告警，但 status.warnings 为空');
      if (warningVisible !== true) failures.push('期望告警横幅可见（#warningBar 未显示）');
    }
    if (pageErrors.length > 0) failures.push(`页面抛出未捕获异常：${pageErrors.join(' | ')}`);

    // 等「合成器真的产出一帧」再截图（这是一处实测踩出来的坑）：
    // 页面在加载回调里同时把告警横幅插入 DOM 并画好 Canvas，此刻布局已完成、
    // `#warningBar` 的 offsetParent 也非空，但**立刻截图拿到的仍是插入横幅之前的那张合成
    // 表面**——同一 URL 的 CLI 截图与「就绪即截」的 CDP 截图字节完全相同、都缺横幅，而多等
    // 一会儿再截就能看到（webui 自查脚本中途做了别的断言，天然多等了片刻，所以它有）。
    // 因此这里显式用「两次 rAF + 固定沉降时间」换一次呈现，避免把工具时序误判成 UI 缺陷。
    // 关键一步：先派发一次 resize，强制页面重新布局并提交一帧新画面。
    // 实测（同一 URL、同一 chromium）：只等 rAF / 只加长等待时间都拿不到横幅，而「让页面
    // 重排一次」后横幅就出现在截图里（警示色像素 1249，与 webui 自查截图一致）。
    // 原因在于回放加载后的 DOM 插入发生在首帧之后，而截图取的是上一次提交的合成表面。
    await cdp.evaluate('(() => { window.dispatchEvent(new Event(\'resize\')); return true; })()');
    await cdp.evaluate('new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(resolve, 0))))');
    await sleep(opts.settleMs);

    const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
    const bytes = Buffer.from(shot.data, 'base64');
    fs.writeFileSync(opts.out, bytes);
    cdp.close();

    const report = {
      ok: failures.length === 0,
      url: opts.url,
      out: opts.out,
      bytes: bytes.length,
      ready,
      warningVisible,
      bannerDiag,
      status,
      failures,
      stderrTail: stderr ? stderr.split('\n').slice(-3).join(' / ') : '',
    };
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
    return failures.length === 0 ? 0 : 1;
  } finally {
    try { child.kill('SIGKILL'); } catch { /* 进程可能已经退出 */ }
    // 清理本次运行的临时 profile（失败也无所谓，不影响验收结论）。
    try { fs.rmSync(profile, { recursive: true, force: true }); } catch { /* ignore */ }
    try { fs.rmSync(home, { recursive: true, force: true }); } catch { /* ignore */ }
  }
}

main()
  .then((code) => process.exit(code))
  .catch((err) => {
    process.stderr.write(`shot.js 失败：${err && err.message ? err.message : err}\n`);
    process.exit(2);
  });
