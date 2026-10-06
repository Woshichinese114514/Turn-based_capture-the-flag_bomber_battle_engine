/* =============================================================================
 * webui/js/render.js —— Canvas 2D 渲染层
 * -----------------------------------------------------------------------------
 * 职责：把「当前帧 + 地图」画到 canvas 上：地形、阵营、中心区、旗、炸弹、
 *       单位、事件高亮（移动残影/攻击连线/爆炸十字/冲突闪烁）。
 * 依赖：core.js、state.js、parser 的 accessors。
 *
 * 设计意图与为什么：
 *   1. 坐标系：JSON 里是整数网格坐标，左上角 (0,0)、x 向右、y 向下。
 *      像素换算只有一处公式（见 computeLayout）：
 *          px = originX + gx * cell ;  py = originY + gy * cell
 *      所有绘制都走 gridRect()/gridCenter()，绝不各自算一遍 —— 这类换算最容易出现
 *      「半个格子偏移」的隐蔽 bug，集中一处才好排查。
 *   2. 形状必须能区分实体类型，不能只靠颜色（规范 §3）：
 *        单位 = 圆角方块 + 队伍色描边；旗 = 三角旗形；炸弹 = 圆形 + 倒计时数字。
 *   3. 跳转/步进时直接画目标帧，不做补间；事件高亮只是「叠加提示」，不阻塞任何操作。
 *   4. 所有绘制都对缺失字段兜底（坐标 null → 跳过该实体并计数），避免一个坏实体毁掉整帧。
 * ========================================================================== */
