#!/usr/bin/env bash
# =============================================================================
# 项目验收脚本（Lead 使用）
# =============================================================================
# 设计意图：把「交付物是否真的成立」拆成可独立判定的 5 个验收关卡，任何一关失败
# 都立刻以非零退出码中止，避免出现「看起来跑完了但其实某项没验」的情况。
#
# 为什么每关单独写一节而不是串成一长串命令：
#   1. 关卡之间失败原因完全不同（编译 / 规则 / 协议 / 前端 / 版本一致性），
#      分开输出才能一眼定位是哪一层的回归；
#   2. 便于在修复某一层后只重跑对应关卡（ARTIFACT_DIR 复用已有产物）。
#
# 关卡：
#   1. workspace 全量测试
#   2. CLI 批量对局（2 队固定随机 + 3 队每局随机）产出四件套
#   3. 回放契约校验（tools/validate_replay.py）
#   4. 三个版本号一致性（manifest / summary / 回放 init 三处）
#   5. Web UI 无头取证（tools/shot.js，CDP 截图 + window.__QFR_STATUS__ 断言），
#      读的是关卡 2 生成的真实回放，外加手写样例与错误路径
#
# 用法：
#   tools/acceptance.sh              # 跑全部关卡
#   tools/acceptance.sh 3            # 只跑第 3 关（复用上次产物目录）
#   ARTIFACT_DIR=out/accept tools/acceptance.sh
#
# 注意：所有 cargo 调用都必须走 ./scripts/cargo —— 默认 $HOME/.cargo 是只读的，
# 直接调用 cargo 会得到 "Read-only file system (os error 30)"。
# =============================================================================
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ARTIFACT_DIR="${ARTIFACT_DIR:-out/accept}"
ONLY="${1:-all}"

# run_stage 负责统一「关卡标题 + 计时 + 日志落盘」，日志留在 ARTIFACT_DIR 下，
# 出问题时可回溯完整输出而不是只有最后几行。
STAGE_LOG_DIR="$ARTIFACT_DIR/logs"
mkdir -p "$STAGE_LOG_DIR"

