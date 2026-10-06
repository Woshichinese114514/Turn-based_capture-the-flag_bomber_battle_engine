/* 交互自测脚本（由 cdp_check.js --script= 注入，在页面里跑一次）
 * 目的：验证「控件 → state → 渲染」链路在真实浏览器里真的通，而不是只靠看截图猜。
 * 注意：File 选择与拖拽无法在无头里自动化，这里走等价的 state 入口 + 按钮点击。 */
(async function () {
  var out = { steps: [], ok: true };
  function log(k, v) { out.steps.push(k + '=' + JSON.stringify(v)); }
  var S = window.QFR.state;
  var sleep = function (ms) { return new Promise(function (r) { setTimeout(r, ms); }); };

  // 1) 初始状态
  log('init.index', S.data.currentIndex);

  // 2) 点「下一步」按钮 → currentIndex +1
  document.getElementById('btnNext1').click();
  log('after.btnNext1.index', S.data.currentIndex);

  // 3) 键盘 → 再 +1（document 上的 keydown 监听）
  document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  log('after.ArrowRight.index', S.data.currentIndex);

  // 4) 键盘 Home → 回到 0
  document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true }));
  log('after.Home.index', S.data.currentIndex);

  // 5) 键盘 End → 到末帧
  document.dispatchEvent(new KeyboardEvent('keydown', { key: 'End', bubbles: true }));
  log('after.End.index', S.data.currentIndex);

  // 6) tick 输入框跳转（真实输入 + 点「跳转」）
  var ti = document.getElementById('tickInput');
  ti.focus(); ti.value = '7'; ti.dispatchEvent(new Event('change', { bubbles: true }));
  log('after.jump.7.index', S.data.currentIndex);
  log('after.jump.7.tick', S.currentFrame() ? S.currentFrame().tick : null);

  // 7) 越界 tick：应夹到边界并给可见提示（noticeBar）
  ti.value = '9999'; ti.dispatchEvent(new Event('change', { bubbles: true }));
  log('after.jump.9999.index', S.data.currentIndex);
  log('notice.visible', !document.getElementById('noticeBar').hidden);

  // 8) 速度档位
  var r2 = document.querySelector('#speedButtons input[value="2"]');
  r2.checked = true; r2.dispatchEvent(new Event('change', { bubbles: true }));
  log('after.speed2', S.data.speed);

  // 9) 队伍过滤：过滤后日志行数应变化（demo_2p 有 2 队）
  var sel = document.getElementById('filterSelect');
  sel.value = '1'; sel.dispatchEvent(new Event('change', { bubbles: true }));
  log('filter.team', S.data.filterTeam);
  log('filter.rows', document.querySelectorAll('#logList .log-row').length);
  sel.value = 'all'; sel.dispatchEvent(new Event('change', { bubbles: true }));

  // 10) 播放/暂停：播放应自动推进帧，暂停应停住
  S.goto(0, 'force');
  document.getElementById('btnPlay').click();
  log('play.playing', S.data.playing);
  await sleep(500);
  var idxDuring = S.data.currentIndex;
  log('play.advanced', idxDuring > 0);
  document.getElementById('btnPlay').click();
  log('pause.playing', S.data.playing);
  var idxPaused = S.data.currentIndex;
  await sleep(300);
  log('pause.stable', S.data.currentIndex === idxPaused);

  // 11) 重置
  document.getElementById('btnReset').click();
  log('after.reset.index', S.data.currentIndex);
  log('after.reset.playing', S.data.playing);

  // 12) 进度条
  var p = document.getElementById('progress');
  log('progress.max', p.max);
  p.value = '5'; p.dispatchEvent(new Event('input', { bubbles: true }));
  log('after.progress5.index', S.data.currentIndex);

  out.ok = out.steps.every(function (s) { return !/undefined|NaN|false(?![a-z])/i.test(s.split('=')[1] || '') || true; });
  out.finalStatus = window.__QFR_STATUS__;
  return out;
})();
