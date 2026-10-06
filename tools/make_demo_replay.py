#!/usr/bin/env python3
"""生成「手写样例回放」——供 Web UI 在 Rust 引擎就绪之前开发与自测。

这**不是**游戏引擎，也不是第二套规则实现：它只是一个**确定性的编排脚本**，
用最简单的策略（各单位朝最近的旗或地图中心前进、够得着就打、顺手放炸弹）跑出一局面，
让 12 种事件都真实出现一次。真正的规则与数值以 `crates/sim`（Rust）为准，
它的输出才是回放内容的唯一权威来源；本脚本的产物只是**结构合法的样例**。

产物（写到 `samples/`）：
* `demo_2p.jsonl`        2 队、15×15、约 24 tick，覆盖全部 12 种事件
* `demo_3p.jsonl`        3 队版本，用于验证 UI 的「队伍数不为 2」分支
* `version_mismatch.jsonl` 把 demo 的版本号改成未来版本（UI 应警告但继续渲染）
* `malformed.jsonl`      故意混入坏行（非法 JSON、缺字段、未知 type、空行、半截文件）

用法：
    python3 tools/make_demo_replay.py --out samples
"""

from __future__ import annotations

import argparse
import copy
import json
import os
from typing import Any

TERRAIN_EMPTY = 0
TERRAIN_WALL = 1
TERRAIN_VOID = 2
TERRAIN_BASE_OFFSET = 3

# 与 crates/protocol/src/versions.rs 保持一致（协议版本 → 样例版本）。
ENGINE_VERSION = 1
RULES_VERSION = 2

WIDTH = 15
HEIGHT = 15
CENTER = (7, 7)
CENTER_RADIUS = 4
FLAG_SPAWN_INTERVAL = 5
MAX_TICKS = 24
DEMO_SEED = 20240501

DIRECTIONS = {"up": (0, -1), "down": (0, 1), "left": (-1, 0), "right": (1, 0)}

# 墙体位置：刻意避开中心十字通道，保证双方一定能走到中心区（演示用，不做连通性校验）
WALLS = [(5, 7), (6, 7), (7, 3), (8, 3), (9, 9), (10, 9), (3, 8), (4, 8),
         (11, 5), (12, 5), (5, 11), (6, 11), (3, 3), (11, 3)]
VOIDS = [(7, 0), (7, 14), (0, 13), (14, 1)]


