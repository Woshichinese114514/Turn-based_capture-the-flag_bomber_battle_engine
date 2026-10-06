#!/usr/bin/env python3
"""回放 JSONL 契约验证器（Rust 端与 Web UI 端共用的「权威」校验工具）。

设计意图
--------
回放格式是这个项目唯一的跨边界契约：Rust 端导出、Web UI 端消费。文档（docs/replay-format.md）
是给人读的，本脚本是给 CI / 交付验收用的**可执行契约**——Rust 端每跑完一批对局都应该拿它
校验回放，Web UI 开发者也可以在写解析器之前先用它确认样例文件是合法的。

检查分三级：
* **error**：违反契约（缺字段、类型错、tick 不连续、地形数组长度不对…）→ 退出码 1。
* **warning**：可疑但不一定错（未知事件类型 = 未来版本的新事件、死亡单位倒计时为 0、
  帧数与 end.ticks 不一致…）→ 不影响退出码，但会打印出来。
* 只做**结构/一致性**校验，不校验规则（例如「攻击距离必须 ≤3」属于引擎自测范围，
  见 docs/rules.md 第 14 节）。

用法
----
    python3 tools/validate_replay.py samples/demo_2p.jsonl
    python3 tools/validate_replay.py --expect-engine 1 out/replays/*.jsonl
    python3 tools/validate_replay.py --quiet out/replays/*.jsonl   # 只输出失败信息
"""

from __future__ import annotations

import argparse
import glob
import json
import sys
from collections import Counter
from typing import Any

# 契约规定的 12 种事件及其必填字段（docs/replay-format.md 第 5 节）。
# 新增事件类型时必须同时改这里、protocol 的 GameEvent、文档与 Web UI 的事件描述函数。
EVENT_FIELDS: dict[str, tuple[str, ...]] = {
    "unit_moved": ("unit", "from_x", "from_y", "to_x", "to_y"),
    "unit_attacked": ("attacker", "target", "damage"),
    "unit_died": ("unit", "team", "by"),
    "unit_respawned": ("unit", "team", "x", "y"),
    "flag_picked": ("unit", "flag"),
    "flag_dropped": ("flag", "x", "y"),
    "flag_spawned": ("flag", "x", "y"),
    "score": ("team", "unit", "flag", "new_score"),
    "bomb_placed": ("unit", "bomb", "x", "y", "timer"),
    "bomb_exploded": ("bomb", "x", "y", "radius", "hit_units"),
    "move_conflict": ("x", "y", "units"),
    "illegal_action": ("unit", "action", "reason"),
}

TERRAIN_EMPTY = 0
TERRAIN_WALL = 1
TERRAIN_VOID = 2
TERRAIN_BASE_OFFSET = 3

# 事件里带坐标的字段名（用于统一做「坐标必须在图内」的检查）。
EVENT_COORD_FIELDS: dict[str, tuple[str, ...]] = {
    "unit_moved": ("from_x", "from_y", "to_x", "to_y"),
    "unit_respawned": ("x", "y"),
    "flag_dropped": ("x", "y"),
    "flag_spawned": ("x", "y"),
    "bomb_placed": ("x", "y"),
    "bomb_exploded": ("x", "y"),
    "move_conflict": ("x", "y"),
}


class Report:
    """收集一个文件的所有诊断信息。

    刻意不抛异常：校验器要尽可能把同一份文件里的问题一次性全报出来，
    而不是遇到第一个问题就退出（那样修一轮跑一轮，效率太低）。
    """

    def __init__(self, path: str, max_errors: int) -> None:
        self.path = path
        self.max_errors = max_errors
        self.errors: list[str] = []
        self.warnings: list[str] = []
        # 统计信息，供 --quiet 之外的人类阅读
        self.stats: dict[str, Any] = {}
        self.event_counts: Counter[str] = Counter()

    def error(self, line: int | None, message: str) -> None:
        where = f"{self.path}:{line}: " if line else f"{self.path}: "
        self.errors.append(f"{where}ERROR {message}")

    def warn(self, line: int | None, message: str) -> None:
        where = f"{self.path}:{line}: " if line else f"{self.path}: "
        self.warnings.append(f"{where}WARN  {message}")

    @property
    def failed(self) -> bool:
        return bool(self.errors)


