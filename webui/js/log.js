/* =============================================================================
 * webui/js/log.js —— 事件日志（人类可读中文）+ 信息面板
 * -----------------------------------------------------------------------------
 * 职责：
 *   A. 把协议事件翻译成中文句子（见 eventText），并按队伍过滤；
 *   B. 只在「tick 变化」时重建事件列表 DOM（规范 §5：不要每帧重建）；
 *   C. 渲染信息面板（tick/分数/AI/存活数/旗数/炸弹数，结束时显示赢家与击杀死亡）。
 * 依赖：core.js、state.js、parser 的 accessors。
 *
 * 设计意图与为什么：
 *   1. 事件文本是「用户数据 + 模板」：AI 名字、非法动作 reason 都可能含 < > & 等字符。
 *      因此一律用 textContent / createElement 构造，禁止 innerHTML 拼接（规范 §1 硬约束）。
 *   2. 每帧的事件只有几条，重建整个列表（几十行）代价可忽略；相比之下「增量 diff」
 *      容易在跳转（往前跳）时残留旧事件，正确性优先。
 *   3. 未知事件类型原样显示类型名 —— 向前兼容，不丢信息也不报错。
 * ========================================================================== */
(function (global) {
  'use strict';

  var QFR = global.QFR;

  /* ------------------------- 事件 → 中文描述 ------------------------------ */

  // 每个函数返回 { team: 队伍ID|null, icon: 类别标记, text: 中文文本 }。
  // team 用于「按队伍过滤」：引擎的 related_team 语义 =
  //   攻击/死亡/移动/拾旗/得分 → 主动方或相关方队伍；炸弹爆炸 → 放置者队伍。
  var EVENT_RENDERERS = {
    unit_moved: function (e, ctx) {
      return {
        team: ctx.teamOfUnit(e.unit),
        icon: 'moved',
        text: ctx.u(e.unit) + ' 从 ' + QFR.xy(e.from_x, e.from_y) + ' 移动到 ' + QFR.xy(e.to_x, e.to_y)
      };
    },
    unit_attacked: function (e, ctx) {
      return {
        team: ctx.teamOfUnit(e.attacker),
        icon: 'attacked',
        text: ctx.u(e.attacker) + ' 攻击 ' + ctx.u(e.target) + '，造成 ' + QFR.num(e.damage, 0) + ' 点伤害'
      };
    },
    unit_died: function (e, ctx) {
      var by = (e.by === null || e.by === undefined) ? null : e.by;
      var byText;
      if (by === null) byText = '死因不明';
      else if (by === e.unit) byText = '被自己放置的炸弹炸死';
      else byText = '被' + ctx.u(by) + '击杀';
      return {
        team: QFR.num(e.team, ctx.teamOfUnit(e.unit)),
        icon: 'died',
        text: ctx.teamName(e.team) + '单位 ' + QFR.num(e.unit, '?') + ' 阵亡（' + byText + '）'
      };
    },
    unit_respawned: function (e, ctx) {
      return {
        team: QFR.num(e.team, ctx.teamOfUnit(e.unit)),
        icon: 'respawned',
        text: ctx.teamName(e.team) + '单位 ' + QFR.num(e.unit, '?') + ' 在己方阵营满血复活于 ' + QFR.xy(e.x, e.y)
      };
    },
    flag_picked: function (e, ctx) {
      return {
        team: ctx.teamOfUnit(e.unit),
        icon: 'flag',
        text: ctx.u(e.unit) + ' 拾取旗 #' + QFR.num(e.flag, '?')
      };
    },
    flag_dropped: function (e, ctx) {
      return {
        team: null, // 旗落地本身不属于任何队伍：按队伍过滤时归入「中立」
        icon: 'flag',
        text: '旗 #' + QFR.num(e.flag, '?') + ' 掉落在 ' + QFR.xy(e.x, e.y) + '（携带者阵亡）'
      };
    },
    flag_spawned: function (e, ctx) {
      return {
        team: null,
        icon: 'flag',
        text: '中心区刷新新旗 #' + QFR.num(e.flag, '?') + ' 于 ' + QFR.xy(e.x, e.y)
      };
    },
    score: function (e, ctx) {
      return {
        team: QFR.num(e.team, ctx.teamOfUnit(e.unit)),
        icon: 'score',
        text: ctx.teamName(e.team) + '单位 ' + QFR.num(e.unit, '?') + ' 把旗 #' + QFR.num(e.flag, '?') +
          ' 带回阵营得分，' + ctx.teamName(e.team) + ' 当前 ' + QFR.num(e.new_score, '?') + ' 分'
      };
    },
    bomb_placed: function (e, ctx) {
      return {
        team: ctx.teamOfUnit(e.unit),
        icon: 'bomb',
        text: ctx.u(e.unit) + ' 在 ' + QFR.xy(e.x, e.y) + ' 放置炸弹 #' + QFR.num(e.bomb, '?') +
          '（引信 ' + QFR.num(e.timer, '?') + ' 回合；炸弹会友伤，不分敌我）'
      };
    },
    bomb_exploded: function (e, ctx) {
      var hits = QFR.arr(e.hit_units);
      var hitText = hits.length ? '命中单位 [' + hits.join(', ') + ']（' + ctx.teamNameList(hits) + '）'
        : '没有命中任何单位';
      return {
        team: null,
        icon: 'boom',
        text: '炸弹 #' + QFR.num(e.bomb, '?') + ' 在 ' + QFR.xy(e.x, e.y) + ' 爆炸（半径 ' +
          QFR.num(e.radius, '?') + '），' + hitText
      };
    },
    move_conflict: function (e, ctx) {
      var units = QFR.arr(e.units);
      return {
        team: null,
        icon: 'conflict',
        text: '移动冲突：单位 [' + units.join(', ') + '] 同 tick 都想进入 ' + QFR.xy(e.x, e.y) +
          '，全部留在原地（消耗 1 AP）'
      };
    },
    illegal_action: function (e, ctx) {
      var act = QFR.str(e.action, '未知动作');
      var reason = QFR.str(e.reason, '未给出原因');
      // ★ 安全要点：action / reason 是引擎里 AI 提交的原始字符串，属于不可信数据。
      //   这里只生成纯文本，DOM 侧一律 textContent 写入。
      return {
        team: ctx.teamOfUnit(e.unit),
        icon: 'illegal',
        text: ctx.u(e.unit) + ' 提交了非法动作「' + act + '」：' + reason + '（引擎按等待处理）'
      };
    }
  };

  var EVENT_ICON_TEXT = {
    moved: '移', attacked: '攻', died: '亡', respawned: '生', flag: '旗',
    score: '分', bomb: '弹', boom: '爆', conflict: '冲', illegal: '非', unknown: '?'
  };

  // 构造事件上下文：提供「单位 ID → 队伍/名字」的反查与队伍名格式化。
  function makeContext(accessors, frame) {
    var memo = {};
    function teamOfUnit(id) {
      if (id === null || id === undefined) return null;
      if (Object.prototype.hasOwnProperty.call(memo, id)) return memo[id];
      var t = accessors.teamOfUnitInFrame(frame, id);
      memo[id] = t;
      return t;
    }
    return {
      teamOfUnit: teamOfUnit,
      teamName: function (team) {
        if (team === null || team === undefined) return '未知队';
        var label = QFR.teamLabel(team);
        var ai = accessors.aiName(team);
        return (ai && ai !== '未知') ? label + '（' + ai + '）' : label;
      },
      teamNameList: function (ids) {
        var seen = {}, parts = [];
        for (var i = 0; i < ids.length; i++) {
          var t = teamOfUnit(ids[i]);
          var key = String(t);
          if (seen[key]) continue;
          seen[key] = true;
          parts.push(QFR.teamLabel(t));
        }
        return parts.length ? parts.join('、') : '未知队伍';
      },
      // 单位短名：能查到队伍就写「蓝队单位 3」，否则「单位 3」——绝不因为查不到而报错。
      u: function (id) {
        var t = teamOfUnit(id);
        if (t === null || t === undefined) return '单位 ' + QFR.num(id, '?');
        return QFR.teamLabel(t) + '单位 ' + QFR.num(id, '?');
      }
    };
  }

  /**
   * 把一个事件翻译成 { team, icon, text }。
   * @returns {{team:number|null, icon:string, text:string, unknownType:boolean}}
   */
  function eventText(event, accessors, frame) {
    var type = QFR.str(event && event.type, 'unknown');
    var ctx = makeContext(accessors, frame);
    var renderer = EVENT_RENDERERS[type];
    if (!renderer) {
      // 未知事件：显示原始类型名 + 关键字段的紧凑摘要（不是 JSON 原文），便于调试新协议。
      var keys = [];
      for (var k in event) {
        if (!Object.prototype.hasOwnProperty.call(event, k)) continue;
        if (k === 'type') continue;
        var v = event[k];
        if (v === null || typeof v === 'object') continue; // 嵌套对象不展开，避免变成「贴 JSON」
        keys.push(k + '=' + String(v));
        if (keys.length >= 6) break;
      }
      return {
        team: null,
        icon: 'unknown',
        text: '未知事件类型「' + type + '」' + (keys.length ? '（' + keys.join('，') + '）' : ''),
        unknownType: true
      };
    }
    var r = renderer(event, ctx);
    r.unknownType = false;
    return r;
  }

  /* --------------------------- 事件日志 DOM ------------------------------- */

  var els = {}; // 缓存 DOM 引用，避免每次更新都查 DOM

  function bindDom(refs) { els = refs; }

  function appendRow(container, item, tick) {
    var row = document.createElement('div');
    row.className = 'log-row log-' + item.icon;
    if (item.team !== null && item.team !== undefined) row.setAttribute('data-team', String(item.team));

    var tickEl = document.createElement('span');
    tickEl.className = 'log-tick';
    tickEl.textContent = 'tick ' + tick;

    var iconEl = document.createElement('span');
    iconEl.className = 'log-icon';
    iconEl.textContent = EVENT_ICON_TEXT[item.icon] || '?';

    var textEl = document.createElement('span');
    textEl.className = 'log-text';
    // ★ 唯一写入事件文本的地方：textContent，天然转义所有 < > & 与引号。
    textEl.textContent = item.text;

    row.appendChild(tickEl);
    row.appendChild(iconEl);
    row.appendChild(textEl);
    container.appendChild(row);
  }

  /**
   * 重建某帧的事件日志。只在 tick 变化时调用（state 的 'tick' 事件）。
   * 注意：日志按帧显示「本 tick 发生的事件」，而不是整个回放的历史 ——
   * 这样跳转到任意帧都能立刻看到该帧发生了什么，不会因为往回跳而看到未来事件。
   */
  function renderEvents(frame, accessors, filterTeam) {
    if (!els.logList) return 0;
    var list = els.logList;
    while (list.firstChild) list.removeChild(list.firstChild);

    if (!frame) {
      if (els.logEmpty) els.logEmpty.hidden = false;
      return 0;
    }
    var events = frame.events || [];
    var shown = 0;
    for (var i = 0; i < events.length; i++) {
      var item = eventText(events[i], accessors, frame);
      // 队伍过滤：'all' 显示全部；具体队伍只显示「该队相关」的事件；
      // 中立事件（旗刷新/爆炸/冲突，team=null）在过滤时归入每队都显示更啰嗦，
      // 因此过滤模式下只显示与该队直接相关的事件。
      if (filterTeam !== 'all') {
        if (item.team === null || item.team === undefined || item.team !== filterTeam) continue;
      }
      appendRow(list, item, frame.tick);
      shown++;
    }
    if (els.logEmpty) els.logEmpty.hidden = shown > 0;
    if (els.logEmpty) {
      els.logEmpty.textContent = events.length === 0
        ? '本 tick 没有事件（tick ' + frame.tick + '）'
        : '本 tick 的 ' + events.length + ' 条事件都被队伍过滤器隐藏了';
    }
    return shown;
  }

  /* ---------------------------- 信息面板 --------------------------------- */

  function setText(el, text) {
    if (el) el.textContent = text;
  }

  function renderInfo(replay, frame, accessors, index) {
    var init = replay.init;
    var total = replay.frames.length;

    // 同时给出「帧序号」与「tick 值」：正常回放两者相同（tick 从 1 递增），
    // 但缺 init/缺 tick 的坏文件里 tick 可能不连续，只显示一个会让人误解进度。
    setText(els.infoTick, frame
      ? ('帧 ' + (QFR.num(index, 0) + 1) + ' / ' + total + '　（tick ' + frame.tick + '）')
      : ('— / ' + total + ' 帧'));
    setText(els.infoFile, replay.meta && replay.meta.sourceName ? replay.meta.sourceName : '—');
    setText(els.infoMap, init.map.width + ' × ' + init.map.height +
      (init.max_ticks ? '，最大 ' + init.max_ticks + ' tick' : ''));

    // 存活/旗/炸弹统计
    var alive = 0, dead = 0, carried = 0;
    if (frame) {
      for (var i = 0; i < frame.units.length; i++) {
        if (frame.units[i].alive) alive++; else dead++;
      }
      for (var f = 0; f < frame.flags.length; f++) {
        if (frame.flags[f].carrier !== null) carried++;
      }
    }
    setText(els.infoUnits, alive + ' 存活 / ' + dead + ' 阵亡');
    setText(els.infoFlags, (frame ? frame.flags.length : 0) + ' 面（' + carried + ' 面被携带）');
    setText(els.infoBombs, (frame ? frame.bombs.length : 0) + ' 个');

    // 分数牌 + AI 名字
    if (els.scoreBoard) {
      var board = els.scoreBoard;
      while (board.firstChild) board.removeChild(board.firstChild);
      var teams = init.teams;
      var teamCount = Math.max(teams.length, (frame ? frame.scores.length : 0), replay.end.scores.length);
      var best = -Infinity;
      var scores = frame ? frame.scores : [];
      var t;
      for (t = 0; t < teamCount; t++) best = Math.max(best, QFR.num(scores[t], 0));

      for (t = 0; t < teamCount; t++) {
        var score = QFR.num(scores[t], 0);
        var card = document.createElement('div');
        card.className = 'score-card' + (score === best && best > 0 ? ' leading' : '');
        card.style.setProperty('--team-color', QFR.teamColor(t));

        var dot = document.createElement('span');
        dot.className = 'score-dot';
        var name = document.createElement('span');
        name.className = 'score-name';
        name.textContent = QFR.teamLabel(t) + ' · ' + accessors.aiName(t);
        var val = document.createElement('span');
        val.className = 'score-val';
        val.textContent = String(score);

        card.appendChild(dot);
        card.appendChild(name);
        card.appendChild(val);
        board.appendChild(card);
      }
    }

    // 结束结论：只有 position 在末帧 且 有 end 行时才显示（避免播放中途剧透）。
    if (els.resultBox) {
      var atEnd = frame && index >= total - 1;
      if (atEnd) {
        els.resultBox.hidden = false;
        while (els.resultBox.firstChild) els.resultBox.removeChild(els.resultBox.firstChild);

        var title = document.createElement('div');
        title.className = 'result-title';
        if (!replay.end.__present) {
          title.textContent = '未提供最终结果（回放缺少 end 行）';
        } else if (replay.end.winner === null) {
          title.textContent = '最终结果：平局（各队并列最高分）';
        } else {
          title.textContent = '最终结果：' + QFR.teamLabel(replay.end.winner) + ' 获胜';
          title.style.color = QFR.teamColor(replay.end.winner);
        }
        els.resultBox.appendChild(title);

        var detail = document.createElement('div');
        detail.className = 'result-detail';
        var parts = [];
        for (t = 0; t < teamCount; t++) {
          parts.push(QFR.teamLabel(t) + ' 得分 ' + QFR.num(replay.end.scores[t], 0) +
            '、击杀 ' + QFR.num(replay.end.kills[t], 0) +
            '、死亡 ' + QFR.num(replay.end.deaths[t], 0));
        }
        detail.textContent = parts.join(' ｜ ');
        els.resultBox.appendChild(detail);
      } else {
        els.resultBox.hidden = true;
      }
    }
  }

  QFR.log = {
    bindDom: bindDom,
    eventText: eventText,
    renderEvents: renderEvents,
    renderInfo: renderInfo,
    EVENT_ICON_TEXT: EVENT_ICON_TEXT
  };
})(window);
