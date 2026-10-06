/* =============================================================================
 * webui/js/controls.js —— 所有用户交互入口（文件、按钮、进度条、键盘、画布点击）
 * -----------------------------------------------------------------------------
 * 职责：把 DOM 事件翻译成 QFR.state 的状态变更调用；控件外观同步由 applyState 负责。
 * 依赖：core.js、state.js、render.js（仅做坐标换算点选）、main.js 暴露的 QFR.loader。
 *
 * 设计意图与为什么：
 *   1. 控件层绝不直接改 currentIndex/playing —— 所有入口都走 state 的函数，
 *      保证「进度条拖动到末尾」和「按 End 键」得到完全一致的状态。
 *   2. 进度条用 input[type=range]（原生可拖动 + 可键盘操作 + 无依赖），
 *      input 事件高频触发 → state.goto 内部有 clamp 与「同值直接返回」保护，不会抖动。
 *   3. 键盘处理只在 document 上监听一次；输入框/选择框获得焦点时不抢按键
 *      （否则在 tick 输入框里按左右箭头会变成步进而不是移动光标）。
 *   4. 拖拽文件用 dragenter/dragover/dragleave 计数（dragleave 会在子元素上触发，
 *      不计数会导致高亮状态闪个不停）。
 * ========================================================================== */