def _is_int(value: Any) -> bool:
    """JSON 里没有 int/float 的严格区分，但契约要求坐标与计数必须是整数。

    `True` 在 Python 里是 `int` 的子类，必须显式排除，否则 `"alive": true` 会被误判成数字。
    """
    return isinstance(value, int) and not isinstance(value, bool)


def _require(obj: dict[str, Any], keys: tuple[str, ...], report: Report, line: int, ctx: str) -> bool:
    """检查必填字段是否齐全（只检查存在性，类型由各自的检查函数负责）。"""
    ok = True
    for key in keys:
        if key not in obj:
            report.error(line, f"{ctx} 缺少字段 {key!r}")
            ok = False
    return ok


def _check_coord(obj: dict[str, Any], x_key: str, y_key: str, width: int, height: int,
                 report: Report, line: int, ctx: str) -> None:
    """坐标必须是整数且在 [0,width) × [0,height) 内。

    之所以连 `to_x` 这种「移动目标格」也要检查：引擎不该把单位移到图外，
    出现这种现象说明结算有 bug，早点在回放校验里暴露出来，比在 UI 里画到画布外面好。
    """
    for key in (x_key, y_key):
        if key in obj and not _is_int(obj[key]):
            report.error(line, f"{ctx} 的 {key} 必须是整数，得到 {obj[key]!r}")
            return
    if x_key not in obj or y_key not in obj:
        return
    x, y = obj[x_key], obj[y_key]
    if not (0 <= x < width and 0 <= y < height):
        report.error(line, f"{ctx} 坐标 ({x},{y}) 越界（地图 {width}×{height}）")


def _check_event(event: Any, report: Report, line: int, teams: int, width: int, height: int,
                 unit_ids: set[int]) -> None:
    if not isinstance(event, dict):
        report.error(line, f"事件必须是对象，得到 {type(event).__name__}")
        return
    etype = event.get("type")
    if not isinstance(etype, str):
        report.error(line, f"事件缺少字符串 type 字段：{event!r}")
        return
    report.event_counts[etype] += 1
    if etype not in EVENT_FIELDS:
        # 未知事件类型不算错误：这是向前兼容——旧 UI 读新引擎的回放时应当忽略而不是崩。
        report.warn(line, f"未知事件类型 {etype!r}（可能是更新版本引擎新增的事件）")
        return
    _require(event, EVENT_FIELDS[etype], report, line, f"{etype} 事件")

    if "x" in EVENT_COORD_FIELDS.get(etype, ()):
        _check_coord(event, "x", "y", width, height, report, line, f"{etype} 事件")
    if etype == "unit_moved":
        _check_coord(event, "from_x", "from_y", width, height, report, line, "unit_moved 起点")
        _check_coord(event, "to_x", "to_y", width, height, report, line, "unit_moved 终点")

    # 引用完整性：事件里出现的单位/队伍 ID 必须在合法范围内
    for key in ("unit", "attacker", "target"):
        if key in event and _is_int(event[key]) and event[key] not in unit_ids:
            report.warn(line, f"{etype} 事件的 {key}={event[key]} 不在本帧单位列表中")
    if "team" in event and _is_int(event["team"]) and not (0 <= event["team"] < teams):
        report.error(line, f"{etype} 事件的 team={event['team']} 超出队伍数 {teams}")
    if etype == "move_conflict":
        units = event.get("units")
        if not isinstance(units, list) or len(units) < 2:
            report.error(line, f"move_conflict 的 units 必须是长度 ≥2 的数组，得到 {units!r}")
    if etype == "bomb_placed" and "timer" in event and event["timer"] not in (1, 2):
        report.warn(line, f"bomb_placed 的 timer={event['timer']}，契约中放置时应为 2（或刚落地的 1）")
    if etype == "bomb_exploded" and not isinstance(event.get("hit_units"), list):
        report.error(line, "bomb_exploded 的 hit_units 必须是数组")


