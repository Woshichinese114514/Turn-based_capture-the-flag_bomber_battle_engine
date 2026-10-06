//! 攻击结算：射程 + 视线的合法性判定、伤害与死亡（`docs/rules.md` §3）。
//!
//! ## 判定要点
//!
//! * 攻击成立需要**全部**满足：攻击者存活、本 tick 未攻击过、AP ≥ 1、目标存活、
//!   目标是敌人、曼哈顿距离 ≤ `attack_range`、两点之间无墙（§3）。
//! * 每 tick 每单位最多攻击 1 次：成功后置 `attacked_this_turn`，
//!   同 tick 内第二条 `Attack` 指令会因「本回合已攻击过」被拒（§14 测试 6）。
//! * 攻击**不消耗整回合**：命中后只要还有 AP，仍可继续移动/放炸弹/拾旗（§2、§3）。
//! * 同 tick 内多个单位攻击同一目标时，按「行动顺序」（AI 轮换顺序，即指令收集顺序）
//!   依次结算；后手若打到已经死亡的尸体，会记 `illegal_action`（§3）。
//!
//! ## 视线为什么用 Bresenham
//!
//! 规则要求「检查起点与终点之间经过的所有格，任一格是墙则无视线」（§3.2）。
//! 这里用标准 Bresenham 直线：它给出**唯一确定**的一串格子，且实现短、
//! 不引入浮点误差。副作用是对角相邻（曼哈顿距离 2）的目标没有中间格，因此不被阻挡；
//! 若改用「超覆盖（supercover）」则对角方向会被两侧的墙挡住，等于让对角攻击几乎失效，
//! 与「射程 3 的贴身对射」意图不符。这个取舍是刻意的。

use mapgen::{MapData, Rng};
use protocol::{Coord, EntityId, GameEvent};

use crate::ai::{Action, UnitCommand};
use crate::state::GameState;
use crate::RulesConfig;

/// 结算本 tick 的全部攻击指令。
pub(crate) fn resolve_attacks(
    state: &mut GameState,
    map: &MapData,
    rules: &RulesConfig,
    rng: &mut Rng,
    commands: &[UnitCommand],
) {
    for command in commands {
        let target = match &command.action {
            Action::Attack(target) => *target,
            _ => continue,
        };
        if let Some(reason) = attack_rejection(state, map, rules, command.unit, target) {
            state.push_event(GameEvent::IllegalAction {
                unit: command.unit,
                action: command.action.label(),
                reason,
            });
            continue;
        }
        // 通过全部校验：扣 AP（必定成功）、标记本回合已攻击、结算伤害。
        state.spend_ap(command.unit, 1);
        if let Some(attacker) = state.unit_mut(command.unit) {
            attacker.attacked_this_turn = true;
        }
        state.push_event(GameEvent::UnitAttacked {
            attacker: command.unit,
            target,
            damage: rules.attack_damage,
        });
        apply_damage(
            state,
            map,
            rules,
            rng,
            target,
            rules.attack_damage,
            Some(command.unit),
        );
    }
}

/// 攻击的合法性判定；返回 `Some(原因)` 表示攻击被拒（不消耗 AP）。
///
/// 单独抽出来的原因：它需要读取攻击者与目标的多个字段，集中在一处能保证
/// 「报出的原因」与「实际的拒绝条件」一一对应，AI 开发者据此就能定位自己的 bug。
fn attack_rejection(
    state: &GameState,
    map: &MapData,
    rules: &RulesConfig,
    attacker: EntityId,
    target: EntityId,
) -> Option<String> {
    let Some(unit) = state.unit(attacker) else {
        return Some("单位不存在".to_string());
    };
    if !unit.alive {
        return Some("单位已死亡，不能攻击".to_string());
    }
    if unit.ap_left == 0 {
        return Some("AP 不足".to_string());
    }
    if unit.attacked_this_turn {
        return Some("本回合已攻击过".to_string());
    }
    if !rules.flag_carrier_can_attack && unit.carrying_flag.is_some() {
        return Some("携带旗时不能攻击".to_string());
    }
    let Some(defender) = state.unit(target) else {
        return Some("目标单位不存在".to_string());
    };
    if !defender.alive {
        return Some("目标已死亡".to_string());
    }
    if defender.team == unit.team {
        return Some("目标是友方单位".to_string());
    }
    let distance = unit.pos.manhattan(defender.pos);
    if distance > rules.attack_range {
        return Some(format!("目标距离 {distance} 超出射程 {}", rules.attack_range));
    }
    if !line_clear(map, unit.pos, defender.pos) {
        return Some("中间有墙，没有视线".to_string());
    }
    None
}

/// 两点之间是否有清晰视线（只看墙；单位不阻挡视线）。
///
/// 起点与终点本身不检查（攻击者/目标站在墙角上互射是允许的，
/// 规则只说「中间格不含墙」）。
pub(crate) fn line_clear(map: &MapData, from: Coord, to: Coord) -> bool {
    let mut x = from.x;
    let mut y = from.y;
    let dx = (to.x - x).abs();
    let dy = -(to.y - y).abs();
    let sx = if x < to.x { 1 } else { -1 };
    let sy = if y < to.y { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        let is_endpoint = (x == from.x && y == from.y) || (x == to.x && y == to.y);
        if !is_endpoint && map.blocks_sight(x, y) {
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

/// 对单位造成伤害，HP 归 0 时走统一的死亡结算（掉旗/击杀计数/事件）。
///
/// 死亡结算集中在 [`GameState::kill_unit`]，因此攻击与炸弹两条路径不会出现行为差异。
pub(crate) fn apply_damage(
    state: &mut GameState,
    map: &MapData,
    rules: &RulesConfig,
    rng: &mut Rng,
    victim: EntityId,
    damage: u8,
    by: Option<EntityId>,
) {
    let Some(unit) = state.unit(victim) else {
        return;
    };
    if !unit.alive {
        return;
    }
    let hp = unit.hp.saturating_sub(damage);
    if let Some(unit) = state.unit_mut(victim) {
        unit.hp = hp;
    }
    if hp == 0 {
        state.kill_unit(victim, by, map, rules, rng);
    }
}
