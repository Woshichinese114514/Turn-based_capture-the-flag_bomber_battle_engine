/* =============================================================================
 * webui/js/main.js —— 装配层（模块接线、加载流程、渲染调度、无头验收钩子）
 * -----------------------------------------------------------------------------
 * 职责：
 *   1. 收集 DOM 引用并注入各模块（render/log/controls）；
 *   2. 订阅 state 的变更事件，把「状态变化」翻译成「重绘 + 面板更新」；
 *   3. 播放循环（requestAnimationFrame + 时间累积，不阻塞主线程）；
 *   4. 加载流程：URL 参数 / 文件选择 / 拖拽 → parser → state.loadReplay → 首帧渲染；
 *   5. 无头验收钩子：window.__QFR_READY__ / __QFR_STATUS__ / __QFR_ERROR__。
 * 依赖：所有其他模块（必须最后加载）。
 *
 * 设计意图与为什么：
 *   1. 渲染用 rAF 合并：一次操作可能触发多个 state 事件（例如「跳到末帧」同时改 index 与
 *      playing），合并到下一帧只画一次，避免重复计算布局。
 *   2. 播放用「时间累积」而不是 setInterval(fixed)：setInterval 会漂移，且页面被节流时
 *      会积压回调；累积法在慢机器上自动降帧，速度快时也不会连播多帧。
 *   3. 出错（文件坏/URL 打不开）时不销毁控件：进度条与控制保留可用，用户可以直接换文件。
 * ========================================================================== */