def validate_file(path: str, args: argparse.Namespace) -> Report:
    report = Report(path, args.max_errors)
    try:
        with open(path, "r", encoding="utf-8") as handle:
            raw_lines = handle.read().splitlines()
    except OSError as exc:
        report.error(None, f"无法读取文件：{exc}")
        return report

    if not raw_lines:
        report.error(None, "文件为空")
        return report

    # 逐行解析。空行只警告（有些工具会写尾随空行），半截行/非法 JSON 是错误。
    lines: list[tuple[int, dict[str, Any]]] = []
    for index, raw in enumerate(raw_lines, start=1):
        if not raw.strip():
            report.warn(index, "空行（JSONL 不应有空行，但可以安全跳过）")
            continue
        try:
            obj = json.loads(raw)
        except json.JSONDecodeError as exc:
            report.error(index, f"不是合法 JSON：{exc.msg}")
            continue
        if not isinstance(obj, dict):
            report.error(index, f"每行必须是 JSON 对象，得到 {type(obj).__name__}")
            continue
        line_type = obj.get("type")
        if line_type is None:
            report.error(index, "缺少 type 字段（应为 init/frame/end）")
            continue
        lines.append((index, obj))

    if not lines:
        report.error(None, "没有任何合法的 JSONL 行")
        return report

    first_line, first = lines[0]
    last_line, last = lines[-1]
    if first.get("type") != "init":
        report.error(first_line, f"第一行必须是 init，得到 {first.get('type')!r}")
    if last.get("type") != "end":
        report.error(last_line, f"最后一行必须是 end，得到 {last.get('type')!r}")
    if len(lines) < 2:
        report.error(None, "至少要有一行 init 和一行 end")
        return report

    init = first if first.get("type") == "init" else {}
    # ---- init 行 ----
    teams = 0
    width = height = 0
    unit_ids: set[int] = set()
    if init:
        _require(init, ("engine_version", "rules_version", "map_gen_version", "map", "teams",
                        "max_ticks", "flag_spawn_interval", "center_radius", "seed"),
                 report, first_line, "init")
        # 注意：init.teams 是 TeamInfo 数组（不是队伍数量字段）。队伍数 = 数组长度，
        # 这是协议里的一个容易误解的点：没有单独的 teams 计数，避免两处数字打架。
        teams_list = init.get("teams")
        teams = len(teams_list) if isinstance(teams_list, list) else 0
        map_obj = init.get("map") if isinstance(init.get("map"), dict) else {}
        _require(map_obj, ("width", "height", "map_gen_version", "terrain"), report, first_line, "init.map")
        width = map_obj.get("width", 0)
        height = map_obj.get("height", 0)
        terrain = map_obj.get("terrain")
        if not _is_int(width) or not _is_int(height) or width <= 0 or height <= 0:
            report.error(first_line, f"地图尺寸非法：{width}×{height}")
            width = height = 0
        if not isinstance(terrain, list):
            report.error(first_line, "init.map.terrain 必须是数组")
        elif width and height:
            if len(terrain) != width * height:
                report.error(first_line,
                             f"地形数组长度 {len(terrain)} ≠ width*height = {width * height}")
            max_code = TERRAIN_BASE_OFFSET + max(teams - 1, 0)
            for i, code in enumerate(terrain):
                if not _is_int(code) or not (TERRAIN_EMPTY <= code <= max_code):
                    report.error(first_line,
                                 f"地形码非法：索引 {i}(x={i % width},y={i // width}) = {code!r}"
                                 f"（允许 0..{max_code}）")
                    break
        teams_list = init.get("teams")
        if not isinstance(teams_list, list) or not teams_list:
            report.error(first_line, "init.teams 必须是非空数组（每个元素是一支队伍，长度即队伍数）")
        else:
            seen_teams = set()
            for info in teams_list:
                if not isinstance(info, dict):
                    report.error(first_line, f"init.teams 的元素必须是对象：{info!r}")
                    continue
                _require(info, ("team_id", "base_x", "base_y"), report, first_line, "init.teams[]")
                tid = info.get("team_id")
                if _is_int(tid):
                    if tid in seen_teams:
                        report.error(first_line, f"init.teams 里 team_id={tid} 重复")
                    seen_teams.add(tid)
                    if not (0 <= tid < teams):
                        report.error(first_line, f"team_id={tid} 超出 teams={teams}")
                if _is_int(info.get("base_x")) and _is_int(info.get("base_y")) and width and height:
                    bx, by = info["base_x"], info["base_y"]
                    if not (0 <= bx <= width - 3 and 0 <= by <= height - 3):
                        report.error(first_line, f"阵营区左上角 ({bx},{by}) 越界，3×3 放不下")
                    elif isinstance(terrain, list) and len(terrain) == width * height and _is_int(tid):
                        # 阵营区 9 格必须都是 TeamBase(team_id) —— 地形与 teams[] 必须自洽
                        for dy in range(3):
                            for dx in range(3):
                                code = terrain[(by + dy) * width + (bx + dx)]
                                if _is_int(code) and code != TERRAIN_BASE_OFFSET + tid:
                                    report.error(first_line,
                                                 f"阵营区格 ({bx + dx},{by + dy}) 的地形码 {code} "
                                                 f"应为 {TERRAIN_BASE_OFFSET + tid}（team {tid}）")
                                    break
                            else:
                                continue
                            break
        for key, expected in (("engine_version", args.expect_engine),
                             ("rules_version", args.expect_rules),
                             ("map_gen_version", args.expect_map_gen)):
            if expected is not None and init.get(key) != expected:
                report.error(first_line, f"{key}={init.get(key)!r}，期望 {expected}")
        report.stats["init"] = {
            "engine_version": init.get("engine_version"),
            "rules_version": init.get("rules_version"),
            "map_gen_version": init.get("map_gen_version"),
            "teams": teams,
            "map": f"{width}×{height}",
            "max_ticks": init.get("max_ticks"),
            "center_radius": init.get("center_radius"),
            "flag_spawn_interval": init.get("flag_spawn_interval"),
        }

    # ---- frame 行 ----
    expected_tick = 1
    frame_count = 0
    for line_no, obj in lines[1:-1]:
        if obj.get("type") != "frame":
            report.error(line_no, f"中间行必须是 frame，得到 {obj.get('type')!r}")
            continue
        frame_count += 1
        _require(obj, ("tick", "scores", "units", "flags", "bombs", "events"),
                 report, line_no, "frame")
        tick = obj.get("tick")
        if not _is_int(tick):
            report.error(line_no, f"tick 必须是整数，得到 {tick!r}")
        else:
            if tick != expected_tick:
                report.error(line_no, f"tick={tick} 不连续，期望 {expected_tick}（必须从 1 起逐 1 递增）")
            expected_tick = tick + 1
            if _is_int(init.get("max_ticks")) and tick > init["max_ticks"]:
                report.error(line_no, f"tick={tick} 超过 max_ticks={init['max_ticks']}")
        scores = obj.get("scores")
        if not isinstance(scores, list) or (teams and len(scores) != teams):
            report.error(line_no, f"scores 必须是长度 {teams} 的数组，得到 {scores!r}")
        elif any(not _is_int(s) for s in scores):
            report.error(line_no, f"scores 必须全为整数：{scores!r}")

        units = obj.get("units")
        unit_ids = set()
        if not isinstance(units, list):
            report.error(line_no, "units 必须是数组")
            units = []
        for unit in units:
            if not isinstance(unit, dict):
                report.error(line_no, f"单位必须是对象：{unit!r}")
                continue
            if not _require(unit, ("id", "team", "x", "y", "hp", "alive", "respawn_timer",
                                   "carrying_flag", "attacked_this_turn"),
                            report, line_no, "unit"):
                continue
            uid = unit.get("id")
            if _is_int(uid):
                if uid in unit_ids:
                    report.error(line_no, f"单位 id={uid} 在同一帧内重复")
                unit_ids.add(uid)
            if _is_int(unit.get("team")) and teams and not (0 <= unit["team"] < teams):
                report.error(line_no, f"单位 {uid} 的 team={unit['team']} 超出队伍数 {teams}")
            if not isinstance(unit.get("alive"), bool):
                report.error(line_no, f"单位 {uid} 的 alive 必须是布尔值")
            if _is_int(unit.get("hp")) and not (0 <= unit["hp"] <= 3):
                report.error(line_no, f"单位 {uid} 的 hp={unit['hp']} 超出 0..=3")
            if _is_int(unit.get("hp")) and isinstance(unit.get("alive"), bool):
                if unit["alive"] and unit["hp"] == 0:
                    report.error(line_no, f"单位 {uid} 标记存活但 hp=0")
                if unit["alive"] and unit["respawn_timer"] != 0:
                    report.warn(line_no, f"单位 {uid} 存活但 respawn_timer={unit['respawn_timer']}")
            _check_coord(unit, "x", "y", width, height, report, line_no, f"单位 {uid}")
            # 存活单位必须站在可通行地形上：墙/虚空上出现活单位一定是引擎 bug
            if unit.get("alive") and isinstance(terrain, list) and len(terrain) == width * height \
                    and _is_int(unit.get("x")) and _is_int(unit.get("y")):
                code = terrain[unit["y"] * width + unit["x"]]
                if _is_int(code) and code in (TERRAIN_WALL, TERRAIN_VOID):
                    report.error(line_no, f"存活单位 {uid} 站在不可通行地形 code={code} "
                                          f"({unit['x']},{unit['y']})")
                if _is_int(code) and code >= TERRAIN_BASE_OFFSET and _is_int(unit.get("team")) \
                        and code - TERRAIN_BASE_OFFSET != unit["team"]:
                    report.warn(line_no, f"单位 {uid}(team {unit['team']}) 站在敌方阵营格 "
                                         f"({unit['x']},{unit['y']})")

        flags = obj.get("flags")
        if not isinstance(flags, list):
            report.error(line_no, "flags 必须是数组")
            flags = []
        units_by_id = {u.get("id"): u for u in units if isinstance(u, dict) and _is_int(u.get("id"))}
        for flag in flags:
            if not isinstance(flag, dict):
                report.error(line_no, f"旗必须是对象：{flag!r}")
                continue
            if not _require(flag, ("id", "x", "y", "carrier"), report, line_no, "flag"):
                continue
            _check_coord(flag, "x", "y", width, height, report, line_no, f"旗 {flag.get('id')}")
            carrier = flag.get("carrier")
            if carrier is not None:
                holder = units_by_id.get(carrier)
                if holder is None:
                    report.error(line_no, f"旗 {flag.get('id')} 的携带者 {carrier} 不在单位列表中")
                else:
                    if (holder.get("x"), holder.get("y")) != (flag.get("x"), flag.get("y")):
                        report.error(line_no, f"旗 {flag.get('id')} 被携带，但其坐标 "
                                              f"({flag.get('x')},{flag.get('y')}) 与携带者 "
                                              f"{carrier} 的坐标 "
                                              f"({holder.get('x')},{holder.get('y')}) 不一致")
                    if holder.get("carrying_flag") != flag.get("id"):
                        report.error(line_no, f"旗 {flag.get('id')} 的携带者 {carrier} 的 "
                                              f"carrying_flag={holder.get('carrying_flag')!r} 不匹配")
                if carrier in units_by_id and units_by_id[carrier].get("alive") is False:
                    report.error(line_no, f"旗 {flag.get('id')} 的携带者 {carrier} 已死亡")

        bombs = obj.get("bombs")
        if not isinstance(bombs, list):
            report.error(line_no, "bombs 必须是数组")
            bombs = []
        for bomb in bombs:
            if not isinstance(bomb, dict):
                report.error(line_no, f"炸弹必须是对象：{bomb!r}")
                continue
            if not _require(bomb, ("id", "x", "y", "team", "timer", "radius"), report, line_no, "bomb"):
                continue
            _check_coord(bomb, "x", "y", width, height, report, line_no, f"炸弹 {bomb.get('id')}")
            if _is_int(bomb.get("timer")) and bomb["timer"] not in (1, 2):
                report.error(line_no, f"炸弹 {bomb.get('id')} 的 timer={bomb['timer']}，"
                                      f"契约中只应出现 1 或 2（0 的那一 tick 就爆炸了）")
            if _is_int(bomb.get("team")) and teams and not (0 <= bomb["team"] < teams):
                report.error(line_no, f"炸弹 {bomb.get('id')} 的 team={bomb['team']} 超出队伍数")

        events = obj.get("events")
        if not isinstance(events, list):
            report.error(line_no, "events 必须是数组")
        else:
            for event in events:
                _check_event(event, report, line_no, teams, width, height, unit_ids)

    # ---- end 行 ----
    if last.get("type") == "end":
        _require(last, ("match_index", "seed", "map_gen_version", "ticks", "scores", "kills",
                        "deaths", "winner", "ai_names"), report, last_line, "end")
        teams = teams or len(last.get("scores") or [])
        for key in ("scores", "kills", "deaths", "ai_names"):
            value = last.get(key)
            if isinstance(value, list) and teams and len(value) != teams:
                report.error(last_line, f"end.{key} 长度 {len(value)} ≠ teams={teams}")
        winner = last.get("winner")
        if winner is not None and (not _is_int(winner) or (teams and not (0 <= winner < teams))):
            report.error(last_line, f"end.winner={winner!r} 非法（应为 null 或 0..teams-1）")
        if _is_int(last.get("ticks")) and frame_count and last["ticks"] != frame_count:
            report.warn(last_line, f"end.ticks={last['ticks']} 与 frame 行数 {frame_count} 不一致")
        report.stats["end"] = {"ticks": last.get("ticks"), "scores": last.get("scores"),
                               "winner": winner, "ai_names": last.get("ai_names")}

    report.stats["frames"] = frame_count
    report.stats["events"] = dict(report.event_counts)
    return report


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="校验回放 JSONL 是否符合 docs/replay-format.md")
    parser.add_argument("files", nargs="+", help="回放文件路径（支持 shell 通配符展开后的多文件）")
    parser.add_argument("--expect-engine", type=int, default=None, help="要求 engine_version == N")
    parser.add_argument("--expect-rules", type=int, default=None, help="要求 rules_version == N")
    parser.add_argument("--expect-map-gen", type=int, default=None, help="要求 map_gen_version == N")
    parser.add_argument("--quiet", action="store_true", help="只打印失败信息与总结果")
    parser.add_argument("--max-errors", type=int, default=25,
                        help="单个文件最多报告多少条错误（默认 25，避免刷屏）")
    args = parser.parse_args(argv)

    # 支持把通配符当普通参数传进来（Windows / 某些 shell 不展开）
    paths: list[str] = []
    for pattern in args.files:
        expanded = sorted(glob.glob(pattern))
        paths.extend(expanded if expanded else [pattern])

    total_errors = 0
    total_warnings = 0
    failed_files = 0
    for path in paths:
        report = validate_file(path, args)
        total_errors += len(report.errors)
        total_warnings += len(report.warnings)
        if report.failed:
            failed_files += 1
        if not args.quiet:
            for line in report.warnings:
                print(line)
            for line in report.errors[:args.max_errors]:
                print(line)
            if len(report.errors) > args.max_errors:
                print(f"{path}: ... 还有 {len(report.errors) - args.max_errors} 条错误未显示")
            stats = report.stats
            if stats:
                print(f"{path}: OK-ish  {stats.get('init', {}).get('map', '?')} "
                      f"teams={stats.get('init', {}).get('teams', '?')} "
                      f"frames={stats.get('frames', 0)} "
                      f"versions=({stats.get('init', {}).get('engine_version', '?')}/"
                      f"{stats.get('init', {}).get('rules_version', '?')}/"
                      f"{stats.get('init', {}).get('map_gen_version', '?')}) "
                      f"events={stats.get('events', {})}")
        elif report.failed:
            for line in report.errors:
                print(line)

    print(f"\n校验完成：{len(paths)} 个文件，错误 {total_errors}，警告 {total_warnings}，"
          f"失败文件 {failed_files}")
    if total_errors:
        print("提示：错误定义见 docs/replay-format.md（字段表、地形编码、事件表、容错策略）")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