def base_top_left(teams: int, team: int) -> tuple[int, int]:
    """阵营区左上角约定（必须与 docs/internal-api.md 第 2 节、mapgen 一致）。"""
    if teams == 2:
        return (1, 1) if team == 0 else (WIDTH - 4, HEIGHT - 4)
    return [(1, 1), (WIDTH - 4, 1), ((WIDTH - 3) // 2, HEIGHT - 4)][team]


def build_terrain(teams: int) -> list[int]:
    terrain = [TERRAIN_EMPTY] * (WIDTH * HEIGHT)
    for (x, y) in WALLS:
        terrain[y * WIDTH + x] = TERRAIN_WALL
    for (x, y) in VOIDS:
        terrain[y * WIDTH + x] = TERRAIN_VOID
    for team in range(teams):
        bx, by = base_top_left(teams, team)
        for dy in range(3):
            for dx in range(3):
                terrain[(by + dy) * WIDTH + (bx + dx)] = TERRAIN_BASE_OFFSET + team
    return terrain


def in_base(teams: int, x: int, y: int) -> int | None:
    """返回该格所属阵营的 team_id，不属于任何阵营返回 None。"""
    for team in range(teams):
        bx, by = base_top_left(teams, team)
        if bx <= x < bx + 3 and by <= y < by + 3:
            return team
    return None


def is_walkable(terrain: list[int], x: int, y: int) -> bool:
    if not (0 <= x < WIDTH and 0 <= y < HEIGHT):
        return False
    code = terrain[y * WIDTH + x]
    return code == TERRAIN_EMPTY or code >= TERRAIN_BASE_OFFSET


def los_clear(terrain: list[int], x0: int, y0: int, x1: int, y1: int) -> bool:
    """Bresenham 直线视线检查：起点与终点之外的格子只要有一堵墙就看不见。

    与 Rust 端规则一致（docs/rules.md 第 3 节）：墙阻挡视线，单位与虚空不阻挡。
    """
    dx, dy = abs(x1 - x0), abs(y1 - y0)
    sx = 1 if x0 < x1 else -1
    sy = 1 if y0 < y1 else -1
    err = dx - dy
    x, y = x0, y0
    while (x, y) != (x1, y1):
        if (x, y) != (x0, y0) and terrain[y * WIDTH + x] == TERRAIN_WALL:
            return False
        e2 = 2 * err
        if e2 > -dy:
            err -= dy
            x += sx
        if e2 < dx:
            err += dx
            y += sy
    return True


def manhattan(a: tuple[int, int], b: tuple[int, int]) -> int:
    return abs(a[0] - b[0]) + abs(a[1] - b[1])


def greedy_step(terrain: list[int], occupied: set[tuple[int, int]], src: tuple[int, int],
                dst: tuple[int, int], order: list[str]) -> tuple[str, tuple[int, int]] | None:
    """朝目标走一步：优先能缩短曼哈顿距离的方向，避开墙与已占格。

    允许回到「距离不变」的方向（绕墙用），但不允许走远。找不到就走不了（返回 None）。
    """
    best: tuple[int, tuple[int, int]] | None = None
    best_dist = manhattan(src, dst)
    for name in order:
        dx, dy = DIRECTIONS[name]
        nxt = (src[0] + dx, src[1] + dy)
        if not is_walkable(terrain, *nxt) or nxt in occupied:
            continue
        dist = manhattan(nxt, dst)
        if dist < best_dist or (dist == best_dist and best is None):
            best, best_dist = (name, nxt), dist
    return best


class Demo:
    def __init__(self, teams: int, ai_names: list[str]) -> None:
        self.teams = teams
        self.terrain = build_terrain(teams)
        self.units: dict[int, dict[str, Any]] = {}
        next_id = 1
        for team in range(teams):
            bx, by = base_top_left(teams, team)
            # 放在阵营区的三个角，避免初始就互相堵住
            cells = [(bx, by), (bx + 1, by), (bx, by + 1)]
            for (x, y) in cells:
                self.units[next_id] = {"id": next_id, "team": team, "x": x, "y": y, "hp": 3,
                                       "alive": True, "respawn_timer": 0, "carrying_flag": None,
                                       "attacked_this_turn": False}
                next_id += 1
        self.flags: dict[int, dict[str, Any]] = {}
        self.bombs: dict[int, dict[str, Any]] = {}
        self.scores = [0] * teams
        self.kills = [0] * teams
        self.deaths = [0] * teams
        self.next_flag_id = 1
        self.next_bomb_id = 1
        self.ai_names = ai_names

    # ---------- 查询辅助 ----------
    def unit_cells(self) -> set[tuple[int, int]]:
        return {(u["x"], u["y"]) for u in self.units.values() if u["alive"]}

    def flag_cells(self) -> set[tuple[int, int]]:
        return {(f["x"], f["y"]) for f in self.flags.values()}

    def enemy_of(self, unit: dict[str, Any]) -> list[dict[str, Any]]:
        return [u for u in self.units.values()
                if u["alive"] and u["team"] != unit["team"]]

    # ---------- 每个 tick 的策略（很简单：抢旗 > 打人 > 放炸弹 > 朝目标走）----------
    def plan(self, unit: dict[str, Any]) -> dict[str, Any]:
        ap = 2
        plan: dict[str, Any] = {"attack": None, "bomb": False, "pick": False, "moves": []}
        # 1) 脚下有旗 → 拾旗（最高优先级，否则会被自己走掉）
        if unit["carrying_flag"] is None:
            for flag in self.flags.values():
                if flag["carrier"] is None and (flag["x"], flag["y"]) == (unit["x"], unit["y"]):
                    plan["pick"] = True
                    ap -= 1
                    break
        # 2) 射程内敌人 → 攻击（携带旗时按规则不能攻击）
        if unit["carrying_flag"] is None and ap >= 1:
            for enemy in self.enemy_of(unit):
                if manhattan((unit["x"], unit["y"]), (enemy["x"], enemy["y"])) <= 3 and \
                        los_clear(self.terrain, unit["x"], unit["y"], enemy["x"], enemy["y"]):
                    plan["attack"] = enemy["id"]
                    ap -= 1
                    break
        # 3) 旁边有敌人且自己不在阵营里 → 放炸弹（演示炸弹爆炸/友伤/阵营免疫）
        if ap >= 1 and in_base(self.teams, unit["x"], unit["y"]) is None:
            for enemy in self.enemy_of(unit):
                if manhattan((unit["x"], unit["y"]), (enemy["x"], enemy["y"])) <= 2:
                    plan["bomb"] = True
                    ap -= 1
                    break
        # 4) 剩余的 AP 全部用来走向目标
        carrier = unit["carrying_flag"] is not None
        if carrier:
            bx, by = base_top_left(self.teams, unit["team"])
            target = (bx + 1, by + 1)
        else:
            ground = [f for f in self.flags.values() if f["carrier"] is None]
            target = (ground[0]["x"], ground[0]["y"]) if ground else CENTER
        if (unit["x"], unit["y"]) == target:
            return plan
        blocked = self.unit_cells()
        pos = (unit["x"], unit["y"])
        # 走动是分「轮」结算的（每轮所有单位各走一步、再判定撞格），见 execute()
        while ap > 0 and len(plan["moves"]) < 2:
            step = greedy_step(self.terrain, blocked - {pos}, pos, target,
                               ["up", "down", "left", "right"])
            if step is None:
                break
            name, nxt = step
            plan["moves"].append((name, nxt))
            pos = nxt
            ap -= 1
        return plan

    def snapshot(self, tick: int, events: list[dict[str, Any]]) -> dict[str, Any]:
        """把内部状态拍成一份完整快照（回放要求每帧全量，理由见 protocol 的 RenderFrame 注释）。

        注意旗的坐标：契约要求「被携带时旗的坐标等于携带者坐标」（引擎保证，UI 只按坐标画）。
        本脚本里旗自己不会移动，所以在快照阶段按携带者位置同步一次。
        """
        units = sorted(self.units.values(), key=lambda u: u["id"])
        by_id = {u["id"]: u for u in units}
        flags = []
        for flag in sorted(self.flags.values(), key=lambda f: f["id"]):
            carrier = flag["carrier"]
            holder = by_id.get(carrier) if carrier is not None else None
            if holder is not None:
                flags.append({"id": flag["id"], "x": holder["x"], "y": holder["y"],
                              "carrier": carrier})
            else:
                flags.append({"id": flag["id"], "x": flag["x"], "y": flag["y"], "carrier": None})
        return {
            "type": "frame",
            "tick": tick,
            "scores": list(self.scores),
            "units": [{"id": u["id"], "team": u["team"], "x": u["x"], "y": u["y"], "hp": u["hp"],
                       "alive": u["alive"], "respawn_timer": u["respawn_timer"],
                       "carrying_flag": u["carrying_flag"],
                       "attacked_this_turn": u["attacked_this_turn"]}
                      for u in units],
            "flags": flags,
            "bombs": [{"id": b["id"], "x": b["x"], "y": b["y"], "team": b["team"],
                       "timer": b["timer"], "radius": b["radius"]}
                      for b in sorted(self.bombs.values(), key=lambda b: b["id"])],
            "events": events,
        }

    # ---------- 一 tick 的编排 ----------
    def tick(self, tick_no: int) -> dict[str, Any]:
        events: list[dict[str, Any]] = []
        for unit in self.units.values():
            unit["attacked_this_turn"] = False
        # 行动顺序每回合轮换（与规则一致：避免固定先手优势）
        order = [u for _, u in sorted(self.units.items(),
                                      key=lambda kv: (kv[1]["team"] - tick_no) % self.teams * 100 + kv[0])]
        plans = {u["id"]: self.plan(u) for u in order if u["alive"]}

        # 1) 移动：分两轮结算，每轮收集所有意图 → 检测撞格 → 执行
        for round_index in range(2):
            intents: dict[int, tuple[str, tuple[int, int]]] = {}
            for unit in order:
                if not unit["alive"]:
                    continue
                moves = plans.get(unit["id"], {}).get("moves", [])
                if round_index < len(moves):
                    intents[unit["id"]] = moves[round_index]
            # 撞格检测：同一目标格被 ≥2 个单位请求 → 全部留在原地（都消耗 AP）
            target_count: dict[tuple[int, int], list[int]] = {}
            for uid, (_, nxt) in intents.items():
                target_count.setdefault(nxt, []).append(uid)
            for cell, uids in target_count.items():
                if len(uids) >= 2:
                    events.append({"type": "move_conflict", "x": cell[0], "y": cell[1],
                                   "units": sorted(uids)})
            taken = self.unit_cells()
            for uid, (name, nxt) in sorted(intents.items()):
                unit = self.units[uid]
                if len(target_count[nxt]) >= 2:
                    continue  # 冲突：留在原地
                if nxt in taken and nxt not in [(u["x"], u["y"]) for u in self.units.values()
                                                 if u["id"] == uid and u["alive"]]:
                    events.append({"type": "illegal_action", "unit": uid,
                                   "action": f"move({name})", "reason": "目标格被占用"})
                    continue
                events.append({"type": "unit_moved", "unit": uid, "from_x": unit["x"],
                               "from_y": unit["y"], "to_x": nxt[0], "to_y": nxt[1]})
                unit["x"], unit["y"] = nxt
                taken = self.unit_cells()

        # 2) 攻击（按轮换后的行动顺序依次结算）
        for unit in order:
            uid = plans.get(unit["id"], {}).get("attack")
            if uid is None or not unit["alive"]:
                continue
            target = self.units.get(uid)
            if target is None or not target["alive"]:
                events.append({"type": "illegal_action", "unit": unit["id"], "action": f"attack({uid})",
                               "reason": "目标已死亡"})
                continue
            unit["attacked_this_turn"] = True
            target["hp"] = max(0, target["hp"] - 1)
            events.append({"type": "unit_attacked", "attacker": unit["id"], "target": uid,
                           "damage": 1})
            if target["hp"] == 0:
                self.kill(target, by=unit["id"], events=events)

        # 3) 放炸弹
        for unit in order:
            if plans.get(unit["id"], {}).get("bomb") and unit["alive"]:
                bid = self.next_bomb_id
                self.next_bomb_id += 1
                self.bombs[bid] = {"id": bid, "x": unit["x"], "y": unit["y"], "team": unit["team"],
                                   "timer": 2, "radius": 2}
                events.append({"type": "bomb_placed", "unit": unit["id"], "bomb": bid,
                               "x": unit["x"], "y": unit["y"], "timer": 2})

        # 4) 拾旗
        for unit in order:
            if plans.get(unit["id"], {}).get("pick") and unit["alive"]:
                for flag in self.flags.values():
                    if flag["carrier"] is None and (flag["x"], flag["y"]) == (unit["x"], unit["y"]):
                        flag["carrier"] = unit["id"]
                        unit["carrying_flag"] = flag["id"]
                        events.append({"type": "flag_picked", "unit": unit["id"], "flag": flag["id"]})
                        break

        # 5) 炸弹倒计时与爆炸（规则：阵营格免疫、墙阻挡传播、友伤生效）
        for bid in sorted(list(self.bombs)):
            bomb = self.bombs[bid]
            bomb["timer"] -= 1
            if bomb["timer"] > 0:
                continue
            hit: set[int] = set()
            cells = [(bomb["x"], bomb["y"])]
            for dx, dy in ((0, -1), (0, 1), (-1, 0), (1, 0)):
                for step in range(1, bomb["radius"] + 1):
                    cx, cy = bomb["x"] + dx * step, bomb["y"] + dy * step
                    if not (0 <= cx < WIDTH and 0 <= cy < HEIGHT):
                        break
                    if self.terrain[cy * WIDTH + cx] == TERRAIN_WALL:
                        break  # 墙后的格子不受影响
                    cells.append((cx, cy))
            for unit in self.units.values():
                if unit["alive"] and (unit["x"], unit["y"]) in cells \
                        and in_base(self.teams, unit["x"], unit["y"]) is None:
                    hit.add(unit["id"])
            events.append({"type": "bomb_exploded", "bomb": bid, "x": bomb["x"], "y": bomb["y"],
                           "radius": bomb["radius"], "hit_units": sorted(hit)})
            del self.bombs[bid]
            for uid in sorted(hit):
                unit = self.units[uid]
                unit["hp"] = max(0, unit["hp"] - 2)
                if unit["hp"] == 0:
                    self.kill(unit, by=bid, events=events)

        # 6) 复活倒计时
        for unit in sorted(self.units.values(), key=lambda u: u["id"]):
            if unit["alive"] or unit["respawn_timer"] == 0:
                continue
            unit["respawn_timer"] -= 1
            if unit["respawn_timer"] == 0:
                spot = self.free_base_cell(unit["team"])
                if spot is None:
                    unit["respawn_timer"] = 1  # 阵营满员：下回合再试
                else:
                    unit["x"], unit["y"] = spot
                    unit["hp"] = 3
                    unit["alive"] = True
                    unit["carrying_flag"] = None
                    events.append({"type": "unit_respawned", "unit": unit["id"],
                                   "team": unit["team"], "x": spot[0], "y": spot[1]})

        # 7) 旗刷新（每 FLAG_SPAWN_INTERVAL tick 检查一次）
        ground = [f for f in self.flags.values() if f["carrier"] is None]
        if tick_no % FLAG_SPAWN_INTERVAL == 0 and len(ground) < self.teams:
            spot = self.free_center_cell()
            if spot is not None:
                fid = self.next_flag_id
                self.next_flag_id += 1
                self.flags[fid] = {"id": fid, "x": spot[0], "y": spot[1], "carrier": None}
                events.append({"type": "flag_spawned", "flag": fid, "x": spot[0], "y": spot[1]})

        # 8) 得分：携带者进入己方阵营
        for unit in sorted(self.units.values(), key=lambda u: u["id"]):
            if not unit["alive"] or unit["carrying_flag"] is None:
                continue
            if in_base(self.teams, unit["x"], unit["y"]) == unit["team"]:
                fid = unit["carrying_flag"]
                unit["carrying_flag"] = None
                self.flags.pop(fid, None)
                self.scores[unit["team"]] += 1
                events.append({"type": "score", "team": unit["team"], "unit": unit["id"],
                               "flag": fid, "new_score": self.scores[unit["team"]]})

        return self.snapshot(tick_no, events)

    # ---------- 小工具 ----------
    def kill(self, unit: dict[str, Any], by: int, events: list[dict[str, Any]]) -> None:
        unit["alive"] = False
        unit["hp"] = 0
        unit["respawn_timer"] = 10
        self.deaths[unit["team"]] += 1
        if by in self.units:
            self.kills[self.units[by]["team"]] += 1
        events.append({"type": "unit_died", "unit": unit["id"], "team": unit["team"], "by": by})
        if unit["carrying_flag"] is not None:
            fid = unit["carrying_flag"]
            unit["carrying_flag"] = None
            spot = self.free_drop_cell(unit["x"], unit["y"])
            if spot is not None:
                self.flags[fid]["x"], self.flags[fid]["y"] = spot
                self.flags[fid]["carrier"] = None
                events.append({"type": "flag_dropped", "flag": fid, "x": spot[0], "y": spot[1]})
            else:
                self.flags.pop(fid, None)  # 极端兜底：本 tick 无合法落点，旗暂时退场

    def free_base_cell(self, team: int) -> tuple[int, int] | None:
        bx, by = base_top_left(self.teams, team)
        taken = self.unit_cells()
        for dy in range(3):
            for dx in range(3):
                cell = (bx + dx, by + dy)
                if cell not in taken:
                    return cell
        return None

    def free_center_cell(self) -> tuple[int, int] | None:
        occupied = self.unit_cells() | self.flag_cells()
        for dist in range(CENTER_RADIUS + 1):
            for dx in range(-dist, dist + 1):
                for dy in range(-dist, dist + 1):
                    if abs(dx) + abs(dy) != dist:
                        continue
                    x, y = CENTER[0] + dx, CENTER[1] + dy
                    if not is_walkable(self.terrain, x, y) or in_base(self.teams, x, y) is not None:
                        continue
                    if (x, y) not in occupied:
                        return (x, y)
        return None

    def free_drop_cell(self, cx: int, cy: int) -> tuple[int, int] | None:
        occupied = self.unit_cells() | self.flag_cells()
        for size in (3, 5):
            span = size // 2
            for dy in range(-span, span + 1):
                for dx in range(-span, span + 1):
                    x, y = cx + dx, cy + dy
                    if not is_walkable(self.terrain, x, y) or in_base(self.teams, x, y) is not None:
                        continue
                    if (x, y) not in occupied:
                        return (x, y)
        return None


def build_init(demo: Demo, teams: int, ai_names: list[str]) -> dict[str, Any]:
    # 版本号三件套必须与真实引擎一致，否则样例会在「版本不匹配」上给出误导信号：
    # engine 1 / map_gen 1 没变；rules 2 = 虚空改为「可进入但立即死亡」（JSON 结构未变，
    # 见 docs/replay-format.md 的版本历史）。样例本身仍不会主动走进虚空——那属于规则演示，
    # 由 crates/sim/src/tests.rs 的回归测试负责。
    return {
        "type": "init",
        "engine_version": ENGINE_VERSION,
        "rules_version": RULES_VERSION,
        "map_gen_version": 1,
        "map": {"width": WIDTH, "height": HEIGHT, "map_gen_version": 1, "terrain": demo.terrain},
        "teams": [{"team_id": t, "ai_name": ai_names[t],
                   "base_x": base_top_left(teams, t)[0], "base_y": base_top_left(teams, t)[1]}
                  for t in range(teams)],
        "max_ticks": MAX_TICKS,
        "flag_spawn_interval": FLAG_SPAWN_INTERVAL,
        "center_radius": CENTER_RADIUS,
        "seed": DEMO_SEED,
    }


def build_end(demo: Demo, teams: int, ai_names: list[str]) -> dict[str, Any]:
    best = max(demo.scores)
    winners = [t for t, s in enumerate(demo.scores) if s == best]
    return {
        "type": "end",
        "match_index": 0,
        "seed": DEMO_SEED,
        "map_gen_version": 1,
        "ticks": MAX_TICKS,
        "scores": demo.scores,
        "kills": demo.kills,
        "deaths": demo.deaths,
        "winner": winners[0] if len(winners) == 1 else None,
        "ai_names": ai_names,
    }


def make_demo(teams: int) -> list[dict[str, Any]]:
    ai_names = ["demo_alpha", "demo_beta", "demo_gamma"][:teams]
    demo = Demo(teams, ai_names)
    lines = [build_init(demo, teams, ai_names)]
    for tick_no in range(1, MAX_TICKS + 1):
        lines.append(demo.tick(tick_no))
    lines.append(build_end(demo, teams, ai_names))
    return lines


def dump(lines: list[dict[str, Any]], path: str) -> None:
    """写出 JSONL：一行一个对象。用紧凑分隔符（回放文件体积敏感），保持字段顺序可读。"""
    with open(path, "w", encoding="utf-8") as handle:
        for obj in lines:
            handle.write(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))
            handle.write("\n")
    print(f"写出 {path}（{len(lines)} 行）")


def make_malformed(lines: list[dict[str, Any]], path: str) -> None:
    """坏文件：UI 必须能报错但不崩溃（docs/replay-format.md 的容错策略）。"""
    init = copy.deepcopy(lines[0])
    frames = copy.deepcopy(lines[1:6])
    end = copy.deepcopy(lines[-1])
    # 1) 合法 init
    # 2) 非法 JSON（半截行）
    # 3) 缺 type 字段
    # 4) 未知 type
    # 5) 缺字段的 frame（缺 units/events）
    # 6) 一个正常 frame
    # 7) 空行
    # 8) 合法 end
    broken_frame = {k: v for k, v in frames[1].items() if k not in ("units", "events")}
    raw_lines = [json.dumps(init, ensure_ascii=False, separators=(",", ":"))]
    raw_lines.append('{"type":"frame","tick":1,"scores":[0,0]')            # 半截 JSON
    raw_lines.append(json.dumps({"tick": 2, "scores": [0, 0]}, separators=(",", ":")))  # 缺 type
    raw_lines.append(json.dumps({"type": "weather_report", "tick": 3}, separators=(",", ":")))
    raw_lines.append(json.dumps(broken_frame, ensure_ascii=False, separators=(",", ":")))
    raw_lines.append(json.dumps(frames[2], ensure_ascii=False, separators=(",", ":")))
    raw_lines.append("")
    raw_lines.append(json.dumps(end, ensure_ascii=False, separators=(",", ":")))
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(raw_lines) + "\n")
    print(f"写出 {path}（{len(raw_lines)} 行，含 4 处故意缺陷）")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="生成供 Web UI 开发用的样例回放")
    parser.add_argument("--out", default="samples", help="输出目录（默认 samples）")
    args = parser.parse_args(argv)
    os.makedirs(args.out, exist_ok=True)

    two_player = make_demo(2)
    dump(two_player, os.path.join(args.out, "demo_2p.jsonl"))
    three_player = make_demo(3)
    dump(three_player, os.path.join(args.out, "demo_3p.jsonl"))

    mismatch = copy.deepcopy(two_player)
    mismatch[0]["engine_version"] = 99
    mismatch[0]["map"]["map_gen_version"] = 7
    dump(mismatch, os.path.join(args.out, "version_mismatch.jsonl"))

    make_malformed(two_player, os.path.join(args.out, "malformed.jsonl"))

    print("\n提示：用 tools/validate_replay.py 校验前三份（malformed 预期报错）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(__import__("sys").argv[1:]))
