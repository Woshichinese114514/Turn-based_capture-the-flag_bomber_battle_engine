/* =============================================================================
 * webui/js/core.js —— 常量、工具、配置与命名空间
 * -----------------------------------------------------------------------------
 * 职责：定义全局命名空间 window.QFR、协议常量、队伍配色、通用小工具与参数解析。
 * 依赖方向：core.js 不依赖任何其他模块（必须第一个加载）。其他模块依赖它。
 *
 * 设计意图：
 *   1. 用 IIFE + 命名空间对象而不是 ES module —— `file://` 下 `<script type="module">`
 *      会被 CORS 拦截，双击 index.html 就打不开；经典 script 标签没有这个限制。
 *   2. 协议常量（版本号、地形编码）集中在这里：将来协议升级只改一处。
 *   3. 队伍颜色用 Okabe–Ito 色盲友好调色板，且按 team_id 固定取值 ——
 *      不随回放加载顺序变化，保证同一支队在任何回放里颜色一致。
 * ========================================================================== */
(function (global) {
  'use strict';

  // 全局命名空间：所有模块挂在这里，避免污染 window 上的裸变量。
  var QFR = global.QFR || (global.QFR = {});

  /* ----------------------------- 协议常量 ---------------------------------- */

  // UI 已知的协议版本（见 docs/replay-format.md 第 7 节）。回放里的版本号大于这里就是「未来版本」，
  // 只警告不报错 —— 向前兼容是硬要求：旧 UI 必须还能渲染新回放里它认识的字段。
  QFR.KNOWN = {
    engine_version: 1,
    rules_version: 1,
    map_gen_version: 1
  };

  // 地形编码：0=空地 1=墙 2=虚空 3+team_id=阵营格（见 docs/replay-format.md §3.1）。
  QFR.TERRAIN = {
    EMPTY: 0,
    WALL: 1,
    VOID: 2,
    BASE_OFFSET: 3, // 编码 >= 3 时，team_id = code - 3
    MAX_TEAM_CODE: 6 // 2..4 队时最大编码是 3+3=6；超出按「未知地形」兜底
  };

  /* ----------------------------- 队伍配色 ---------------------------------- */

  // Okabe–Ito 8 色定量调色板的前 4 个（本项目 MAX_TEAMS = 4）。
  // 选它是因为它对红绿色盲也可区分，且都是高饱和亮色，适合暗色背景。
  QFR.TEAM_COLORS = ['#0072B2', '#D55E00', '#009E73', '#CC79A7'];

  // 稳定取色：用 team_id 取模，越界（脏数据里的 team=99）也不会取到 undefined。
  QFR.teamColor = function (team) {
    var t = typeof team === 'number' && isFinite(team) ? Math.abs(Math.floor(team)) : 0;
    return QFR.TEAM_COLORS[t % QFR.TEAM_COLORS.length];
  };

  // 队伍中文名必须与上面调色板的实际颜色一一对应：0→蓝 1→橙 2→绿 3→品红。
  // 曾经用过「红队/蓝队」，结果日志写「红队攻击蓝队」而画面上是青蓝打橙色，语义与视觉不符，
  // 会误导复盘；这里以「颜色就是身份」为准，名字只做颜色说明，不承担阵营含义。
  QFR.TEAM_LABELS = ['蓝队', '橙队', '绿队', '品红队'];
  QFR.teamLabel = function (team) {
    if (typeof team !== 'number' || !isFinite(team)) return '未知队';
    var t = Math.floor(team);
    if (t >= 0 && t < QFR.TEAM_LABELS.length) return QFR.TEAM_LABELS[t];
    return '第 ' + t + ' 队'; // 超出已知队伍数时兜底，不返回空串
  };

  /* ------------------------------- 工具 ----------------------------------- */

  // 数值兜底：JSON 里字段可能是 null / 字符串 / 缺失，直接参与运算会变成 NaN 污染整个渲染。
  QFR.num = function (v, dflt) {
    return typeof v === 'number' && isFinite(v) ? v : dflt;
  };

  // 数组兜底：协议里 units/flags/bombs/events/scores 都可能是 null 或缺失。
  QFR.arr = function (v) {
    return Array.isArray(v) ? v : [];
  };

  // 安全取字符串：AI 名字、非法动作 reason 都是「用户数据」，渲染时必须走 textContent，
  // 绝不能用 innerHTML 拼接（规范 §1 硬约束）。
  QFR.str = function (v, dflt) {
    if (typeof v === 'string') return v;
    if (v === null || v === undefined) return dflt === undefined ? '' : dflt;
    return String(v);
  };

  // 坐标显示统一成 "(x,y)"：事件日志里到处要用，集中一处避免格式不一致。
  QFR.xy = function (x, y) {
    return '(' + QFR.num(x, '?') + ',' + QFR.num(y, '?') + ')';
  };

  QFR.clamp = function (v, lo, hi) {
    return v < lo ? lo : (v > hi ? hi : v);
  };

  /* --------------------------- URL 参数解析 -------------------------------- */

  // 只做最小解析：本页面只用到 replay / tick / err 三个参数，
  // 不想引入完整 querystring 库（双击打开时 location.search 也可能为空）。
  QFR.query = function (name) {
    var search = global.location && global.location.search ? global.location.search : '';
    if (!search) return null;
    var pairs = search.replace(/^\?/, '').split('&');
    for (var i = 0; i < pairs.length; i++) {
      if (!pairs[i]) continue;
      var eq = pairs[i].indexOf('=');
      var key = eq >= 0 ? pairs[i].slice(0, eq) : pairs[i];
      if (decodeURIComponent(key) !== name) continue;
      var raw = eq >= 0 ? pairs[i].slice(eq + 1) : '';
      try {
        return decodeURIComponent(raw.replace(/\+/g, ' '));
      } catch (e) {
        return raw; // 百分号编码坏了也要能拿到原串，不让解析异常冒泡
      }
    }
    return null;
  };

  /* ------------------------- 无头验收钩子 --------------------------------- */

  // Lead 用 chromium 无头截图验收，页面必须在首帧渲染完成后同步打出就绪标志。
  // 这些函数只是「写标志位」，不做任何渲染，方便任何模块调用。
  QFR.signalReady = function (status) {
    global.__QFR_STATUS__ = status || {};
    global.__QFR_READY__ = true;
  };

  QFR.signalError = function (message) {
    global.__QFR_ERROR__ = String(message == null ? '未知错误' : message);
  };
})(window);
