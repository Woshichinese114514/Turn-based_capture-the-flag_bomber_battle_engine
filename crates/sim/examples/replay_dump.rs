//! 把一整局回放以 JSONL 打到 stdout，用于人工检查与 `tools/validate_replay.py` 校验。
//!
//! ```text
//! ./scripts/cargo run -q -p sim --example replay_dump > target/replay.jsonl
//! python3 tools/validate_replay.py target/replay.jsonl
//! ```
//!
//! 这个示例同时也是 `cli` / `ai` 接入 `sim` 的最小参考：
//! 构造 `MatchConfig` → 造 AI 实例 → `Sim::new` → `run_to_end_with_replay` → `to_jsonl`。
//!
//! 示例 AI 的优先序：能打就打 → 脚边有旗就捡 → 身边有敌人就放炸弹 → 否则用 BFS 往目标走。
//! 因为它会真正贴上去交火、抢旗、回阵营，跑出来的回放覆盖攻击、死亡、复活、炸弹、拾旗、得分
//! 等各类事件，正好用来验证协议事件表与 `tools/validate_replay.py`。

use std::collections::VecDeque;

use protocol::{Coord, Direction};
use sim::ai::{MapView, Observation, TeamActions, TeamAi};
use sim::{MatchConfig, Sim};

/// 贪心 + BFS 寻路 AI：不使用任何随机数，行为只依赖快照（满足「同种子必可复现」）。
struct Greedy {
    name: String,
}

impl Greedy {
    /// 朝目标走一步（无地形绕行）：优先拉平 x 差距，其次 y。
    fn step_towards(from: Coord, to: Coord) -> Option<Direction> {
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        if dx.abs() >= dy.abs() && dx != 0 {
            return Some(if dx > 0 {
                Direction::Right
            } else {
                Direction::Left
            });
        }
        if dy != 0 {
            return Some(if dy > 0 {
                Direction::Down
            } else {
                Direction::Up
            });
        }
        None
    }

