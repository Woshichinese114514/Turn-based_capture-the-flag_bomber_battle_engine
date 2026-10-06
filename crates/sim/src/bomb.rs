//! 炸弹：放置、倒计时与十字爆炸（`docs/rules.md` §4）。
//!
//! ## 倒计时语义（与 `BombView.timer` 的文档严格一致）
//!
//! `placed_tick` 记录放置所在的 tick；该 tick 的倒计时阶段**跳过**这颗炸弹。
//! 于是 fuse=2 时：放置 tick 帧里 `timer = 2`，下一 tick 帧里 `timer = 1`，
//! 再下一 tick 爆炸（此时帧里已没有这颗炸弹）。回放中只可能出现 2 和 1 ——
//! 正是协议文档要求的取值集合。
//!
//! ## 爆炸规则
//!
//! * 十字：上下左右各传播 `radius` 格；**遇墙立刻停止该方向**（虚空不阻挡传播，
//!   因为空地上不可能放炸弹、也没人能站进去，但它仍可能出现在方向线上）。
//! * 伤害 `bomb_damage`（标准 2），**有友伤**；站在任意阵营格上的单位**免疫**。
//! * 炸弹不可被攻击摧毁，也不阻挡移动/视线。
//! * 同一格可以堆叠多颗炸弹，各自独立倒计时；同一 tick 多颗引爆时按放置顺序

//!   （即 `bombs` 的下标顺序）依次结算，保证确定性。
//! * `unit_died.by` 记为炸弹的放置者（可能是自己 —— 自爆）。

use mapgen::{MapData, Rng};
use protocol::{Coord, Direction, EntityId, GameEvent};

use crate::ai::{Action, UnitCommand};
use crate::state::{Bomb, GameState};
use crate::RulesConfig;

/// 结算本 tick 的放置指令（规则 §9 第 3 步的第三类动作）。
pub(crate) fn resolve_placements(
    state: &mut GameState,
    map: &MapData,
    rules: &RulesConfig,
    commands: &[UnitCommand],
) {
    for command in commands {
        if !matches!(&command.action, Action::PlaceBomb) {
            continue;
        }
        if let Some(reason) = placement_rejection(state, map, command.unit) {
            state.push_event(GameEvent::IllegalAction {
                unit: command.unit,
                action: command.action.label(),
                reason,
            });
            continue;
        }
        let Some((pos, team)) = state.unit(command.unit).map(|unit| (unit.pos, unit.team)) else {
            continue;
        };
        let id = state.alloc_bomb_id();
        state.bombs.push(Bomb {
            id,
            pos,
            owner: command.unit,
            team,
            timer: rules.bomb_fuse,
            radius: rules.bomb_radius,
            placed_tick: state.tick,
        });
        state.spend_ap(command.unit, 1);
        state.push_event(GameEvent::BombPlaced {
            unit: command.unit,
            bomb: id,
            x: pos.x,
            y: pos.y,
            timer: rules.bomb_fuse,
        });
    }
}

/// 放置的合法性判定；返回 `Some(原因)` 表示被拒（不消耗 AP）。
fn placement_rejection(state: &GameState, map: &MapData, unit_id: EntityId) -> Option<String> {
    let Some(unit) = state.unit(unit_id) else {
        return Some("单位不存在".to_string());
    };
    if !unit.alive {
        return Some("单位已死亡，不能放炸弹".to_string());
    }
    if unit.ap_left == 0 {
        return Some("AP 不足".to_string());
    }
    if map.in_any_base(unit.pos.x, unit.pos.y).is_some() {
        return Some("阵营格内不能放置炸弹".to_string());
    }
    None
}

/// 倒计时阶段：所有既有炸弹减 1，减到 0 的立即引爆（规则 §9 第 4 步）。
pub(crate) fn tick_and_explode(
    state: &mut GameState,
    map: &MapData,
    rules: &RulesConfig,
    rng: &mut Rng,
) {
    let tick = state.tick;
    let mut exploding: Vec<EntityId> = Vec::new();
    for bomb in &mut state.bombs {
        // 放置当回合不递减（见模块文档）。
        if bomb.placed_tick == tick {
            continue;
        }
        if bomb.timer > 0 {
            bomb.timer -= 1;
        }
        if bomb.timer == 0 {
            exploding.push(bomb.id);
        }
    }
    // 用 ID 而不是下标：引爆会从 `bombs` 里删除元素，下标会失效。
    for bomb_id in exploding {
        explode(state, map, rules, rng, bomb_id);
    }
}

/// 引爆一颗炸弹：计算十字范围、结算免疫与伤害、删除炸弹、发事件。
fn explode(
    state: &mut GameState,
    map: &MapData,
    rules: &RulesConfig,
    rng: &mut Rng,
    bomb_id: EntityId,
) {
    let Some(index) = state.bombs.iter().position(|bomb| bomb.id == bomb_id) else {
        return;
    };
    let bomb = state.bombs.remove(index);
    let cells = blast_cells(map, bomb.pos, bomb.radius);

    // 先收集受影响单位（按单位 ID 升序 = `units` 的下标顺序），
    // 阵营格免疫者直接跳过，不算「被命中」。
    let mut hit: Vec<EntityId> = Vec::new();
    for unit in &state.units {
        if !unit.alive || !cells.contains(&unit.pos) {
            continue;
        }
        if map.in_any_base(unit.pos.x, unit.pos.y).is_some() {
            continue;
        }
        hit.push(unit.id);
    }

    state.push_event(GameEvent::BombExploded {
        bomb: bomb.id,
        x: bomb.pos.x,
        y: bomb.pos.y,
        radius: bomb.radius,
        hit_units: hit.clone(),
    });

    // 伤害在事件之后统一结算：这样 `bomb_exploded.hit_units` 里是「被本次爆炸扣血」
    // 的完整名单，而 `unit_died` 事件紧跟在它后面，回放顺序对人类阅读最自然。
    for unit_id in hit {
        crate::combat::apply_damage(
            state,
            map,
            rules,
            rng,
            unit_id,
            rules.bomb_damage,
            Some(bomb.owner),
        );
    }
}

/// 十字爆炸覆盖的格子（含中心格）。
fn blast_cells(map: &MapData, center: Coord, radius: u8) -> Vec<Coord> {
    let mut cells = vec![center];
    for dir in Direction::ALL {
        let (dx, dy) = dir.delta();
        for step in 1..=radius as i32 {
            let pos = Coord::new(center.x + dx * step, center.y + dy * step);
            if !map.in_bounds(pos.x, pos.y) {
                break;
            }
            if map.blocks_sight(pos.x, pos.y) {
                // 墙阻挡传播：该方向后续格子也不受影响。
                break;
            }
            cells.push(pos);
        }
    }
    cells
}
