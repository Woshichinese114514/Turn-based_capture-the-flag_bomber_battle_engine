/* =============================================================================
 * webui/js/state.js —— 状态机（唯一状态源）
 * -----------------------------------------------------------------------------
 * 职责：集中保存 currentIndex / playing / speed / filter 等所有「会变的状态」，
 *       并暴露唯一的改状态入口（goto/step/setPlaying/setSpeed…）。
 * 依赖：core.js。
 *
 * 设计意图（为什么这么做）：
 *   1. 规范 §6 要求「不要用全局裸变量做状态」。所有状态都挂在 QFR.state.data 上，
 *      任何 UI 改动都必须走这里的函数 —— 这样「进度条拖动」「键盘 ←」「输入 tick 跳转」
 *      不可能走出互相矛盾的状态（例如 playing=true 却停在末帧）。
 *   2. change 事件用「监听器数组」实现，模块之间通过事件通信，不互相读内部字段。
 * ========================================================================== */
(function (global) {
  'use strict';

  var QFR = global.QFR;

  // 播放速度档位（规范 §4：0.5/1/2/4x）。基速 1x = 每秒 2.5 帧，约等于「看得清」的速度。
  var SPEEDS = [0.5, 1, 2, 4];
  var BASE_TICKS_PER_SECOND = 2.5;

  var listeners = [];

  var state = {
    // --- 回放数据（由 main.js 装载后写入，其他模块只读） ---
    replay: null,      // parser 的解析结果
    accessors: null,   // parser.createAccessors(replay)

    // --- 播放状态机 ---
    currentIndex: 0,   // 当前帧在 replay.frames 里的下标（0 基）；显示给用户时 +1
    playing: false,
    speed: 1,

    // --- 过滤与展示 ---
    filterTeam: 'all', // 'all' 或队伍 ID（数字）
    selectedBombId: null,

    // --- 加载来源信息（信息面板/状态钩子用） ---
    sourceName: '',    // 文件名或 URL
    loadError: null    // 致命错误文本（页面顶部横幅显示）
  };

  function emit(type) {
    for (var i = 0; i < listeners.length; i++) {
      try {
        listeners[i](type, state);
      } catch (e) {
        // 某个监听器出错不应该让整条 UI 链路挂掉
        if (global.console && console.error) console.error('[QFR.state] listener error:', e);
      }
    }
  }

  function onChange(fn) { listeners.push(fn); }

  /* --------------------------- 状态查询辅助 ------------------------------- */

  function frameCount() {
    return state.replay && state.replay.frames ? state.replay.frames.length : 0;
  }

  function currentFrame() {
    if (!state.replay || !state.replay.frames.length) return null;
    return state.replay.frames[QFR.clamp(state.currentIndex, 0, state.replay.frames.length - 1)];
  }

  function lastIndex() { return Math.max(0, frameCount() - 1); }

  /* ------------------------------ 改状态入口 ------------------------------ */

  // 跳到指定帧下标；跳转时不做任何补间动画（规范 §4：跳转直接画目标帧）。
  // clamp 而不是报错：进度条/输入框都可能给出越界值。
  function goto(index, reason) {
    if (!state.replay) return;
    var idx = QFR.clamp(Math.floor(index), 0, lastIndex());
    if (idx === state.currentIndex && reason !== 'force') return;
    state.currentIndex = idx;
    // 任何跳转都清掉炸弹选中，避免指向已消失的炸弹。
    state.selectedBombId = null;
    emit('tick');
  }

  // 相对步进（±1 / ±10 / ±50）。到末帧自动暂停，避免空转。
  function step(delta) {
    if (!state.replay) return;
    var target = state.currentIndex + delta;
    if (target >= lastIndex()) {
      target = lastIndex();
      if (state.playing) setPlaying(false, 'reached-end');
    }
    if (target < 0) target = 0;
    goto(target, 'step');
  }

  function first() { setPlaying(false, 'first'); goto(0, 'first'); }
  function last() { setPlaying(false, 'last'); goto(lastIndex(), 'last'); }

  // 按 tick 值跳转（用户输入的是 1 基 tick）：O(log n) 二分 → 最近的、不超过该 tick 的帧。
  function gotoTick(tick) {
    if (!state.replay) return;
    // accessors 存在 state.accessors（state.data 上），不是 QFR.state 本体上。
    var acc = state.accessors;
    var idx = acc ? acc.indexOfTick(Math.floor(tick)) : 0;
    if (idx < 0) idx = lastIndex();
    setPlaying(false, 'goto-tick');
    goto(idx, 'goto-tick');
  }

  function setPlaying(playing, reason) {
    var next = !!playing;
    // 已经在末帧还点播放 → 从头开始（用户直觉），而不是什么都不发生。
    if (next && state.currentIndex >= lastIndex() && lastIndex() > 0) {
      state.currentIndex = 0;
      emit('tick');
    }
    if (next === state.playing && reason !== 'force') return;
    state.playing = next;
    emit('playing');
  }

  function togglePlaying() { setPlaying(!state.playing, 'toggle'); }

  function setSpeed(speed) {
    var s = QFR.num(speed, 1);
    // 只接受协议规定的 4 档，其他值就近取整到已知档位（防止 UI 传来奇怪的值）。
    var best = SPEEDS[0], bestDiff = Infinity;
    for (var i = 0; i < SPEEDS.length; i++) {
      var d = Math.abs(SPEEDS[i] - s);
      if (d < bestDiff) { bestDiff = d; best = SPEEDS[i]; }
    }
    if (best === state.speed) return;
    state.speed = best;
    emit('speed');
  }

  function setFilterTeam(team) {
    var next = (team === 'all' || team === null || team === undefined) ? 'all' : QFR.num(team, 'all');
    if (next === state.filterTeam) return;
    state.filterTeam = next;
    emit('filter');
  }

  function setSelectedBomb(id) {
    var next = (id === null || id === undefined) ? null : QFR.num(id, null);
    if (next === state.selectedBombId) return;
    state.selectedBombId = next;
    emit('select');
  }

  // 装载新回放：重置所有与「上一份回放」绑定的状态，防止串味。
  function loadReplay(replay, sourceName) {
    state.replay = replay;
    state.accessors = QFR.parser.createAccessors(replay);
    state.sourceName = sourceName || '';
    state.loadError = null;
    state.currentIndex = 0;
    state.playing = false;
    state.filterTeam = 'all';
    state.selectedBombId = null;
    emit('loaded');
    emit('tick');
  }

  function setLoadError(message) {
    state.loadError = message ? String(message) : null;
    emit('error');
  }

  function reset() {
    state.playing = false;
    state.currentIndex = 0;
    state.filterTeam = 'all';
    state.selectedBombId = null;
    emit('reset');
    emit('tick');
  }

  QFR.state = {
    SPEEDS: SPEEDS,
    BASE_TICKS_PER_SECOND: BASE_TICKS_PER_SECOND,
    data: state,
    onChange: onChange,
    frameCount: frameCount,
    lastIndex: lastIndex,
    currentFrame: currentFrame,
    goto: goto,
    step: step,
    first: first,
    last: last,
    gotoTick: gotoTick,
    setPlaying: setPlaying,
    togglePlaying: togglePlaying,
    setSpeed: setSpeed,
    setFilterTeam: setFilterTeam,
    setSelectedBomb: setSelectedBomb,
    loadReplay: loadReplay,
    setLoadError: setLoadError,
    reset: reset
  };
})(window);