    /// 与引擎 `combat::line_clear` 同款 Bresenham：只把墙算作遮挡，单位不挡视线。
    ///
    /// AI 只能通过 `MapView` 看世界（`blocks_sight`），所以自己判断视线，
    /// 避免提交注定失败的 `attack`（那会产出 `illegal_action` 噪音）。
    fn line_clear(map: &MapView, from: Coord, to: Coord) -> bool {
        let (mut x, mut y) = (from.x, from.y);
        let dx = (to.x - x).abs();
        let dy = -(to.y - y).abs();
        let sx = if x < to.x { 1 } else { -1 };
        let sy = if y < to.y { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            let endpoint = (x == from.x && y == from.y) || (x == to.x && y == to.y);
            if !endpoint && map.blocks_sight(x, y) {
                return false;
            }
            if x == to.x && y == to.y {
                return true;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// 用 BFS 在可通行地形上找 `start -> goal` 的第一步。
    ///
    /// AI 只能通过 `Observation`/`MapView` 看世界，所以这里自己写 BFS：
    /// 不穿墙/虚空，也不走进敌方阵营格（那本来就是非法移动）。
    /// 目标不可达（例如目标站在敌方阵营里）时返回 `None`，调用方退化成朝目标直走。
    fn bfs_step(map: &MapView, team: u8, start: Coord, goal: Coord) -> Option<Direction> {
        if start == goal {
            return None;
        }
        let width = map.width as i32;
        let height = map.height as i32;
        let index = |c: Coord| (c.y * width + c.x) as usize;
        let total = (width * height) as usize;
        let mut seen = vec![false; total];
        let mut prev: Vec<Option<Coord>> = vec![None; total];
        let mut queue = VecDeque::new();
        seen[index(start)] = true;
        queue.push_back(start);

        let mut reached = false;
        while let Some(current) = queue.pop_front() {
            if current == goal {
                reached = true;
                break;
            }
            for dir in Direction::ALL {
                let next = current.step(dir);
                if !map.in_bounds(next.x, next.y) {
                    continue;
                }
                let slot = index(next);
                if seen[slot] || !map.is_walkable(next.x, next.y) {
                    continue;
                }
                match map.in_any_base(next.x, next.y) {
                    Some(owner) if owner != team => continue,
                    _ => {}
                }
                seen[slot] = true;
                prev[slot] = Some(current);
                queue.push_back(next);
            }
        }
        if !reached {
            return None;
        }

        // 从 goal 沿 prev 回溯到 start，取路径上第一步的方向。
        let mut step = goal;
        while let Some(previous) = prev[index(step)] {
            if previous == start {
                let delta = (step.x - start.x, step.y - start.y);
                return Direction::ALL
                    .iter()
                    .copied()
                    .find(|dir| dir.delta() == delta);
            }
            step = previous;
        }
        None
    }
}

impl TeamAi for Greedy {
    fn name(&self) -> &str {
        &self.name
    }

    fn decide(&mut self, obs: &Observation) -> TeamActions {
        let mut actions = TeamActions::new();
        // 敌人按 id 排序：同 tick 内选择稳定 → 回放可复现。
        let mut enemies: Vec<(u32, Coord)> =
            obs.enemies().map(|enemy| (enemy.id, enemy.pos)).collect();
        enemies.sort_by_key(|(id, _)| *id);

        for unit_id in &obs.my_units {
            let Some(unit) = obs.me(*unit_id) else {
                continue;
            };
            if !unit.alive {
                continue;
            }
            let carrying = unit.carrying_flag.is_some();
            let nearest_enemy = enemies
                .iter()
                .min_by_key(|(_, pos)| pos.manhattan(unit.pos));

            // 1) 脚边有地上的旗 → 拾旗。
            let flag_here = obs
                .flags
                .iter()
                .any(|flag| flag.carrier.is_none() && flag.pos == unit.pos);
            if !carrying && flag_here {
                actions.pick_flag(*unit_id);
                continue;
            }

            // 2) 射程 3 内且有视线的敌人 → 攻击（携旗时规则禁止攻击，跳过）。
            if !carrying {
                if let Some((target, target_pos)) = nearest_enemy {
                    if unit.can_attack()
                        && unit.pos.manhattan(*target_pos) <= 3
                        && Self::line_clear(&obs.map, unit.pos, *target_pos)
                    {
                        actions.attack(*unit_id, *target);
                        continue;
                    }
                }
            }

            // 3) 身边 2 格内有敌人且不在阵营格 → 放炸弹（阵营内禁止放置）。
            let adjacent = enemies
                .iter()
                .any(|(_, pos)| pos.manhattan(unit.pos) <= 2);
            if unit.ap_left >= 2 && adjacent && obs.map.in_any_base(unit.pos.x, unit.pos.y).is_none()
            {
                actions.place_bomb(*unit_id);
                continue;
            }

            // 4) 选目标：携旗回自家阵营中心；否则去最近的落地旗；再否则扑最近的敌人。
            let own_base = obs
                .map
                .bases
                .get(obs.team as usize)
                .map(|base| Coord::new(base.x + 1, base.y + 1));
            let ground_flag = obs.enemy_flags_on_ground().next().map(|flag| flag.pos);
            let any_flag = obs
                .flags
                .iter()
                .filter(|flag| flag.carrier.is_none())
                .min_by_key(|flag| flag.pos.manhattan(unit.pos))
                .map(|flag| flag.pos);
            let goal = if carrying {
                own_base
            } else {
                any_flag
                    .or(ground_flag)
                    .or_else(|| nearest_enemy.map(|(_, pos)| *pos))
            };

            // 5) 选一步可走的移动：优先 BFS/直走方向，被占则侧移绕开，全被占就等（Wait 不耗 AP）。
            //
            // 「目标格被其他单位占用」是引擎会拒的动作；AI 先自己看 `obs.units` 排除掉，
            // 免得每 tick 刷一堆 illegal_action。这正是 `Observation` 存在的意义。
            let occupied = |pos: Coord| obs.units.iter().any(|other| other.alive && other.pos == pos);
            let passable = |pos: Coord| {
                obs.map.in_bounds(pos.x, pos.y)
                    && obs.map.is_walkable(pos.x, pos.y)
                    && !occupied(pos)
                    && match obs.map.in_any_base(pos.x, pos.y) {
                        Some(owner) => owner == obs.team,
                        None => true,
                    }
            };

            let mut candidates: Vec<Direction> = Vec::new();
            if let Some(goal) = goal {
                if let Some(primary) =
                    Self::bfs_step(&obs.map, obs.team, unit.pos, goal)
                        .or_else(|| Self::step_towards(unit.pos, goal))
                {
                    candidates.push(primary);
                    // 侧移候选：与首选垂直的两个方向（点积为 0），按 Direction::ALL 顺序，保持确定性。
                    let (px, py) = primary.delta();
                    for dir in Direction::ALL {
                        let (dx, dy) = dir.delta();
                        if dx * px + dy * py == 0 {
                            candidates.push(dir);
                        }
                    }
                }
            }
            let chosen = candidates
                .into_iter()
                .find(|dir| passable(unit.pos.step(*dir)));
            match chosen {
                Some(dir) => actions.move_unit(*unit_id, dir),
                // 无路可走（被同伴堵住）时等待：0 AP，不会产生非法动作。
                None => actions.wait(*unit_id),
            };
        }
        actions
    }
}

/// 读一个可选环境变量（用于快速换参数：队伍数/种子/尺寸/时长），解析失败就用默认值。
///
/// 例：`SIM_EXAMPLE_TEAMS=3 SIM_EXAMPLE_SEED=7 ./scripts/cargo run -q -p sim --example replay_dump`，
/// 这样不用改代码就能验证 3 队回放（地形码 3+team、阵营归属、数组长度校验都走不同分支）。
fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let teams = env_u64("SIM_EXAMPLE_TEAMS", 2) as u8;
    let seed = env_u64("SIM_EXAMPLE_SEED", 20240607);
    let size = env_u64("SIM_EXAMPLE_SIZE", 15) as u16;
    let max_ticks = env_u64("SIM_EXAMPLE_TICKS", 120) as u32;

    let mut config = MatchConfig::new(teams, seed);
    config.width = size;
    config.height = size;
    config.max_ticks = max_ticks;

    let ais: Vec<Box<dyn TeamAi>> = (0..teams)
        .map(|team| {
            Box::new(Greedy {
                name: format!("greedy-{team}"),
            }) as Box<dyn TeamAi>
        })
        .collect();

    let mut sim = Sim::new(config, ais).expect("示例配置必须合法");
    let bundle = sim.run_to_end_with_replay();
    match bundle.to_jsonl() {
        Ok(text) => print!("{text}"),
        Err(error) => eprintln!("回放序列化失败：{error}"),
    }
}
