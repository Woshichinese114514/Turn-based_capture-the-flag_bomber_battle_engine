#!/usr/bin/env python3
"""AI 行为分析器：把一批回放里的「无意义动作」量化出来，用于 AI 改进前后对比。

## 为什么需要它

`summary.json` 只告诉你胜率与评分，看不出 AI 是不是在**空转**：
两个单位互相抢同一格、拾不到旗却整局来回横跳、把 AP 花在同一个目标上……
这些行为不会让对局崩溃，但会显著拉低强度。本工具把三类信号从回放里数出来：

1. **移动冲突**（`move_conflict`）：同一 tick 多个单位请求进入同一格，
   引擎的裁决是「全部留在原地**且都消耗 1 AP**」。对 AI 来说这是纯浪费，
   属于可以靠队内协调（认领目标格）避免的部分；
2. **非法动作**（`illegal_action`）：AI 规划了引擎必然拒绝的动作（AP 不足、
   目标已死、旗已被抢走……）。基线 AI 里它主要来自「多队同时抢同一面旗」的竞态，
   但数值明显上涨就说明 AI 开始凭过时快照决策；
3. **来回横跳**（backtrack）：某单位走 A→B 又马上折回 A（位置序列 A→B→A）。
   它是「死循环」最直接的可观测形式，通常出现在寻路不可达时的兜底随机走位里。
   相邻的另一个信号是**停顿**（stall）：走了一步之后又停住不动（A→B→B），
   代表单位到达目标附近却动不了（被堵、目标被抢走），也算走位效率损失。

另外统计 **掉入虚空**（`unit_died` 且 `by` 为 null、死亡格地形为虚空），
用来验证「虚空致死」这条规则真的在引擎里生效。

## 用法

    python3 tools/analyze_ai.py out/run_a out/run_b ...
    python3 tools/analyze_ai.py --json out/run_a      # 输出机器可读的 JSON

每个参数应是一个「输出目录」（含 `matches.jsonl` 与 `replays/`）。
按 AI 名字聚合：同一名字在多个槽位出场时合并统计（与 `summary.json` 的 by_ai_name 口径一致）。
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections import Counter, defaultdict

# 12 种事件之外的事件（新协议版本新增的）也应被容忍：只统计我们认识的类型。
MOVED = "unit_moved"
CONFLICT = "move_conflict"
ILLEGAL = "illegal_action"
DIED = "unit_died"


def iter_replay_files(run_dir: str):
    """按文件名排序返回 `replays/*.jsonl`（排序保证报告可复现）。"""
    replays = os.path.join(run_dir, "replays")
    if not os.path.isdir(replays):
        return []
    names = sorted(n for n in os.listdir(replays) if n.endswith(".jsonl"))
    return [os.path.join(replays, n) for n in names]


def load_replay(path: str):
    """逐行解析一份回放：返回 (init, frames, end)；畸形行跳过而不是崩溃。

    这里刻意不复用 `tools/validate_replay.py` 的严格校验：分析器要能在
    「回放本身有瑕疵」的情况下仍然给出统计（例如手工构造的样例）。
    """
    init = None
    frames = []
    end = None
    bad = 0
    with open(path, "r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                bad += 1
                continue
            kind = row.get("type")
            if kind == "init":
                init = row
            elif kind == "frame":
                frames.append(row)
            elif kind == "end":
                end = row
    return init, frames, end, bad


def terrain_grid(init):
    """把 init 的地形数组还原成 `terrain[(x, y)] -> 整数编码` 的查询函数。"""
    if not init:
        return None
    mp = init.get("map") or {}
    width = mp.get("width")
    height = mp.get("height")
    terrain = mp.get("terrain")
    if not isinstance(width, int) or not isinstance(height, int) or not isinstance(terrain, list):
        return None

    def at(x, y):
        if not (0 <= x < width and 0 <= y < height):
            return None
        idx = y * width + x
        return terrain[idx] if idx < len(terrain) else None

    return at


def unit_positions(frame):
    """`{unit_id: (x, y, alive)}`；字段缺失时用 0 兜底（容错优先）。"""
    out = {}
    for unit in frame.get("units") or []:
        if not isinstance(unit, dict):
            continue
        uid = unit.get("id")
        if not isinstance(uid, int):
            continue
        out[uid] = (unit.get("x"), unit.get("y"), bool(unit.get("alive")))
    return out


def analyze_replay(path: str):
    """统计单份回放的「无意义动作」计数与队伍构成。"""
    init, frames, end, bad = load_replay(path)
    team_names = []
    if init:
        for team in init.get("teams") or []:
            if isinstance(team, dict):
                team_names.append(team.get("ai_name") or f"team{team.get('team_id')}")
    at = terrain_grid(init)

    # 单位 → 队伍 的映射。有了它才能把「冲突/非法/横跳」归因到**具体某个 AI**：
    # 一次回放内单位 ID 全局唯一且队伍不会变，只要在任意一帧见过该单位就能定下它的队伍。
    team_of = {}
    per_team = defaultdict(Counter)

    events = Counter()
    illegal_reasons = Counter()
    conflicts = 0
    void_falls = 0
    backtracks = 0
    stalls = 0

    # 逐单位记录最近两个 tick 的位置，用来识别两类「走位浪费」：
    # * backtracks：A→B→A，即走一步又原地折回（死循环最直接的证据）；
    # * stalls：A→B→B，即走一步之后又停下（被堵住/目标被抢走的犹豫信号）。
    # 两者用同一份历史计算，避免为了一个指标再扫一遍回放。
    prev = {}
    for frame in frames:
        positions = unit_positions(frame)
        # 先登记本帧出现的单位，再处理事件，这样同帧事件也能归因到队伍。
        for unit in frame.get("units") or []:
            if not isinstance(unit, dict):
                continue
            uid, team = unit.get("id"), unit.get("team")
            if isinstance(uid, int) and isinstance(team, int):
                team_of[uid] = team
        for event in frame.get("events") or []:
            if not isinstance(event, dict):
                continue
            kind = event.get("type")
            events[kind] += 1
            if kind == MOVED:
                uid = event.get("unit")
                if uid in team_of:
                    per_team[team_of[uid]]["moves"] += 1
            if kind == CONFLICT:
                conflicts += 1
                # 一格被 N 个单位抢，就把这次冲突按 1/N 记给每个参与者所属的 AI。
                units = event.get("units") or []
                share = 1.0 / max(1, len(units))
                for uid in units:
                    if uid in team_of:
                        per_team[team_of[uid]]["conflicts"] += share
            elif kind == ILLEGAL:
                # 只保留原因前缀（去掉可能变化的数字），便于聚合成「原因排行」。
                reason = str(event.get("reason") or "")
                illegal_reasons[reason] += 1
                uid = event.get("unit")
                if uid in team_of:
                    per_team[team_of[uid]]["illegal"] += 1
            elif kind == DIED:
                uid = event.get("unit")
                pos = positions.get(uid)
                if at is not None and pos and pos[0] is not None and pos[1] is not None:
                    if at(pos[0], pos[1]) == 2:  # 地形编码 2 = 虚空
                        void_falls += 1
                        if uid in team_of:
                            per_team[team_of[uid]]["void_falls"] += 1
        for uid, (x, y, alive) in positions.items():
            if not alive:
                prev.pop(uid, None)
                continue
            last = prev.get(uid)
            if last is not None:
                # last = (上一 tick 位置, 上上 tick 位置)；当前位置是 (x, y)。
                last_pos, before_pos = last
                if before_pos == (x, y) and last_pos != (x, y):
                    backtracks += 1  # A→B→A：又走回去了
                    if uid in team_of:
                        per_team[team_of[uid]]["backtracks"] += 1
                elif last_pos == (x, y) and before_pos != (x, y):
                    stalls += 1  # A→B→B：走了一步又停住
                    if uid in team_of:
                        per_team[team_of[uid]]["stalls"] += 1
                prev[uid] = ((x, y), last_pos)
            else:
                prev[uid] = ((x, y), (x, y))


    return {
        "path": path,
        "team_names": team_names,
        "bad_lines": bad,
        "ticks": end.get("ticks") if isinstance(end, dict) else None,
        "scores": end.get("scores") if isinstance(end, dict) else None,
        "winner": end.get("winner") if isinstance(end, dict) else None,
        "conflicts": conflicts,
        "illegal": sum(v for k, v in events.items() if k == ILLEGAL),
        "moves": events[MOVED],
        "backtracks": backtracks,
        "stalls": stalls,
        "void_falls": void_falls,
        "per_team": per_team,
        "illegal_reasons": illegal_reasons,
        "events": events,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="把一批回放的 AI 行为量化（冲突/非法/横跳/停顿/掉虚空）")
    parser.add_argument("runs", nargs="+", help="输出目录（含 matches.jsonl 与 replays/）")
    parser.add_argument("--json", action="store_true", help="以 JSON 输出（便于脚本比对）")
    args = parser.parse_args()

    report = {"runs": []}
    for run_dir in args.runs:
        run = {"dir": run_dir, "replays": 0, "by_ai": {}, "totals": Counter()}
        agg = defaultdict(Counter)
        for path in iter_replay_files(run_dir):
            result = analyze_replay(path)
            run["replays"] += 1
            for key in ("conflicts", "illegal", "moves", "backtracks", "stalls", "void_falls", "bad_lines"):
                # 只累加总计数；**不要**动 `agg`——`agg` 按 AI 名字分桶，
                # 混进指标名会让报告里冒出「conflicts」这种假 AI（第一版就是这么错的）。
                run["totals"][key] += result[key]
            # 每份回放的两支/三支队伍各记一次（与 summary.json 的出场次数口径一致），
            # 因此这里的「每局均值」是「每次出场均值」，比较时要注意分母。
            for index, name in enumerate(result["team_names"]):
                bucket = agg[name]
                bucket["appearances"] += 1
                # 行为指标按「事件所属单位」归因到该单位的 AI（见 analyze_replay 的 team_of），
                # 不再按队伍数平摊——平摊会把同一个 AI 的指标印成一模一样（第一版的问题）。
                for key in ("conflicts", "illegal", "moves", "backtracks", "stalls", "void_falls"):
                    bucket[key] += result["per_team"][index][key]
                if result["scores"]:
                    idx = result["team_names"].index(name)
                    if idx < len(result["scores"]):
                        bucket["score_sum"] += result["scores"][idx]
                if result["winner"] is not None and result["winner"] < len(result["team_names"]):
                    if result["team_names"][result["winner"]] == name:
                        bucket["wins"] += 1
                elif result["winner"] is None:
                    bucket["draws"] += 1
        for name, bucket in sorted(agg.items()):
            appearances = max(1, bucket["appearances"])
            run["by_ai"][name] = {
                "appearances": bucket["appearances"],
                "wins": bucket["wins"],
                "draws": bucket["draws"],
                "win_rate": round(bucket["wins"] / appearances, 3),
                "avg_score": round(bucket["score_sum"] / appearances, 2),
                "conflicts_per_match": round(bucket["conflicts"] / appearances, 2),
                "illegal_per_match": round(bucket["illegal"] / appearances, 2),
                "moves_per_match": round(bucket["moves"] / appearances, 2),
                "backtracks_per_match": round(bucket["backtracks"] / appearances, 2),
                "stalls_per_match": round(bucket["stalls"] / appearances, 2),
                "void_falls_per_match": round(bucket["void_falls"] / appearances, 2),
            }
        run["totals"] = dict(run["totals"])
        report["runs"].append(run)

    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2))
        return 0

    for run in report["runs"]:
        print(f"== {run['dir']}（{run['replays']} 份回放）")
        totals = run["totals"]
        print(
            "   合计: 移动 {moves} / 冲突 {conflicts} / 非法 {illegal} / 横跳 {backtracks} / 停顿 {stalls} / 掉虚空 {void_falls}".format(
                **totals
            )
        )
        for name, stats in run["by_ai"].items():
            print(
                "   {name:<12} 出场 {appearances:>4} 胜 {wins:>4} 平 {draws:>4} 胜率 {win_rate:>5}"
                " 均分 {avg_score:>6} | 冲突/出场 {conflicts_per_match:>5} 非法/出场 {illegal_per_match:>5}"
                " 横跳/出场 {backtracks_per_match:>5} 停顿/出场 {stalls_per_match:>5}"
                " 掉虚空/出场 {void_falls_per_match:>5}".format(
                    name=name, **stats
                )
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