(function (global) {
  'use strict';

  var QFR = global.QFR;
  var els = {};
  var dragDepth = 0;
  var SPEED_RADIOS = null;

  function $(id) { return document.getElementById(id); }

  function bind(refs) {
    els = refs;

    /* --------------------------- 文件选择与拖拽 --------------------------- */

    if (els.fileInput) {
      els.fileInput.addEventListener('change', function () {
        var file = els.fileInput.files && els.fileInput.files[0];
        if (!file) return;
        // 选完立刻清空 value：否则「连续两次选同一个文件」不会再触发 change 事件。
        QFR.loader.loadFile(file);
        els.fileInput.value = '';
      });
    }

    if (els.dropZone) {
      var stage = els.dropZone;

      stage.addEventListener('dragenter', function (ev) {
        ev.preventDefault();
        dragDepth++;
        stage.classList.add('dragging');
      });
      stage.addEventListener('dragover', function (ev) {
        // 必须 preventDefault，否则浏览器默认行为是「打开该文件」而不是交给我们的 handler。
        ev.preventDefault();
        if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'copy';
      });
      stage.addEventListener('dragleave', function (ev) {
        ev.preventDefault();
        dragDepth = Math.max(0, dragDepth - 1);
        if (dragDepth === 0) stage.classList.remove('dragging');
      });
      stage.addEventListener('drop', function (ev) {
        ev.preventDefault();
        dragDepth = 0;
        stage.classList.remove('dragging');
        var dt = ev.dataTransfer;
        if (!dt || !dt.files || !dt.files.length) {
          QFR.loader.showNotice('拖入的内容里没有文件（请拖 .jsonl 回放文件）。', 'warn');
          return;
        }
        var file = dt.files[0];
        // 非 .jsonl：给出可读提示但仍尝试解析（有些引擎导出没有扩展名）。
        if (!/\.(jsonl|json|ndjson|txt)$/i.test(file.name || '')) {
          QFR.loader.showNotice('「' + (file.name || '未知文件') + '」不是 .jsonl 回放文件，仍会尝试解析。', 'warn');
        }
        // 多文件拖拽：只取第一个，并提示（规范只要求单文件）。
        if (dt.files.length > 1) {
          QFR.loader.showNotice('拖入了 ' + dt.files.length + ' 个文件，只加载第一个。', 'warn');
        }
        QFR.loader.loadFile(file);
      });
    }

    /* ------------------------------ 播放控制 ------------------------------ */

    on(els.btnPrev1, 'click', function () { QFR.state.step(-1); });
    on(els.btnNext1, 'click', function () { QFR.state.step(1); });
    on(els.btnPrev10, 'click', function () { QFR.state.step(-10); });
    on(els.btnNext10, 'click', function () { QFR.state.step(10); });
    on(els.btnPrev50, 'click', function () { QFR.state.step(-50); });
    on(els.btnNext50, 'click', function () { QFR.state.step(50); });
    on(els.btnFirst, 'click', function () { QFR.state.first(); });
    on(els.btnLast, 'click', function () { QFR.state.last(); });
    on(els.btnPlay, 'click', function () { QFR.state.togglePlaying(); });
    on(els.btnReset, 'click', function () { QFR.state.reset(); });

    /* -------------------------------- 进度条 ------------------------------ */

    if (els.progress) {
      els.progress.addEventListener('input', function () {
        // range 的值是「帧下标」，直接对应 state.currentIndex，避免 tick 与下标两套编号混淆。
        QFR.state.goto(parseInt(els.progress.value, 10));
      });
    }

    /* ------------------------------ tick 跳转 ----------------------------- */

    if (els.tickInput) {
      els.tickInput.addEventListener('keydown', function (ev) {
        if (ev.key !== 'Enter') return;
        ev.preventDefault();
        jumpToInputTick();
      });
      // 失焦时也提交一次：用户输入后直接点别处也能生效
      els.tickInput.addEventListener('change', jumpToInputTick);
    }
    on(els.btnGo, 'click', jumpToInputTick);

    /* -------------------------------- 速度 -------------------------------- */

    SPEED_RADIOS = els.speedButtons ? els.speedButtons.querySelectorAll('input[type=radio]') : null;
    if (SPEED_RADIOS) {
      for (var i = 0; i < SPEED_RADIOS.length; i++) {
        SPEED_RADIOS[i].addEventListener('change', function (ev) {
          QFR.state.setSpeed(parseFloat(ev.target.value));
        });
      }
    }

    /* ------------------------------ 队伍过滤 ------------------------------ */

    on(els.filterSelect, 'change', function () {
      var v = els.filterSelect.value;
      QFR.state.setFilterTeam(v === 'all' ? 'all' : parseInt(v, 10));
    });

    /* ------------------------------ 缩放按钮 ------------------------------ */

    on(els.btnZoomIn, 'click', function () {
      QFR.render.setZoom(QFR.render.getLayout().zoom + 0.25, 'manual');
      QFR.render.render(QFR.state.currentFrame());
    });
    on(els.btnZoomOut, 'click', function () {
      QFR.render.setZoom(QFR.render.getLayout().zoom - 0.25, 'manual');
      QFR.render.render(QFR.state.currentFrame());
    });
    on(els.btnZoomFit, 'click', function () {
      QFR.render.setZoom(1, 'auto');
      QFR.render.render(QFR.state.currentFrame());
    });

    /* --------------------------- 画布点击选炸弹 --------------------------- */

    if (els.canvas) {
      els.canvas.addEventListener('click', function (ev) {
        var frame = QFR.state.currentFrame();
        if (!frame) return;
        var rect = els.canvas.getBoundingClientRect();
        // clientX/Y 是视口坐标，先减掉画布左上角才是画布内坐标
        var g = QFR.render.pointToGrid(ev.clientX - rect.left, ev.clientY - rect.top);
        if (!g) return;
        var hit = null;
        for (var i = 0; i < frame.bombs.length; i++) {
          if (frame.bombs[i].x === g.x && frame.bombs[i].y === g.y) { hit = frame.bombs[i]; break; }
        }
        QFR.state.setSelectedBomb(hit ? hit.id : null);
      });
    }

    /* -------------------------------- 键盘 -------------------------------- */

    document.addEventListener('keydown', onKeyDown);
  }

  function on(el, evt, fn) {
    if (el) el.addEventListener(evt, fn);
  }

  function jumpToInputTick() {
    if (!els.tickInput) return;
    var v = parseInt(els.tickInput.value, 10);
    var state = QFR.state.data;
    if (!state.replay) return;
    if (!isFinite(v)) {
      QFR.loader.showNotice('请输入数字 tick（1 ~ ' + QFR.state.frameCount() + '）。', 'warn');
      return;
    }
    // 用户输入的是 tick 编号（1 基，与画面 HUD 一致）；超出范围时夹到边界并提示。
    var maxTick = QFR.state.frameCount();
    var clamped = QFR.clamp(v, 1, maxTick);
    if (clamped !== v) {
      QFR.loader.showNotice('tick ' + v + ' 超出范围，已跳到 ' + clamped + '（共 ' + maxTick + ' 帧）。', 'warn');
    }
    QFR.state.gotoTick(clamped);
  }

  function onKeyDown(ev) {
    // 焦点在输入框/下拉框里时不抢按键：否则在 tick 输入框里按 ← 会变成步进。
    var tag = ev.target && ev.target.tagName ? ev.target.tagName.toLowerCase() : '';
    if (tag === 'input' && ev.target.type === 'text') return;
    if (tag === 'select' || tag === 'textarea') return;
    if (ev.metaKey || ev.ctrlKey || ev.altKey) return; // 不覆盖浏览器快捷键（Ctrl+R 等）

    switch (ev.key) {
      case ' ':
      case 'Spacebar':
        ev.preventDefault(); // 空格默认会滚动页面
        QFR.state.togglePlaying();
        break;
      case 'ArrowRight':
        ev.preventDefault();
        QFR.state.step(1);
        break;
      case 'ArrowLeft':
        ev.preventDefault();
        QFR.state.step(-1);
        break;
      case 'Home':
        ev.preventDefault();
        QFR.state.first();
        break;
      case 'End':
        ev.preventDefault();
        QFR.state.last();
        break;
      default:
        return;
    }
  }

  /* --------------------------- 控件外观同步 ------------------------------ */

  // 由 main.js 在状态变化后调用：把状态回写到控件（按钮禁用、滑块位置、播放图标等）。
  function applyState(state) {
    var data = state.data;
    var count = state.frameCount();
    var last = state.lastIndex();

    if (els.progress) {
      els.progress.max = String(last);
      els.progress.value = String(data.currentIndex);
      els.progress.disabled = count === 0;
    }
    if (els.btnPrev1) els.btnPrev1.disabled = data.currentIndex <= 0;
    if (els.btnNext1) els.btnNext1.disabled = data.currentIndex >= last;
    if (els.btnPrev10) els.btnPrev10.disabled = data.currentIndex <= 0;
    if (els.btnNext10) els.btnNext10.disabled = data.currentIndex >= last;
    if (els.btnPrev50) els.btnPrev50.disabled = data.currentIndex <= 0;
    if (els.btnNext50) els.btnNext50.disabled = data.currentIndex >= last;
    if (els.btnFirst) els.btnFirst.disabled = data.currentIndex <= 0;
    if (els.btnLast) els.btnLast.disabled = data.currentIndex >= last;

    if (els.btnPlay) {
      els.btnPlay.textContent = data.playing ? '⏸ 暂停' : '▶ 播放';
      els.btnPlay.setAttribute('aria-label', data.playing ? '暂停播放' : '开始播放');
      els.btnPlay.classList.toggle('playing', !!data.playing);
      els.btnPlay.disabled = count === 0;
    }
    if (els.tickInput) {
      els.tickInput.max = String(count);
      // 只在用户没在编辑时同步值，避免把用户正在输入的内容改掉。
      if (document.activeElement !== els.tickInput) {
        var frame = state.currentFrame();
        els.tickInput.value = frame ? String(frame.tick) : '';
      }
    }
    if (els.tickTotal) els.tickTotal.textContent = count ? ('/ ' + count + ' 帧') : '';
    if (els.statusText) {
      els.statusText.textContent = count
        ? ('帧 ' + (data.currentIndex + 1) + ' / ' + count + '　速度 ' + data.speed + 'x' + (data.playing ? '　播放中' : ''))
        : '未加载回放';
    }
    if (els.btnReset) els.btnReset.disabled = count === 0;

    if (SPEED_RADIOS) {
      for (var i = 0; i < SPEED_RADIOS.length; i++) {
        SPEED_RADIOS[i].checked = parseFloat(SPEED_RADIOS[i].value) === data.speed;
      }
    }
    if (els.filterSelect) {
      // 只有加载了回放才知道有几支队；没加载时禁用过滤器。
      els.filterSelect.disabled = !data.replay;
      els.filterSelect.value = String(data.filterTeam);
    }
  }

  QFR.controls = {
    bind: bind,
    applyState: applyState,
    jumpToInputTick: jumpToInputTick,
    onKeyDown: onKeyDown
  };
})(window);