(function (global) {
  'use strict';

  var QFR = global.QFR;

  /* ------------------------------ 颜色常量 -------------------------------- */

  var COLORS = {
    bg: '#0b0f16',
    voidBg: '#05070b',
    empty: '#1b2230',
    emptyAlt: '#202939',
    grid: '#2a3446',
    wall: '#4e5a6e',
    wallEdge: '#39424f',
    center: '#c9b458',
    centerFill: 'rgba(201,180,88,0.06)',
    dead: 'rgba(160,170,185,0.30)',
    hpFull: '#7ee081',
    hpMid: '#f5c542',
    hpLow: '#e35b5b',
    label: '#e8edf5',
    labelDim: '#9aa7bb'
  };

  var state = null;   // 由 bind() 注入 QFR.state
  var canvas = null;
  var ctx = null;
  var layout = { cell: 24, originX: 0, originY: 0, mapW: 0, mapH: 0, zoom: 1, mode: 'auto' };
  var stats = { skippedEntities: 0, unknownTerrain: 0, drawnUnits: 0 };

  function bind(refs, stateModule) {
    canvas = refs.canvas;
    state = stateModule;
    ctx = canvas ? canvas.getContext('2d') : null;
  }

  /* --------------------------- 尺寸 / 坐标换算 ---------------------------- */

  /**
   * 计算画布逻辑尺寸（CSS 像素）。必须用「布局尺寸」而不是 canvas.width：
   * 后者会被 devicePixelRatio 放大，拿它算 cell 会让地图尺寸翻倍。
   */
  function cssSize() {
    var rect = canvas.getBoundingClientRect();
    var w = Math.floor(rect.width);
    var h = Math.floor(rect.height);
    // 极端窗口（例如 0 宽）时给一个最小值，避免除零/负尺寸导致绘制异常。
    return { w: Math.max(120, w || 0), h: Math.max(120, h || 0) };
  }

  /**
   * 根据窗口尺寸与地图尺寸重算格子大小。
   *   cell = floor(min(canvasW / width, canvasH / height)) * zoom
   * 保证格子是正方形；originX/originY 把地图在画布中居中。
   * 注意 canvas.width/height 要乘 devicePixelRatio（高分屏不糊），
   * 但 CSS 尺寸保持不变；绘制前用 ctx.setTransform 把逻辑坐标映射到物理像素。
   */
  function computeLayout(replay) {
    var size = cssSize();
    var mw = replay.init.map.width;
    var mh = replay.init.map.height;
    var base = Math.floor(Math.min(size.w / mw, size.h / mh));
    if (!isFinite(base) || base < 8) base = 8; // 地图很大时也不小于 8px，否则数字完全看不清

    // 自适应倍率：auto 模式在「适配尺寸」基础上再乘一个用户缩放系数。
    var cell = Math.max(8, Math.round(base * layout.zoom));
    var w = cell * mw;
    var h = cell * mh;
    layout.cell = cell;
    layout.originX = Math.round((size.w - w) / 2);
    layout.originY = Math.round((size.h - h) / 2);
    layout.mapW = w;
    layout.mapH = h;
    layout.autoBase = base;
    layout.canvasW = size.w;
    layout.canvasH = size.h;
    // 记录布局时间戳：自检脚本可据此确认「窗口缩放后确实重算过」。
    layout.lastLayoutAt = Date.now();
    return layout;
  }

  function setZoom(zoom, mode) {
    layout.zoom = QFR.clamp(QFR.num(zoom, 1), 0.4, 4);
    if (mode) layout.mode = mode;
  }

  function getLayout() { return layout; }

  // 网格坐标 → 像素矩形。这是全局唯一的坐标换算（像素 = 原点 + 格坐标 × 格子尺寸）。
  function gridRect(gx, gy, cell) {
    var c = cell || layout.cell;
    return {
      x: layout.originX + gx * c,
      y: layout.originY + gy * c,
      w: c,
      h: c
    };
  }

  // 格子中心点（画圆/三角/数字都以中心为基准，天然免去 ±0.5 格偏移）。
  function gridCenter(gx, gy, cell) {
    var c = cell || layout.cell;
    return {
      x: layout.originX + gx * c + c / 2,
      y: layout.originY + gy * c + c / 2
    };
  }

  // 像素 → 网格（画布点击选炸弹时用）。Math.floor 是必须的：不能四舍五入。
  function pointToGrid(px, py) {
    if (!layout.cell) return null;
    var gx = Math.floor((px - layout.originX) / layout.cell);
    var gy = Math.floor((py - layout.originY) / layout.cell);
    return { x: gx, y: gy };
  }

  /* ------------------------------- 绘制文本 ------------------------------- */

  // 简洁的居中文本工具：字体大小随格子缩放，保证小图也能看清。
  function centerText(text, x, y, color, fontPx, weight) {
    ctx.fillStyle = color;
    ctx.font = (weight || '600') + ' ' + fontPx + 'px system-ui, "Noto Sans CJK SC", sans-serif';
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(text, x, y);
  }

  /* ------------------------------ 各图层绘制 ------------------------------ */

  function drawTerrain(replay) {
    var map = replay.init.map;
    var accessors = state.data.accessors;
    var unknown = 0;

    // 先铺一层「虚空底色」：地图外的画布区域也要有个深色背景，看起来才像地图边界。
    ctx.fillStyle = COLORS.voidBg;
    ctx.fillRect(0, 0, layout.canvasW, layout.canvasH);

    for (var y = 0; y < map.height; y++) {
      for (var x = 0; x < map.width; x++) {
        var code = map.terrain[y * map.width + x];
        var kind = QFR.parser.terrainKind(code, accessors.teamCount());
        if (kind === 'unknown') { unknown++; kind = 'empty'; } // 未知地形按空地渲染（规范 §3.1）
        var r = gridRect(x, y);

        if (kind === 'void') {
          // 虚空：最深的底色（不画网格线），表示完全不可进入。
          ctx.fillStyle = COLORS.voidBg;
          ctx.fillRect(r.x, r.y, r.w, r.h);
          ctx.strokeStyle = '#0d1219';
          ctx.lineWidth = 1;
          ctx.strokeRect(r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1);
        } else if (kind === 'wall') {
          // 墙：深色实心块 + 内描边，比空地明显更「重」，一眼能看出通路。
          ctx.fillStyle = COLORS.wall;
          ctx.fillRect(r.x, r.y, r.w, r.h);
          ctx.strokeStyle = COLORS.wallEdge;
          ctx.lineWidth = 1;
          ctx.strokeRect(r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1);
        } else if (kind === 'base') {
          // 阵营格：队伍色半透明填充 + 加粗边界（归属由编码 3+team 直接反推）。
          var team = QFR.parser.baseTeamOf(code);
          var color = QFR.teamColor(team);
          ctx.fillStyle = hexToRgba(color, 0.26);
          ctx.fillRect(r.x, r.y, r.w, r.h);
          ctx.strokeStyle = color;
          ctx.lineWidth = Math.max(1.5, layout.cell * 0.07);
          ctx.strokeRect(r.x + ctx.lineWidth / 2, r.y + ctx.lineWidth / 2,
            r.w - ctx.lineWidth, r.h - ctx.lineWidth);
        } else {
          // 空地：棋盘格深浅交替 + 细网格线，避免大片同色导致「看不出格子」。
          ctx.fillStyle = ((x + y) % 2 === 0) ? COLORS.empty : COLORS.emptyAlt;
          ctx.fillRect(r.x, r.y, r.w, r.h);
          if (layout.cell >= 14) {
            ctx.strokeStyle = COLORS.grid;
            ctx.lineWidth = 1;
            ctx.strokeRect(r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1);
          }
        }
      }
    }

    // 地图外框：明确地图边界（虚空格与画布背景同色，没有边框会分不清）。
    ctx.strokeStyle = '#39465c';
    ctx.lineWidth = 2;
    ctx.strokeRect(layout.originX - 1, layout.originY - 1, layout.mapW + 2, layout.mapH + 2);

    // 未知地形只在控制台警告一次（不刷屏），符合规范「兜底 + 警告」的要求。
    if (unknown && stats.unknownTerrain !== unknown) {
      stats.unknownTerrain = unknown;
      if (global.console && console.warn) console.warn('[QFR.render] 未知地形编码数量：' + unknown + '，已按空地渲染');
    }
  }

  function drawCenterArea(replay) {
    var cr = replay.init.center_radius;
    if (typeof cr !== 'number' || cr < 0) return; // init 未提供 center_radius 时不画
    var map = replay.init.map;
    var cx = (map.width - 1) / 2;
    var cy = (map.height - 1) / 2;

    // 中心区是「旗可能刷新的范围」（曼哈顿距离 ≤ R），用半透明菱形/外接圆示意。
    ctx.save();
    var c = gridCenter(cx, cy);
    ctx.beginPath();
    ctx.arc(c.x, c.y, (cr + 0.5) * layout.cell, 0, Math.PI * 2);
    ctx.fillStyle = COLORS.centerFill;
    ctx.fill();
    ctx.setLineDash([6, 5]);
    ctx.strokeStyle = COLORS.center;
    ctx.lineWidth = 1.5;
    ctx.stroke();
    ctx.restore();

    // 中心点标记
    ctx.beginPath();
    ctx.arc(c.x, c.y, Math.max(2, layout.cell * 0.08), 0, Math.PI * 2);
    ctx.fillStyle = COLORS.center;
    ctx.fill();
  }

  // 旗：三角旗形（与圆形的炸弹、方块的单位在形状上明确区分）。
  function drawFlag(f, cell) {
    var c = gridCenter(f.x, f.y, cell);
    var s = cell;
    ctx.save();
    // 旗杆
    ctx.strokeStyle = '#d8dee9';
    ctx.lineWidth = Math.max(1.5, s * 0.06);
    ctx.beginPath();
    ctx.moveTo(c.x - s * 0.16, c.y + s * 0.34);
    ctx.lineTo(c.x - s * 0.16, c.y - s * 0.34);
    ctx.stroke();
    // 旗面（三角形）：被携带时用更亮的填充，便于和「落地的旗」区分
    ctx.beginPath();
    ctx.moveTo(c.x - s * 0.16, c.y - s * 0.34);
    ctx.lineTo(c.x + s * 0.32, c.y - s * 0.16);
    ctx.lineTo(c.x - s * 0.16, c.y + s * 0.02);
    ctx.closePath();
    ctx.fillStyle = (f.carrier !== null && f.carrier !== undefined) ? '#ffe066' : '#f2f4f8';
    ctx.fill();
    ctx.strokeStyle = '#6c7686';
    ctx.lineWidth = 1;
    ctx.stroke();

    // 旗编号：小字标在格子左下角，多处有旗时便于对照事件日志。
    if (cell >= 22) {
      centerText('#' + f.id, c.x - s * 0.18, c.y + s * 0.42, COLORS.labelDim, Math.max(8, s * 0.26), '500');
    }
    ctx.restore();
  }

  // 炸弹：圆形 + 倒计时数字；颜色由 timer 从黄渐到红（规范 §4.3）。
  function drawBomb(b, cell) {
    var c = gridCenter(b.x, b.y, cell);
    var t = QFR.clamp(QFR.num(b.timer, 0), 0, 2);
    // timer: 2 → 黄, 1 → 橙红, 0 → 红。用比例算颜色，将来 timer 变长也不用改代码。
    var ratio = 1 - (t / 2);
    var fill = mixColor('#f2d024', '#e0322a', QFR.clamp(ratio, 0, 1));

    ctx.beginPath();
    ctx.arc(c.x, c.y, cell * 0.36, 0, Math.PI * 2);
    ctx.fillStyle = fill;
    ctx.fill();
    ctx.strokeStyle = '#2b1d05';
    ctx.lineWidth = Math.max(1, cell * 0.06);
    ctx.stroke();

    // 引信火花：timer 越小火花越大，强化「快炸了」的视觉提示。
    ctx.beginPath();
    ctx.arc(c.x + cell * 0.3, c.y - cell * 0.3, Math.max(1.5, cell * (0.06 + 0.05 * ratio)), 0, Math.PI * 2);
    ctx.fillStyle = '#fff3b0';
    ctx.fill();

    if (cell >= 18) {
      centerText(String(t), c.x, c.y + 0.5, '#2b1d05', Math.max(9, cell * 0.38), '700');
    }
  }

  // 炸弹爆炸范围预览：十字（曼哈顿距离 ≤ radius），墙会挡住传播。
  function drawBombRadius(b, cell, accessors) {
    var radius = QFR.num(b.radius, 2);
    var map = state.data.replay.init.map;
    ctx.save();
    ctx.lineWidth = Math.max(1, cell * 0.06);
    ctx.strokeStyle = 'rgba(255,120,60,0.55)';
    var dirs = [[1, 0], [-1, 0], [0, 1], [0, -1]];
    for (var d = 0; d < dirs.length; d++) {
      for (var step = 1; step <= radius; step++) {
        var x = b.x + dirs[d][0] * step;
        var y = b.y + dirs[d][1] * step;
        if (x < 0 || y < 0 || x >= map.width || y >= map.height) break;
        // 墙阻挡爆炸传播（协议 §3.1 明确定义）；只阻止继续向外，不改变本格。
        if (QFR.parser.terrainKind(map.terrain[y * map.width + x], accessors.teamCount()) === 'wall') break;
        var r = gridRect(x, y, cell);
        ctx.fillStyle = 'rgba(255,110,50,0.16)';
        ctx.fillRect(r.x, r.y, r.w, r.h);
        ctx.strokeRect(r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1);
      }
    }
    ctx.restore();
  }

  // 单位：队伍色圆角方块；带 HP 点阵；携旗叠加小三角；已攻击画小叉。
  function drawUnit(u, cell, accessors) {
    if (!isFinite(u.x) || !isFinite(u.y)) { stats.skippedEntities++; return; }
    var color = QFR.teamColor(u.team);
    var r = gridRect(u.x, u.y, cell);
    var pad = Math.max(2, cell * 0.14);
    var x = r.x + pad, y = r.y + pad, w = r.w - pad * 2, h = r.h - pad * 2;
    var rad = Math.max(2, cell * 0.16);

    if (!u.alive) {
      // 死亡单位：半透明灰影 + 复活倒计时。仍然画出来，
      // 因为「尸体位置」和「还有几回合复活」对理解战局很重要。
      roundRect(x, y, w, h, rad);
      ctx.fillStyle = COLORS.dead;
      ctx.fill();
      ctx.setLineDash([3, 3]);
      ctx.strokeStyle = 'rgba(200,210,225,0.45)';
      ctx.lineWidth = 1;
      ctx.stroke();
      ctx.setLineDash([]);
      if (cell >= 22 && u.respawn_timer > 0) {
        centerText('↻' + u.respawn_timer, x + w / 2, y + h / 2, '#c8d2e1', Math.max(9, cell * 0.32), '600');
      }
      return;
    }

    // 阴影：让单位从地形的棋盘格上「浮」起来
    ctx.save();
    ctx.globalAlpha = 0.35;
    roundRect(x + 1, y + 2, w, h, rad);
    ctx.fillStyle = '#000';
    ctx.fill();
    ctx.restore();

    roundRect(x, y, w, h, rad);
    ctx.fillStyle = mixColor(color, '#ffffff', 0.18); // 提亮一点，暗色背景上更醒目
    ctx.fill();
    ctx.strokeStyle = color;
    ctx.lineWidth = Math.max(1.5, cell * 0.07);
    ctx.stroke();

    // HP 点阵：满血 3 点 → 直接可数，比拼数字更快读；<=1 时变红提示濒死。
    // 放在 ID 文字与格子底边之间的空白带，避免和 ID 文字重叠。
    var hp = QFR.clamp(QFR.num(u.hp, 0), 0, 3);
    var dotR = Math.max(1.4, cell * 0.055);
    var gap = dotR * 2.6;
    var startX = x + w / 2 - gap * (hp - 1) / 2;
    var dotY = y + h - Math.max(2.5, cell * 0.11);
    var dotColor = hp >= 3 ? COLORS.hpFull : (hp === 2 ? COLORS.hpMid : COLORS.hpLow);
    for (var i = 0; i < hp; i++) {
      ctx.beginPath();
      ctx.arc(startX + i * gap, dotY, dotR, 0, Math.PI * 2);
      ctx.fillStyle = dotColor;
      ctx.fill();
    }

    // 单位 ID：格子够大才画，避免小图糊成一团；上移一点给下方 HP 点阵留位。
    if (cell >= 20) {
      centerText('#' + u.id, x + w / 2, y + h / 2 - cell * 0.12, '#0c1016', Math.max(8, cell * 0.30), '700');
    }

    // 携带旗标记：单位右上角叠加一个金色小三角（形状+颜色双重提示）
    if (u.carrying_flag !== null && u.carrying_flag !== undefined) {
      ctx.beginPath();
      ctx.moveTo(x + w * 0.72, y - cell * 0.12);
      ctx.lineTo(x + w * 1.02, y + cell * 0.06);
      ctx.lineTo(x + w * 0.72, y + cell * 0.24);
      ctx.closePath();
      ctx.fillStyle = '#ffe066';
      ctx.fill();
      ctx.strokeStyle = '#6b5a12';
      ctx.lineWidth = 1;
      ctx.stroke();
    }

    // 本回合已攻击：右上角小叉（灰色，不抢主视觉）
    if (u.attacked_this_turn) {
      var ax = x + w - cell * 0.13, ay = y + cell * 0.13, s2 = Math.max(2, cell * 0.09);
      ctx.strokeStyle = '#ff9f43';
      ctx.lineWidth = Math.max(1.2, cell * 0.05);
      ctx.beginPath();
      ctx.moveTo(ax - s2, ay - s2); ctx.lineTo(ax + s2, ay + s2);
      ctx.moveTo(ax + s2, ay - s2); ctx.lineTo(ax - s2, ay + s2);
      ctx.stroke();
    }
  }

  // 选中炸弹的红色脉冲圈（画布点击 / 事件日志联动）
  function drawBombSelection(b, cell) {
    var c = gridCenter(b.x, b.y, cell);
    ctx.beginPath();
    ctx.arc(c.x, c.y, cell * 0.48, 0, Math.PI * 2);
    ctx.strokeStyle = '#ff7043';
    ctx.lineWidth = 2;
    ctx.stroke();
  }

  // 事件高亮：移动残影 / 攻击连线 / 爆炸十字 / 冲突闪烁。
  // 这些只是本帧的「动画素材」，忽略它们也能正确渲染（协议 §5 明文）。
  function drawEventHighlights(frame, cell, accessors) {
    var events = frame.events || [];
    for (var i = 0; i < events.length; i++) {
      var e = events[i];
      var type = QFR.str(e.type, '');
      if (type === 'unit_moved') {
        // 移动残影：起点画一个空心方框，能看到「从哪来」
        var r = gridRect(e.from_x, e.from_y, cell);
        ctx.save();
        ctx.globalAlpha = 0.45;
        ctx.strokeStyle = '#8fb8ff';
        ctx.lineWidth = 1.5;
        ctx.strokeRect(r.x + 2, r.y + 2, r.w - 4, r.h - 4);
        ctx.restore();
      } else if (type === 'unit_attacked') {
        // 攻击连线：攻击者 → 目标（虚线 + 半透明，避免遮住实体）
        var a = unitPos(frame, e.attacker);
        var t = unitPos(frame, e.target);
        if (a && t) {
          var from = gridCenter(a.x, a.y, cell);
          var to = gridCenter(t.x, t.y, cell);
          ctx.save();
          ctx.setLineDash([5, 4]);
          ctx.strokeStyle = 'rgba(255,120,90,0.85)';
          ctx.lineWidth = 2;
          ctx.beginPath();
          ctx.moveTo(from.x, from.y);
          ctx.lineTo(to.x, to.y);
          ctx.stroke();
          ctx.restore();
        }
      } else if (type === 'bomb_exploded') {
        // 爆炸十字：用事件自带的 x/y/radius 重画一次扩散范围（即使是越界/被墙挡的格子也可以
        // 直接按曼哈顿距离画，因为这是「已经发生过」的视觉效果）。
        var bx = QFR.num(e.x, null), by = QFR.num(e.y, null), rad = QFR.num(e.radius, 0);
        if (bx === null || by === null) continue;
        ctx.save();
        ctx.globalAlpha = 0.5;
        var cells = [[bx, by]];
        var dirs = [[1, 0], [-1, 0], [0, 1], [0, -1]];
        for (var d = 0; d < dirs.length; d++) {
          for (var s = 1; s <= rad; s++) cells.push([bx + dirs[d][0] * s, by + dirs[d][1] * s]);
        }
        for (var ci = 0; ci < cells.length; ci++) {
          var rr = gridRect(cells[ci][0], cells[ci][1], cell);
          ctx.fillStyle = 'rgba(255,170,60,0.30)';
          ctx.fillRect(rr.x, rr.y, rr.w, rr.h);
          ctx.strokeStyle = 'rgba(255,220,120,0.75)';
          ctx.lineWidth = 1.5;
          ctx.strokeRect(rr.x + 1, rr.y + 1, rr.w - 2, rr.h - 2);
        }
        ctx.restore();
      } else if (type === 'move_conflict') {
        // 冲突格：红色闪烁方块（本帧静态，跳转时不会残留）
        var cx = QFR.num(e.x, null), cy = QFR.num(e.y, null);
        if (cx === null || cy === null) continue;
        var cr = gridRect(cx, cy, cell);
        ctx.save();
        ctx.strokeStyle = '#ff4d4f';
        ctx.lineWidth = 3;
        ctx.strokeRect(cr.x + 2, cr.y + 2, cr.w - 4, cr.h - 4);
        ctx.restore();
      }
    }
  }

  function unitPos(frame, id) {
    for (var i = 0; i < frame.units.length; i++) {
      if (frame.units[i].id === id) return frame.units[i];
    }
    return null;
  }

  /* ------------------------------ 小工具 ---------------------------------- */

  function roundRect(x, y, w, h, r) {
    var rr = Math.min(r, w / 2, h / 2);
    ctx.beginPath();
    ctx.moveTo(x + rr, y);
    ctx.lineTo(x + w - rr, y);
    ctx.quadraticCurveTo(x + w, y, x + w, y + rr);
    ctx.lineTo(x + w, y + h - rr);
    ctx.quadraticCurveTo(x + w, y + h, x + w - rr, y + h);
    ctx.lineTo(x + rr, y + h);
    ctx.quadraticCurveTo(x, y + h, x, y + h - rr);
    ctx.lineTo(x, y + rr);
    ctx.quadraticCurveTo(x, y, x + rr, y);
    ctx.closePath();
  }

  function hexToRgba(hex, alpha) {
    var h = String(hex).replace('#', '');
    if (h.length === 3) h = h[0] + h[0] + h[1] + h[1] + h[2] + h[2];
    var n = parseInt(h, 16);
    if (!isFinite(n)) return 'rgba(128,128,128,' + alpha + ')';
    return 'rgba(' + ((n >> 16) & 255) + ',' + ((n >> 8) & 255) + ',' + (n & 255) + ',' + alpha + ')';
  }

  // 颜色插值：炸弹倒计时黄→红、单位提亮都用它，避免手写一串魔数色值。
  function mixColor(from, to, ratio) {
    var t = QFR.clamp(ratio, 0, 1);
    var a = hexToRgb(from), b = hexToRgb(to);
    var r = Math.round(a[0] + (b[0] - a[0]) * t);
    var g = Math.round(a[1] + (b[1] - a[1]) * t);
    var bl = Math.round(a[2] + (b[2] - a[2]) * t);
    return 'rgb(' + r + ',' + g + ',' + bl + ')';
  }

  function hexToRgb(hex) {
    var h = String(hex).replace('#', '');
    if (h.length === 3) h = h[0] + h[0] + h[1] + h[1] + h[2] + h[2];
    var n = parseInt(h, 16);
    if (!isFinite(n)) return [128, 128, 128];
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
  }

  /* ------------------------------- 主绘制 --------------------------------- */

  /**
   * 渲染当前帧。整个页面绘制入口只有这一个函数。
   * @param {object} frame 当前帧（可能为 null，例如空回放）
   */
  function render(frame) {
    if (!canvas || !ctx || !state) return;
    var replay = state.data.replay;
    if (!replay) {
      // 没有回放时画一块「等待加载」的提示，避免用户看到一片空白以为坏了。
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.fillStyle = COLORS.bg;
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      return;
    }

    stats.skippedEntities = 0;
    stats.drawnUnits = 0;

    var dpr = global.devicePixelRatio || 1;
    computeLayout(replay);

    // 物理像素 = CSS 像素 × dpr；所有绘制都用 CSS 像素坐标，靠 setTransform 换算。
    var pw = Math.floor(layout.canvasW * dpr);
    var ph = Math.floor(layout.canvasH * dpr);
    if (canvas.width !== pw) canvas.width = pw;
    if (canvas.height !== ph) canvas.height = ph;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    var cell = layout.cell;
    var accessors = state.data.accessors;

    // 图层顺序（从下到上）：地形 → 中心区 → 旗 → 炸弹 → 事件高亮 → 单位。
    // 单位画在最上面，因为它是最重要的信息，不能被阴影/高亮盖住。
    drawTerrain(replay);
    drawCenterArea(replay);

    if (frame) {
      var i;
      // 被携带的旗跟着携带者；协议保证 flag.x/y 已等于携带者位置，直接用即可。
      for (i = 0; i < frame.flags.length; i++) {
        if (!isFinite(frame.flags[i].x) || !isFinite(frame.flags[i].y)) { stats.skippedEntities++; continue; }
        drawFlag(frame.flags[i], cell);
      }
      for (i = 0; i < frame.bombs.length; i++) {
        var b = frame.bombs[i];
        if (!isFinite(b.x) || !isFinite(b.y)) { stats.skippedEntities++; continue; }
        // 选中或只有 1 个炸弹时显示爆炸范围预览（帮助理解炸弹威胁）
        if (state.data.selectedBombId === b.id || frame.bombs.length === 1) {
          drawBombRadius(b, cell, accessors);
        }
        drawBomb(b, cell);
        if (state.data.selectedBombId === b.id) drawBombSelection(b, cell);
      }

      drawEventHighlights(frame, cell, accessors);

      for (i = 0; i < frame.units.length; i++) {
        drawUnit(frame.units[i], cell, accessors);
        stats.drawnUnits++;
      }
    }

    // 左上角 HUD：即使右侧面板被窗口挤掉，画面里也能看到 tick 与分数。
    drawHud(replay, frame);
  }

  function drawHud(replay, frame) {
    var total = replay.frames.length;
    var tick = frame ? frame.tick : 0;
    var text = 'tick ' + tick + ' / ' + total;
    ctx.save();
    ctx.font = '600 13px system-ui, sans-serif';
    var w = ctx.measureText(text).width + 16;
    ctx.fillStyle = 'rgba(8,12,18,0.72)';
    ctx.fillRect(layout.originX, layout.originY - 26, w, 20);
    ctx.fillStyle = COLORS.label;
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    ctx.fillText(text, layout.originX + 8, layout.originY - 16);

    // 分数条（贴在 HUD 右侧，队伍色圆点 + 分数，领先队伍加粗）
    if (frame) {
      var x = layout.originX + w + 10;
      var best = -Infinity;
      for (var t = 0; t < frame.scores.length; t++) best = Math.max(best, QFR.num(frame.scores[t], 0));
      for (var i = 0; i < frame.scores.length; i++) {
        var color = QFR.teamColor(i);
        ctx.beginPath();
        ctx.arc(x + 6, layout.originY - 16, 6, 0, Math.PI * 2);
        ctx.fillStyle = color;
        ctx.fill();
        var s = String(QFR.num(frame.scores[i], 0));
        ctx.font = (QFR.num(frame.scores[i], 0) === best && best > 0 ? '800' : '600') + ' 14px system-ui, sans-serif';
        ctx.fillStyle = COLORS.label;
        ctx.fillText(s, x + 16, layout.originY - 16);
        x += 20 + ctx.measureText(s).width;
      }
    }
    ctx.restore();
  }

  QFR.render = {
    bind: bind,
    render: render,
    computeLayout: computeLayout,
    getLayout: getLayout,
    setZoom: setZoom,
    cssSize: cssSize,
    pointToGrid: pointToGrid,
    stats: stats
  };
})(window);
