/* =============================================================================
 * webui/js/parser.js —— 回放 JSONL 解析、校验与容错
 * -----------------------------------------------------------------------------
 * 职责：把一段 JSONL 文本解析成 { init, frames[], end, warnings[], errors[], stats }。
 * 依赖：core.js（QFR.KNOWN / QFR.num / QFR.arr / QFR.str）。
 * 依赖方向：parser 不碰 DOM、不碰 canvas —— 纯数据层，便于单独测试与将来协议升级。
 *
 * 设计意图与为什么：
 *   1. 全部加载进内存后按 tick 建索引：进度条拖动/输入 tick 必须 O(1) 命中，
 *      差分格式或边读边放都会让跳转退化。
 *   2. 容错优先于严格：畸形行跳过并计数、字段缺失兜底、版本不符只警告。
 *      回放文件是「人类手写样例 + 引擎导出」混合来源，UI 崩一次就没有第二次机会了。
 *   3. 协议字段访问集中在下面的 normalize* 函数里：将来协议加字段只改这里。
 * ========================================================================== */
(function (global) {
  'use strict';

  var QFR = global.QFR;

  /* ---------------------- 图层归一化（字段缺失兜底） ---------------------- */

  // 注意：这里用 `||` 而不是 `??`，因为项目要求能在较老的 Chromium 上跑；
  // 缺失字段（undefined）与显式 null 都走默认值。

  function normalizeUnit(raw, index) {
    var u = raw && typeof raw === 'object' ? raw : {};
    return {
      // id 缺失时用数组下标兜底：宁可显示成「单位 0」也不要整帧丢掉。
      id: QFR.num(u.id, index),
      team: QFR.num(u.team, 0),
      x: QFR.num(u.x, 0),
      y: QFR.num(u.y, 0),
      hp: QFR.num(u.hp, 0),
      // alive 缺失时按 hp>0 推断：比直接当 false 更符合直觉
      alive: typeof u.alive === 'boolean' ? u.alive : QFR.num(u.hp, 0) > 0,
      respawn_timer: QFR.num(u.respawn_timer, 0),
      carrying_flag: (u.carrying_flag === null || u.carrying_flag === undefined) ? null : QFR.num(u.carrying_flag, null),
      attacked_this_turn: u.attacked_this_turn === true
    };
  }

  function normalizeFlag(raw, index) {
    var f = raw && typeof raw === 'object' ? raw : {};
    return {
      id: QFR.num(f.id, index),
      x: QFR.num(f.x, 0),
      y: QFR.num(f.y, 0),
      carrier: (f.carrier === null || f.carrier === undefined) ? null : QFR.num(f.carrier, null)
    };
  }

  function normalizeBomb(raw, index) {
    var b = raw && typeof raw === 'object' ? raw : {};
    return {
      id: QFR.num(b.id, index),
      x: QFR.num(b.x, 0),
      y: QFR.num(b.y, 0),
      team: QFR.num(b.team, 0),
      timer: QFR.num(b.timer, 0),
      radius: QFR.num(b.radius, 2)
    };
  }

  // 事件保留原始类型名：未知事件也必须能被日志显示（规范 §2「向前兼容」）。
  function normalizeEvent(raw) {
    return (raw && typeof raw === 'object') ? raw : { type: 'unknown' };
  }

  // 一帧的归一化：units/flags/bombs/events 缺失当空数组，scores 缺失当全 0。
  function normalizeFrame(raw) {
    var f = (raw && typeof raw === 'object') ? raw : {};
    var scores = QFR.arr(f.scores);
    return {
      tick: QFR.num(f.tick, null), // null 表示需要 parser 后补 tick（按上一帧 +1）
      scores: scores,
      units: QFR.arr(f.units).map(normalizeUnit),
      flags: QFR.arr(f.flags).map(normalizeFlag),
      bombs: QFR.arr(f.bombs).map(normalizeBomb),
      events: QFR.arr(f.events).map(normalizeEvent)
    };
  }

  /* ------------------------------ 版本校验 -------------------------------- */

  // 返回 [{ level: 'warn'|'error', text }]。只做「提示」，绝不因为版本不符中断解析。
  function checkVersions(init) {
    var out = [];
    var known = QFR.KNOWN;

    if (QFR.num(init.engine_version, null) === null) {
      out.push({ level: 'warn', text: 'init 缺少 engine_version 字段（回放可能来自旧版本引擎）。' });
    } else if (init.engine_version > known.engine_version) {
      out.push({
        level: 'warn',
        text: '引擎版本 ' + init.engine_version + ' 高于本查看器已知版本 ' + known.engine_version +
          '，回放可能包含本 UI 不认识的规则/事件；不认识的事件会原样显示，不影响渲染。'
      });
    } else if (init.engine_version < known.engine_version) {
      out.push({
        level: 'warn',
        text: '引擎版本 ' + init.engine_version + ' 低于本查看器已知版本 ' + known.engine_version +
          '，部分字段可能缺失，已按默认值兜底。'
      });
    }

    if (QFR.num(init.rules_version, null) === null) {
      out.push({ level: 'warn', text: 'init 缺少 rules_version 字段。' });
    } else if (init.rules_version !== known.rules_version) {
      out.push({
        level: 'warn',
        text: '规则版本 ' + init.rules_version + ' 与本 UI 已知版本 ' + known.rules_version +
          ' 不一致，伤害/得分等判定口径可能不同。'
      });
    }

    // map_gen_version 决定地形编码含义，不认识时必须黄色警告（规范 §7 明确要求）。
    var mgv = QFR.num(init.map_gen_version, null);
    if (mgv === null) {
      out.push({ level: 'warn', text: 'init 缺少 map_gen_version 字段，地形编码可能无法正确解释。' });
    } else if (mgv !== known.map_gen_version) {
      out.push({
        level: 'warn',
        text: '地图生成版本 ' + mgv + ' 与本 UI 已知版本 ' + known.map_gen_version +
          ' 不一致，地形编码可能不同，已按当前版本尽量渲染。'
      });
    }

    var map = init.map || {};
    var mapMgv = QFR.num(map.map_gen_version, null);
    if (mgv !== null && mapMgv !== null && mapMgv !== mgv) {
      out.push({
        level: 'warn',
        text: '顶层 map_gen_version=' + mgv + ' 与 map.map_gen_version=' + mapMgv + ' 不一致。'
      });
    }
    return out;
  }

  /* ------------------------------- 主解析 --------------------------------- */

  /**
   * 解析回放文本。
   * @param {string} text JSONL 全文
   * @param {{expectedTicks?:number}} [opts]
   * @returns {object} { init, frames, end, warnings, errors, stats, fatal }
   *   fatal=true 表示「一条可用数据都没有」，调用方应显示错误并保留控件可用。
   */
  function parseText(text, opts) {
    opts = opts || {};
    var warnings = [];
    var errors = [];

    // 去掉 UTF-8 BOM：某些编辑器保存的 JSONL 会带 \uFEFF，不去掉第一行必解析失败。
    var src = typeof text === 'string' ? text : '';
    if (src.charCodeAt(0) === 0xFEFF) src = src.slice(1);

    var lines = src.split(/\r?\n/); // 兼容 CRLF（Windows 导出的回放也一样能读）

    var initRaw = null;
    var framesRaw = [];
    var endRaw = null;
    var jsonErrorLines = []; // 记录行号，用于「已跳过 N 行」提示
    var unknownLineTypes = 0;

    for (var i = 0; i < lines.length; i++) {
      var line = lines[i];
      if (!line || !line.trim()) continue; // 空行（样例文件里就有）直接忽略，不算错误

      var obj;
      try {
        obj = JSON.parse(line);
      } catch (e) {
        // 关键容错：单行坏 JSON 只跳过并计数，不抛异常、不中断整份回放。
        jsonErrorLines.push(i + 1);
        continue;
      }
      if (!obj || typeof obj !== 'object') {
        jsonErrorLines.push(i + 1);
        continue;
      }

      switch (obj.type) {
        case 'init':
          // 规范说 init 恰好一行；若出现多行（文件被拼接），取第一条并提示。
          if (initRaw === null) initRaw = obj;
          else warnings.push({ level: 'warn', text: '第 ' + (i + 1) + ' 行出现重复 init 行，已忽略。' });
          break;
        case 'frame':
          framesRaw.push(normalizeFrame(obj));
          break;
        case 'end':
          endRaw = obj; // 出现多行 end 时以最后一行为准
          break;
        default:
          unknownLineTypes++;
          break;
      }
    }

    if (jsonErrorLines.length) {
      errors.push({
        level: 'error',
        text: '已跳过 ' + jsonErrorLines.length + ' 行无法解析的数据（文件可能有损坏）' +
          (jsonErrorLines.length <= 8 ? '，行号：' + jsonErrorLines.join('、') : '，首行行号：' + jsonErrorLines[0])
      });
    }
    if (unknownLineTypes) {
      warnings.push({ level: 'warn', text: '已忽略 ' + unknownLineTypes + ' 行未知类型的行（可能是新协议字段）。' });
    }

    // ---- init 归一化：整行缺失/坏掉时必须能「降级渲染」而不是白屏 ----
    var initMissing = (initRaw === null);
    var init = normalizeInit(initRaw);
    if (initMissing) {
      warnings.push({
        level: 'warn',
        text: '未找到 init 行（文件开头可能已损坏）：地图尺寸与队伍信息缺失，已按 15×15 空地图兜底渲染。'
      });
    }
    // 地形长度不符：补齐/截断都要警告（规范 §7 明确要求「用 Empty 补齐缺失部分」）。
    if (init.map.terrainLengthMismatch) {
      warnings.push({
        level: 'warn',
        text: '地图地形数组长度 ' + init.map.terrainRawLength + ' 与 width*height=' +
          (init.map.width * init.map.height) + ' 不符，已' +
          (init.map.terrainRawLength < init.map.width * init.map.height ? '用空地补齐缺失部分' : '截断多余部分') + '。'
      });
    }
    // 未知地形编码统计：>3+队伍数 的编码按空地渲染，但必须让用户知道。
    var unknownTerrain = 0;
    for (var ti = 0; ti < init.map.terrain.length; ti++) {
      if (terrainKind(init.map.terrain[ti], init.teams.length) === 'unknown') unknownTerrain++;
    }
    if (unknownTerrain) {
      warnings.push({
        level: 'warn',
        text: '地图里有 ' + unknownTerrain + ' 个未知地形编码（超出本 UI 已知的 0..' +
          (QFR.TERRAIN.BASE_OFFSET + Math.max(0, init.teams.length - 1)) + '），已按空地渲染。'
      });
    }

    // 顶层版本号校验（缺失 init 时 normalizeInit 会填 null，checkVersions 会给「缺少字段」提示）。
    var versionWarnings = checkVersions(init);
    for (var w = 0; w < versionWarnings.length; w++) warnings.push(versionWarnings[w]);

    // ---- 帧 tick 补全与排序 ----
    // tick 缺失时按「上一帧 +1」推断；这样即使整份文件都没写 tick 也还能按顺序播放。
    var lastTick = 0;
    for (var k = 0; k < framesRaw.length; k++) {
      var fr = framesRaw[k];
      if (fr.tick === null) fr.tick = lastTick + 1;
      fr.tick = QFR.num(fr.tick, lastTick + 1);
      lastTick = fr.tick;
    }
    framesRaw.sort(function (a, b) { return a.tick - b.tick; });

    // tick 重复：保留第一条（协议要求逐 1 递增，重复说明文件有问题，但不必崩）。
    var frames = [];
    var dupTicks = 0;
    for (var m = 0; m < framesRaw.length; m++) {
      if (frames.length && frames[frames.length - 1].tick === framesRaw[m].tick) { dupTicks++; continue; }
      frames.push(framesRaw[m]);
    }
    if (dupTicks) {
      warnings.push({ level: 'warn', text: '发现 ' + dupTicks + ' 个重复 tick 的帧，已保留每个 tick 的第一帧。' });
    }
    var realFrameCount = frames.length; // 占位帧是否被合成，用来判定 fatal（下面才会 push 占位帧）
    if (!frames.length) {
      warnings.push({ level: 'warn', text: '回放里没有任何 frame 行，无法播放逐帧内容。' });
      // 造一个空的「第 1 tick」占位帧：让渲染器/信息面板有东西可画（地图仍来自 init）。
      frames.push({ tick: 1, scores: [], units: [], flags: [], bombs: [], events: [] });
    }

    // ---- end 归一化 ----
    var end = normalizeEnd(endRaw, init, frames);
    if (endRaw === null) {
      warnings.push({ level: 'warn', text: '回放未提供 end 行，最终结果未知（仍可播放）。' });
    } else if (QFR.num(endRaw.ticks, null) !== null && frames.length &&
      endRaw.ticks !== frames[frames.length - 1].tick) {
      warnings.push({
        level: 'warn',
        text: 'end.ticks=' + endRaw.ticks + ' 与实际帧数/末帧 tick=' + frames[frames.length - 1].tick +
          ' 不一致（文件可能被截断）。'
      });
    }

    var stats = {
      totalLines: lines.length,
      badLines: jsonErrorLines.length,
      badLineNumbers: jsonErrorLines,
      frameCount: frames.length,
      declaredTicks: QFR.num(end.ticks, null),
      ticks: frames.length,
      teams: init.teams.length
    };

    // fatal = 「文件里一条可用数据都没有」：没有 init、没有真正的 frame（frames 里的那一条
    // 是上面为兜底合成的占位帧）、也没有 end。空文件、全是坏行、只有一行垃圾都属于这种情况。
    // 注意不能要求 jsonErrorLines > 0：空文件一行 JSON 错误都没有，但同样没有可用数据。
    // 而 malformed.jsonl 那种「init 坏掉但有 2 个可用 frame」不算 fatal —— 仍然可以渲染。
    var fatal = initMissing && !endRaw && realFrameCount === 0;

    return {
      init: init,
      frames: frames,
      end: end,
      warnings: warnings,
      errors: errors,
      stats: stats,
      fatal: fatal
    };
  }

  /* --------------------------- init / end 归一化 --------------------------- */

  function normalizeInit(raw) {
    var o = (raw && typeof raw === 'object') ? raw : null;
    var mapRaw = o && o.map && typeof o.map === 'object' ? o.map : {};

    var width = QFR.num(mapRaw.width, 15);
    var height = QFR.num(mapRaw.height, 15);
    if (width <= 0) width = 15;
    if (height <= 0) height = 15;

    // 地形：长度不符时补齐/截断 + 记录异常编码（都只警告，不抛异常）。
    var terrainIn = QFR.arr(mapRaw.terrain);
    var terrain = [];
    var i;
    for (i = 0; i < width * height; i++) {
      var raw = terrainIn[i];
      var code = (typeof raw === 'number' && isFinite(raw)) ? Math.floor(raw) : QFR.TERRAIN.EMPTY;
      terrain.push(code);
    }

    var teamsIn = QFR.arr(o && o.teams);
    var teams = teamsIn.map(function (t, idx) {
      var tt = (t && typeof t === 'object') ? t : {};
      return {
        team_id: QFR.num(tt.team_id, idx),
        ai_name: QFR.str(tt.ai_name, ''), // 缺失按 "" 处理，UI 显示「未知」
        base_x: QFR.num(tt.base_x, 0),
        base_y: QFR.num(tt.base_y, 0)
      };
    });

    return {
      __present: o !== null, // 提供给上层判断「init 是否真的存在」
      engine_version: o ? QFR.num(o.engine_version, null) : null,
      rules_version: o ? QFR.num(o.rules_version, null) : null,
      map_gen_version: o ? QFR.num(o.map_gen_version, null) : null,
      map: {
        width: width,
        height: height,
        map_gen_version: QFR.num(mapRaw.map_gen_version, null),
        terrain: terrain,
        terrainLengthMismatch: terrainIn.length !== width * height,
        terrainRawLength: terrainIn.length
      },
      teams: teams,
      max_ticks: QFR.num(o && o.max_ticks, 0),
      flag_spawn_interval: QFR.num(o && o.flag_spawn_interval, 0),
      center_radius: QFR.num(o && o.center_radius, -1), // -1 = 未提供，不画中心区
      seed: QFR.num(o && o.seed, 0)
    };
  }

  function normalizeEnd(raw, init, frames) {
    var o = (raw && typeof raw === 'object') ? raw : null;
    var lastFrame = frames.length ? frames[frames.length - 1] : null;
    var teamCount = init.teams.length;
    var scores = QFR.arr(o && o.scores);
    // end 缺失 scores 时退化为「末帧分数」——比显示 0 更接近真相。
    if (!o || !scores.length) scores = (lastFrame ? lastFrame.scores : []);
    var i;
    var normScores = [];
    for (i = 0; i < Math.max(teamCount, scores.length); i++) normScores.push(QFR.num(scores[i], 0));

    var winnerRaw = o ? o.winner : null;
    return {
      __present: o !== null,
      match_index: QFR.num(o && o.match_index, 0),
      seed: QFR.num(o && o.seed, init.seed),
      map_gen_version: QFR.num(o && o.map_gen_version, init.map_gen_version),
      ticks: QFR.num(o && o.ticks, lastFrame ? lastFrame.tick : 0),
      scores: normScores,
      kills: QFR.arr(o && o.kills),
      deaths: QFR.arr(o && o.deaths),
      // winner === null 表示平局（协议明文规定），必须区分「平局」与「缺失」。
      winner: (winnerRaw === null || winnerRaw === undefined) ? null : QFR.num(winnerRaw, null),
      winnerProvided: !!(o && winnerRaw !== undefined),
      ai_names: QFR.arr(o && o.ai_names).map(function (s) { return QFR.str(s, ''); })
    };
  }

  /* -------------------------- 地形语义与查询 ----------------------------- */

  // 编码 → 语义。判断依据是「本局实际队伍数」而不是固定的 3..6：
  // 若 init 缺失（teamCount=0），编码 3..6 应视为未知地形而不是凭空画出 4 个阵营区。
  // 未知地形按空地渲染 + 控制台警告（规范 §3.1 要求兜底，不抛异常）。
  function terrainKind(code, teamCount) {
    if (code === QFR.TERRAIN.EMPTY) return 'empty';
    if (code === QFR.TERRAIN.WALL) return 'wall';
    if (code === QFR.TERRAIN.VOID) return 'void';
    var n = typeof teamCount === 'number' && isFinite(teamCount) ? teamCount : 0;
    if (code >= QFR.TERRAIN.BASE_OFFSET && code < QFR.TERRAIN.BASE_OFFSET + n) return 'base';
    return 'unknown';
  }

  function baseTeamOf(code) {
    return code >= QFR.TERRAIN.BASE_OFFSET ? code - QFR.TERRAIN.BASE_OFFSET : null;
  }

  // 回放对象上的「访问器」：渲染/日志只通过这些函数取数，将来协议升级便于统一替换。
  function createAccessors(replay) {
    return {
      replay: replay,
      width: function () { return replay.init.map.width; },
      height: function () { return replay.init.map.height; },
      terrainAt: function (x, y) {
        var w = replay.init.map.width, h = replay.init.map.height;
        if (x < 0 || y < 0 || x >= w || y >= h) return -1; // 越界：调用方当「无地形」处理
        return replay.init.map.terrain[y * w + x];
      },
      teamCount: function () { return replay.init.teams.length; },
      aiName: function (team) {
        var t = replay.init.teams[team];
        if (t && t.ai_name) return t.ai_name;
        var en = replay.end && replay.end.ai_names ? replay.end.ai_names[team] : '';
        return typeof en === 'string' && en ? en : '未知';
      },
      // 单位 ID → 队伍：事件日志里只给了 unit id，必须能反查队伍才能写出「蓝队单位 3」。
      teamOfUnit: function (unitId) {
        var maps = replay.__unitTeam;
        if (maps && Object.prototype.hasOwnProperty.call(maps, unitId)) return maps[unitId];
        return null;
      },
      // 地图里阵营编码能反推队伍，但 units 也能给出更权威的映射；两者结合提高命中率。
      teamOfUnitInFrame: function (frame, unitId) {
        for (var i = 0; i < frame.units.length; i++) {
          if (frame.units[i].id === unitId) return frame.units[i].team;
        }
        var t = this.teamOfUnit(unitId);
        return t === null ? null : t;
      },
      frameAt: function (index) {
        if (!replay.frames.length) return null;
        var idx = QFR.clamp(index, 0, replay.frames.length - 1);
        return replay.frames[idx];
      },
      indexOfTick: function (tick) {
        // frames 已按 tick 升序排序，二分查找 → O(log n)；进度条拖动高频调用也不卡。
        var lo = 0, hi = replay.frames.length - 1, best = 0;
        while (lo <= hi) {
          var mid = (lo + hi) >> 1;
          if (replay.frames[mid].tick <= tick) { best = mid; lo = mid + 1; }
          else hi = mid - 1;
        }
      return best;
      }
    };
  }

  // 预建「单位 ID → 队伍」索引，供事件日志反查（事件里只有 unit id）。
  function buildUnitTeamIndex(replay) {
    var map = {};
    function feed(list) {
      for (var i = 0; i < list.length; i++) {
        var u = list[i];
        if (u && typeof u.id === 'number') map[u.id] = u.team;
      }
    }
    for (var f = 0; f < replay.frames.length; f++) feed(replay.frames[f].units);
    replay.__unitTeam = map;
  }

  /* ------------------------------ 入口：加载 ------------------------------ */

  /**
   * 通过相对/绝对 URL 加载回放。用同步 XMLHttpRequest：
   *  - file:// 下 fetch 会被 CORS 拒（XHR 在 --allow-file-access-from-files 时可用）；
   *  - 同步 XHR 在经典脚本里最简单，且文件很小（几十 KB），不会卡住体验。
   * 失败时把原因写进 reject，由 main.js 渲染到页面上（规范 §7：不许静默）。
   */
  function loadUrl(url) {
    return new Promise(function (resolve, reject) {
      var xhr = new XMLHttpRequest();
      try {
        xhr.open('GET', url, true);
      } catch (e) {
        reject(new Error('无法打开回放地址：' + url + '（' + e.message + '）'));
        return;
      }
      // file:// 下 status 可能是 0 但 responseText 有内容，不能只看 status。
      xhr.onload = function () {
        if ((xhr.status >= 200 && xhr.status < 300) || (xhr.status === 0 && xhr.responseText)) {
          resolve({ text: xhr.responseText, url: url });
        } else {
          reject(new Error('加载回放失败：HTTP ' + xhr.status + ' ' + url));
        }
      };
      xhr.onerror = function () {
        reject(new Error('加载回放失败：' + url +
          '（file:// 下请用 chromium 的 --allow-file-access-from-files，或改用「选择文件」按钮）'));
      };
      try {
        xhr.send(null);
      } catch (e) {
        reject(new Error('读取回放失败：' + e.message));
      }
    });
  }

  QFR.parser = {
    parseText: parseText,
    loadUrl: loadUrl,
    createAccessors: createAccessors,
    buildUnitTeamIndex: buildUnitTeamIndex,
    terrainKind: terrainKind,
    baseTeamOf: baseTeamOf,
    normalizeInit: normalizeInit
  };
})(window);