(function (global) {
  'use strict';

  var QFR = global.QFR;
  var state = QFR.state;

  var els = {};

  /* --------------------------- DOM 引用收集 ------------------------------- */

  function collect() {
    var ids = [
      'fileInput', 'canvas', 'dropZone', 'btnLoad', 'noticeBar', 'errorBanner', 'warningBar',
      'logList', 'logEmpty', 'scoreBoard', 'resultBox', 'filterSelect',
      'infoTick', 'infoFile', 'infoMap', 'infoUnits', 'infoFlags', 'infoBombs',
      'progress', 'btnFirst', 'btnPrev50', 'btnPrev10', 'btnPrev1', 'btnPlay', 'btnNext1',
      'btnNext10', 'btnNext50', 'btnLast', 'btnReset', 'tickInput', 'tickTotal', 'btnGo',
      'speedButtons', 'statusText', 'btnZoomIn', 'btnZoomOut', 'btnZoomFit', 'headerFile',
      'headerTeams', 'headerTicks'
    ];
    for (var i = 0; i < ids.length; i++) {
      els[ids[i]] = document.getElementById(ids[i]);
    }
    // 别名：render/controls/log 需要的字段名与 DOM id 不同，这里做一次适配。
    els.canvasEl = els.canvas;
    els.stage = els.dropZone;
    els.speedRadioGroup = els.speedButtons;
    els.speedButtons = els.speedButtons;
  }

  /* ------------------------------ 渲染调度 -------------------------------- */

  var renderPending = false;

  // 请求一次「下一帧重绘」；同一帧内多次请求只画一次。
  function scheduleRender() {
    if (renderPending) return;
    renderPending = true;
    var run = function () {
      renderPending = false;
      redraw();
    };
    if (global.requestAnimationFrame) global.requestAnimationFrame(run);
    else global.setTimeout(run, 16);
  }

  function redraw() {
    var replay = state.data.replay;
    var frame = state.currentFrame();
    QFR.render.render(frame);
    // 面板/日志只在「数据或 tick 变了」时更新，这里是最外层唯一触发点。
    QFR.log.renderInfo(replay || emptyReplay(), frame, state.data.accessors || noopAccessors(), state.data.currentIndex);
    QFR.log.renderEvents(frame, state.data.accessors, state.data.filterTeam);
    QFR.controls.applyState(state);
    updateHeader();
  }

  // 未加载任何回放时给 log.renderInfo 一个空壳，避免它到处判空。
  function emptyReplay() {
    return {
      init: { map: { width: 0, height: 0, terrain: [] }, teams: [], max_ticks: 0 },
      frames: [],
      end: { __present: false, scores: [], kills: [], deaths: [], winner: null, ai_names: [] },
      warnings: [], errors: [], stats: {}
    };
  }

  function noopAccessors() {
    return {
      aiName: function () { return '未知'; },
      teamOfUnit: function () { return null; },
      teamOfUnitInFrame: function () { return null; }
    };
  }

  function updateHeader() {
    var replay = state.data.replay;
    if (!replay) return;
    if (els.headerFile) els.headerFile.textContent = state.data.sourceName || '（未命名回放）';
    if (els.headerTeams) els.headerTeams.textContent = replay.init.teams.length + ' 队';
    if (els.headerTicks) els.headerTicks.textContent = replay.frames.length + ' 帧';
  }

  /* ------------------------------ 播放循环 -------------------------------- */

  var lastTime = 0;
  var accumulator = 0;

  function tickLoop(now) {
    if (!state.data.playing) { loopRunning = false; return; }
    if (!lastTime) lastTime = now;
    var dt = now - lastTime;
    lastTime = now;

    // 单帧最多推进 4 帧：标签页被挂起后恢复时，now-lastTime 可能很大，
    // 不设上限会瞬间冲到末帧（体验上像「跳帧」）。
    var interval = 1000 / (state.BASE_TICKS_PER_SECOND * state.data.speed);
    accumulator = Math.min(accumulator + dt, interval * 4);

    var advanced = false;
    while (accumulator >= interval) {
      accumulator -= interval;
      if (state.data.currentIndex >= state.lastIndex()) {
        state.setPlaying(false, 'reached-end');
        break;
      }
      state.data.currentIndex++;
      advanced = true;
    }
    if (advanced) {
      state.data.selectedBombId = null;
      onStateEvent('tick'); // 手动触发（直接改 index 不经过 state.goto）
    }
    if (state.data.playing) {
      global.requestAnimationFrame(tickLoop);
    } else {
      loopRunning = false;
    }
  }

  var loopRunning = false;
  function startLoop() {
    if (loopRunning) return;
    loopRunning = true;
    lastTime = 0;
    accumulator = 0;
    if (global.requestAnimationFrame) global.requestAnimationFrame(tickLoop);
    else global.setInterval(tickLoop, 16);
  }

  function stopLoop() { loopRunning = false; }

  /* ---------------------------- state 事件订阅 ---------------------------- */

  function onStateEvent(type) {
    switch (type) {
      case 'loaded':
        // 新回放：重建过滤器选项（队伍数变了）与警告横幅。
        rebuildFilterOptions();
        renderWarnings();
        break;
      case 'tick':
        // ★ 关键：只有 tick 变化才更新事件日志（规范 §5）；渲染用 rAF 合并。
        scheduleRender();
        break;
      case 'playing':
        if (state.data.playing) startLoop(); else stopLoop();
        scheduleRender();
        break;
      case 'filter':
        QFR.log.renderEvents(state.currentFrame(), state.data.accessors, state.data.filterTeam);
        break;
      case 'speed':
      case 'select':
      case 'reset':
        scheduleRender();
        break;
      case 'error':
        renderErrorBanner();
        break;
      default:
        scheduleRender();
    }
  }

  function rebuildFilterOptions() {
    if (!els.filterSelect) return;
    var replay = state.data.replay;
    while (els.filterSelect.firstChild) els.filterSelect.removeChild(els.filterSelect.firstChild);

    var all = document.createElement('option');
    all.value = 'all';
    all.textContent = '全部队伍';
    els.filterSelect.appendChild(all);

    var teams = replay ? replay.init.teams.length : 0;
    for (var t = 0; t < teams; t++) {
      var opt = document.createElement('option');
      opt.value = String(t);
      // AI 名字是不可信数据 → textContent（这里 option 的文本同样不能拼 innerHTML）
      opt.textContent = QFR.teamLabel(t) + '（' + (state.data.accessors ? state.data.accessors.aiName(t) : '未知') + '）';
      els.filterSelect.appendChild(opt);
    }
    els.filterSelect.value = 'all';
  }

  /* ------------------------------ 提示与横幅 ------------------------------ */

  // 警告条：把 parser 的 warnings 去重后逐条列出（版本不符、坏行、地形补齐…）。
  function renderWarnings() {
    if (!els.warningBar) return;
    var replay = state.data.replay;
    while (els.warningBar.firstChild) els.warningBar.removeChild(els.warningBar.firstChild);
    if (!replay || !replay.warnings.length) {
      els.warningBar.hidden = true;
      return;
    }
    els.warningBar.hidden = false;

    var title = document.createElement('div');
    title.className = 'banner-title';
    title.textContent = '⚠ 提示（' + replay.warnings.length + ' 条，不影响渲染）';
    els.warningBar.appendChild(title);

    var ul = document.createElement('ul');
    for (var i = 0; i < replay.warnings.length; i++) {
      var li = document.createElement('li');
      // 规范 §2/§7：坏行/版本不符都必须有「可视提示」。这里的文本全部来自程序常量与
      // 回放里的数字，仍然统一用 textContent 写入（防御性：将来若把引擎文本放进来也安全）。
      li.textContent = replay.warnings[i].text;
      ul.appendChild(li);
    }
    els.warningBar.appendChild(ul);
  }

  function renderErrorBanner() {
    if (!els.errorBanner) return;
    var msg = state.data.loadError;
    while (els.errorBanner.firstChild) els.errorBanner.removeChild(els.errorBanner.firstChild);
    if (!msg) { els.errorBanner.hidden = true; return; }
    els.errorBanner.hidden = false;
    var strong = document.createElement('strong');
    strong.textContent = '✕ 打不开这个回放：';
    els.errorBanner.appendChild(strong);
    var span = document.createElement('span');
    span.textContent = msg;
    els.errorBanner.appendChild(span);
    var hint = document.createElement('div');
    hint.className = 'error-hint';
    hint.textContent = '仍可用「选择文件」按钮或把 .jsonl 拖到页面上重新加载；进度条与控制保持可用。';
    els.errorBanner.appendChild(hint);
  }

  // 临时提示（可自动消失）：用于「跳过 N 行」「tick 越界」这类操作反馈。
  var noticeTimer = null;
  function showNotice(text, level) {
    if (!els.noticeBar) return;
    while (els.noticeBar.firstChild) els.noticeBar.removeChild(els.noticeBar.firstChild);
    els.noticeBar.className = 'notice ' + (level || 'info');
    els.noticeBar.hidden = false;
    els.noticeBar.textContent = text;
    if (noticeTimer) global.clearTimeout(noticeTimer);
    noticeTimer = global.setTimeout(function () { els.noticeBar.hidden = true; }, 6000);
  }

  function clearNotice() {
    if (els.noticeBar) els.noticeBar.hidden = true;
    if (noticeTimer) { global.clearTimeout(noticeTimer); noticeTimer = null; }
  }

  /* -------------------------------- 加载流程 ------------------------------ */

  // 加载一段回放文本（来自文件或 URL）。所有解析/校验都在这里收口。
  function loadText(text, sourceName) {
    var result = QFR.parser.parseText(text);
    QFR.parser.buildUnitTeamIndex(result);
    result.meta = { sourceName: sourceName };

    // 坏行提示：规范要求「已跳过 N 行」必须可见。
    for (var i = 0; i < result.errors.length; i++) showNotice(result.errors[i].text, 'error');

    state.loadReplay(result, sourceName);

    // 致命情况（完全没有可用数据）：显示错误横幅，但控件保持可用，地图用兜底渲染。
    if (result.fatal) {
      state.setLoadError('文件里没有任何可用的回放数据（init 与 frame 都不可解析）。');
    } else if (result.errors.length === 0) {
      clearNotice();
    }

    // 首帧渲染完成后打就绪标志（无头验收用）。
    redraw();
    signalReady(sourceName);
  }

  function loadFile(file) {
    var reader = new FileReader();
    reader.onload = function () {
      try {
        loadText(String(reader.result), file.name || '（已选文件）');
        // 回写标题：方便同时开多个页面比对
        document.title = '抢旗人回放查看器 · ' + (file.name || '');
      } catch (e) {
        failLoad('解析文件失败：' + (e && e.message ? e.message : e));
      }
    };
    reader.onerror = function () {
      failLoad('读取文件失败：' + (file && file.name ? file.name : '未知文件'));
    };
    try {
      reader.readAsText(file, 'utf-8');
    } catch (e) {
      failLoad('读取文件失败：' + (e && e.message ? e.message : e));
    }
  }

  function loadUrl(url) {
    QFR.parser.loadUrl(url).then(function (res) {
      loadText(res.text, decodeName(url));
      document.title = '抢旗人回放查看器 · ' + decodeName(url);
    })['catch'](function (err) {
      failLoad(err && err.message ? err.message : String(err));
    });
  }

  function decodeName(url) {
    try { return decodeURIComponent(String(url).split('/').pop() || url); }
    catch (e) { return String(url); }
  }

  // 加载失败：显示横幅 + 状态钩子；页面本身不崩、控件不锁死。
  function failLoad(message) {
    state.setLoadError(message);
    QFR.signalError(message);
    if (global.__QFR_STATUS__) global.__QFR_STATUS__.error = message;
    showNotice(message, 'error');
    QFR.controls.applyState(state);
  }

  /* ---------------------- 无头验收用的状态钩子 --------------------------- */

  function signalReady(sourceName) {
    var replay = state.data.replay;
    var status = {
      file: sourceName || '',
      ticks: replay ? replay.frames.length : 0,
      totalTicks: replay ? replay.frames.length : 0,
      teams: replay ? replay.init.teams.length : 0,
      currentTick: state.currentFrame() ? state.currentFrame().tick : 0,
      currentIndex: state.data.currentIndex,
      speed: state.data.speed,
      playing: state.data.playing,
      warnings: replay ? replay.warnings.map(function (w) { return w.text; }) : [],
      errors: replay ? replay.errors.map(function (e) { return e.text; }) : [],
      badLines: replay ? replay.stats.badLines : 0,
      map: replay ? { width: replay.init.map.width, height: replay.init.map.height } : null,
      rendered: QFR.render.stats.drawnUnits,
      error: null
    };
    QFR.signalReady(status);
  }

  // 供无头脚本/调试用：重新计算尺寸并渲染（resize 后调用）。
  function refreshStatus() {
    redraw();
    signalReady(state.data.sourceName);
    return global.__QFR_STATUS__;
  }

  /* --------------------------------- 启动 --------------------------------- */

  function boot() {
    collect();

    // 模块接线（依赖注入，避免模块之间互相 require）
    QFR.render.bind({ canvas: els.canvasEl }, state);
    QFR.log.bindDom({
      logList: els.logList,
      logEmpty: els.logEmpty,
      scoreBoard: els.scoreBoard,
      resultBox: els.resultBox,
      infoTick: els.infoTick,
      infoFile: els.infoFile,
      infoMap: els.infoMap,
      infoUnits: els.infoUnits,
      infoFlags: els.infoFlags,
      infoBombs: els.infoBombs
    });
    QFR.controls.bind(els);

    // 状态变化 → 统一入口
    state.onChange(onStateEvent);

    // 「选择文件」按钮：点击触发隐藏的 file input
    if (els.btnLoad && els.fileInput) {
      els.btnLoad.addEventListener('click', function () { els.fileInput.click(); });
    }

    // 窗口尺寸变化：格子自适应（cssSize 每次渲染都会重新读布局尺寸，所以只需触发重绘）。
    var onResize = function () { scheduleRender(); };
    global.addEventListener('resize', onResize);
    if (global.ResizeObserver && els.stage) {
      // ResizeObserver 能捕获「侧栏折叠」等不触发 window.resize 的尺寸变化。
      try { new global.ResizeObserver(onResize).observe(els.stage); } catch (e) { /* 忽略 */ }
    }

    // 初始一帧：先画空状态，保证页面不会一片死白。
    redraw();
    rebuildFilterOptions();

    // URL 参数：?replay=<url>&tick=N（规范 §7 无头验收入口）
    var replayUrl = QFR.query('replay');
    var tickParam = QFR.query('tick');
    var errParam = QFR.query('err');

    // ?err=... 是给无头验收用的「人为触发错误」开关（用于验证错误横幅与 __QFR_ERROR__）。
    if (errParam) {
      failLoad(errParam);
      return;
    }

    if (replayUrl) {
      QFR.parser.loadUrl(replayUrl).then(function (res) {
        loadText(res.text, decodeName(replayUrl));
        if (tickParam !== null) {
          var t = parseInt(tickParam, 10);
          if (isFinite(t)) {
            // tick 参数按「tick 编号（1 基）」解释，clamp 到合法范围（0 表示首帧）。
            // 注意：访问器保存在 state.data.accessors 上（不是 state 上），这里必须取 state.data。
            var acc = state.data.accessors;
            var idx = acc ? acc.indexOfTick(Math.max(1, t)) : 0;
            state.goto(idx < 0 ? state.lastIndex() : idx);
            redraw();
            signalReady(state.data.sourceName);
          }
        }
      })['catch'](function (err) {
        failLoad(err && err.message ? err.message : String(err));
      });
    }
  }

  // 暴露 loader 给 controls.js（文件/拖拽入口）；同时暴露测试钩子。
  QFR.loader = {
    loadFile: loadFile,
    loadUrl: loadUrl,
    showNotice: showNotice,
    clearNotice: clearNotice,
    failLoad: failLoad
  };
  QFR.app = {
    refreshStatus: refreshStatus,
    redraw: redraw,
    scheduleRender: scheduleRender,
    loadText: loadText,
    enableTestError: function (msg) { failLoad(msg || '（测试用错误）'); }
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})(window);