run_stage() {
  local num="$1" title="$2"
  shift 2
  if [[ "$ONLY" != "all" && "$ONLY" != "$num" ]]; then
    echo "== 跳过关卡 $num（$title）"
    return 0
  fi
  local log="$STAGE_LOG_DIR/stage_${num}.log"
  echo "== 关卡 $num：$title"
  local start=$SECONDS
  if "$@" >"$log" 2>&1; then
    echo "   OK（$((SECONDS - start))s，日志 $log）"
  else
    echo "   FAIL（$((SECONDS - start))s）—— 日志尾部："
    tail -n 40 "$log" | sed 's/^/   | /'
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# 关卡 1：全 workspace 测试
# ---------------------------------------------------------------------------
stage1() {
  ./scripts/cargo test --workspace
}

# ---------------------------------------------------------------------------
# 关卡 2：CLI 冒烟，产出真实回放供后续关卡使用
# ---------------------------------------------------------------------------
stage2() {
  rm -rf "$ARTIFACT_DIR/run2p" "$ARTIFACT_DIR/run3p"
  # 2 队：fixed-random + base-seed，保证回归可复现（同一条命令必须得到同样的结果）
  ./scripts/cargo run -p cli --release -- run \
    --matches 12 --teams 2 --max-ticks 120 \
    --seed-mode fixed-random --base-seed 20240501 \
    --replay-sample all \
    -o "$ARTIFACT_DIR/run2p"
  # 3 队：per-match + base-seed，验证「随机种子序列本身可复现」这条契约
  ./scripts/cargo run -p cli --release -- run \
    --matches 6 --teams 3 --max-ticks 120 \
    --seed-mode per-match --base-seed 20240501 \
    --ai 0=random --ai 1=greedy_flag --ai 2=defender \
    --replay-sample all \
    -o "$ARTIFACT_DIR/run3p"
  test -s "$ARTIFACT_DIR/run2p/manifest.json"
  test -s "$ARTIFACT_DIR/run2p/summary.json"
  test -s "$ARTIFACT_DIR/run2p/matches.jsonl"
  test -s "$ARTIFACT_DIR/run3p/manifest.json"
  ls "$ARTIFACT_DIR"/run2p/replays/*.jsonl >/dev/null
  ls "$ARTIFACT_DIR"/run3p/replays/*.jsonl >/dev/null
}

# ---------------------------------------------------------------------------
# 关卡 3：回放契约校验（真实引擎产物必须满足与手写样例同一套不变量）
# ---------------------------------------------------------------------------
stage3() {
  # 显式带上 --expect-* 三件套。原因：不带期望值时验证器只检查「回放自身结构自洽」，
  # 而「本次交付必须是 1/1/1」是另一条独立契约——版本号写错但内部一致，它看不见。
  python3 tools/validate_replay.py \
    --expect-engine 1 --expect-rules 2 --expect-map-gen 1 \
    samples/demo_2p.jsonl \
    "$ARTIFACT_DIR"/run2p/replays/*.jsonl \
    "$ARTIFACT_DIR"/run3p/replays/*.jsonl
  # 反证：验证器必须能识破故意构造的坏数据，否则上面那句「0 error」不构成证据。
  # 注意反证也必须带 --expect-*：version_mismatch 的内部结构是自洽的，
  # 只有「拿期望版本去比」才能发现它（这正是真实使用场景：CLI 侧知道自己该是什么版本）。
  for bad in samples/version_mismatch.jsonl samples/malformed.jsonl; do
    if python3 tools/validate_replay.py --quiet \
      --expect-engine 1 --expect-rules 2 --expect-map-gen 1 "$bad"; then
      echo "验证器未能识别故意构造的坏样例：$bad" >&2
      return 1
    fi
  done
  echo "反证通过：version_mismatch 与 malformed 均被验证器判为失败"
}

# ---------------------------------------------------------------------------
# 关卡 4：版本号三件套一致性
# ---------------------------------------------------------------------------
# 设计意图：版本号分散在 manifest / summary / 每份回放 init 中，一旦某处漏写或
# 写旧值，Web UI 就会按错误策略解析。这里用脚本逐处比对，避免靠肉眼抽查。
stage4() {
  python3 tools/check_versions.py "$ARTIFACT_DIR/run2p" "$ARTIFACT_DIR/run3p"
}

# ---------------------------------------------------------------------------
# 关卡 5：Web UI 无头取证（真实浏览器 + 状态断言 + 截图）
# ---------------------------------------------------------------------------
stage5() {
  local shot_dir="$ARTIFACT_DIR/screenshots"
  mkdir -p "$shot_dir"

  # 这里刻意**不用** `chromium --headless --screenshot=...`。实测该路径会整块丢掉
  # 「回放加载完成后由 JS 插入 DOM 的告警横幅」：同一 URL、同一 chromium，
  #   · CLI 截图：警示色 #ffe9b0 像素 0 个、横幅底色 23 个；
  #   · CDP 截图：警示色 1500+ 个（横幅 86px 高，自然高度）。
  # 原因是 CLI 那条路径拿的是插入横幅之前提交的合成表面，且加长 --virtual-time-budget、
  # 加 --run-all-compositor-stages-before-draw 都无效。用 CLI 截图当证据会把
  # 「版本不匹配警告」这一条正确行为误判成缺陷。
  #
  # 因此改用 Lead 自有的 tools/shot.js：走 DevTools Protocol，等页面自己上报
  # window.__QFR_READY__，先断言 window.__QFR_STATUS__（渲染实体数 / 告警条数 / 错误列表）
  # 与横幅的布局可见性，再让浏览器回传合成后的画面。断言失败即非零退出 → 直接当关卡。
  shot_cdp() {
    local out="$1" query="$2"
    shift 2
    local url="file://$ROOT/webui/index.html?$query"
    if ! node tools/shot.js --url "$url" --out "$out" "$@" >>"$STAGE_LOG_DIR/stage_5_shot.log" 2>&1; then
      echo "CDP 取证失败：$query（详见 $STAGE_LOG_DIR/stage_5_shot.log）" >&2
      tail -20 "$STAGE_LOG_DIR/stage_5_shot.log" >&2
      return 1
    fi
    # 双保险：PNG 太小基本等于空白页（正常满屏截图都在 100KB 以上）。
    local size
    size=$(stat -c '%s' "$out" 2>/dev/null || echo 0)
    if [[ "$size" -lt 20000 ]]; then
      echo "截图疑似空白：$out 只有 ${size} 字节" >&2
      return 1
    fi
    echo "   OK：$query → $(basename "$out")（${size} 字节）"
  }

  : >"$STAGE_LOG_DIR/stage_5_shot.log"

  # 1) 真实引擎回放（关卡 2 的产物）：证明「前后端真的打通」，而不是只吃手写样例。
  local replay_name replay_rel
  replay_name="$(basename "$(ls "$ARTIFACT_DIR"/run2p/replays/*.jsonl | head -n1)")"
  replay_rel="../$ARTIFACT_DIR/run2p/replays/$replay_name"
  shot_cdp "$shot_dir/real_2p.png" "replay=$replay_rel&tick=30" --require-status

  # 2) 正常样例（2 队 / 3 队）：只需要「渲染出实体且没有错误」。
  shot_cdp "$shot_dir/demo_2p.png" "replay=../samples/demo_2p.jsonl&tick=12" --require-status
  shot_cdp "$shot_dir/demo_3p.png" "replay=../samples/demo_3p.jsonl&tick=12" --require-status

  # 3) 版本号不匹配：必须「继续渲染」+「横幅可见 + 告警文案非空」，两者都要断言。
  shot_cdp "$shot_dir/version_mismatch.png" "replay=../samples/version_mismatch.jsonl&tick=5" \
    --require-status --require-warnings

  # 4) 畸形 JSONL：坏行只算告警、不算致命错误（--allow-errors），仍要渲染出实体。
  #    该样例只有 tick 2/3 两帧，所以显式指定 tick=3，避免「请求不存在的帧 → 没有实体」。
  shot_cdp "$shot_dir/malformed.png" "replay=../samples/malformed.jsonl&tick=3" \
    --require-status --allow-errors --require-warnings

  # 5) 非回放文件：必须给可读的错误提示（window.__QFR_ERROR__）而不是白屏。
  shot_cdp "$shot_dir/not_a_replay.png" "replay=../README.md" --no-expect-ready --no-require-status

  # 6) 极小窗口：规范要求「再小也要能看到控制与进度」。这里只作尺寸回归（不额外断言），
  #    若哪天布局把 footer 挤没了，截图会明显变形、体积骤降，便于人工发现。
  node tools/shot.js --url "file://$ROOT/webui/index.html?replay=$replay_rel&tick=30" \
    --out "$shot_dir/tiny_window.png" --width 520 --height 360 --require-status \
    >>"$STAGE_LOG_DIR/stage_5_shot.log" 2>&1 || {
      echo "极小窗口取证失败（详见 $STAGE_LOG_DIR/stage_5_shot.log）" >&2
      return 1
    }
}


run_stage 1 "workspace 测试" stage1
run_stage 2 "CLI 批量对局产出四件套" stage2
run_stage 3 "回放契约校验" stage3
run_stage 4 "版本号三件套一致性" stage4
run_stage 5 "Web UI 无头截图" stage5

echo "== 全部关卡通过。产物在 $ARTIFACT_DIR"
