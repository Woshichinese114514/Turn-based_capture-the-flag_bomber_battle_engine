//! tick 内的移动结算：意图收集 → 撞人冲突 → 地形检查 → 占用不动点 → 执行。
//!
//! 实现 `docs/rules.md` §7 的八条步骤。这个模块是整个引擎里最容易写错的地方，
//! 因此把「为什么这么判」逐条写在代码旁边。
//!
//! ## 关键语义（务必与规则原文对照）
//!
//! * **收集阶段**：每个单位每 tick 至多一条 `Move` 意图（§7.1）。重复提交的移动
//!   会被拒绝并记 `illegal_action`，因为本引擎的移动是「全队同时移动」语义，
//!   一个单位在同一 tick 里移动两次没有对应规则。
//! * **撞人 vs 撞墙**：撞人（多个单位抢同一格）**消耗 1 AP** 且留原地，
//!   属于合法但失败的尝试，记 `move_conflict`；撞墙/虚空/敌方阵营/被占位是
//!   AI 的规划错误，**不消耗 AP**，记 `illegal_action`（§7.3/§7.4/§7.7）。
//! * **不动点**：判断「目标格被占」时，先假设所有通过地形检查的意图都会成功，
//!   再反复扫描，把「目标格被一个不会离开的单位占着」的意图标记为失败，
//!   直到某一轮没有变化（§7.5）。这样 A→B,B→A 双方成功，链式阻挡全部失败。
//! * **执行顺序**：先算完所有判定再统一改坐标（§7.6）。若边算边移动，
//!   结果会依赖意图的遍历顺序，破坏可复现性。

use mapgen::MapData;
use protocol::{Coord, EntityId, GameEvent, TeamId, Terrain};

use crate::ai::{Action, UnitCommand};
use crate::state::GameState;

/// 一条被接受的移动意图。
struct MoveIntent {
    unit: EntityId,
    /// 出发格（收集时的位置；本阶段结束前不会被修改）。
    from: Coord,
    to: Coord,
    /// 原始指令标签（如 `move(up)`），写进 `illegal_action.action`。
    label: String,
    /// 被地形/占用拒绝的原因；`Some` 表示该意图失败且**不消耗 AP**。
    reject: Option<String>,
}

/// 结算本 tick 的全部移动意图。
///
/// 不需要 `RulesConfig`：移动固定消耗 1 AP、每回合 1 次，规则里没有可配置字段
/// （对比攻击射程、炸弹引信等都来自规则参数）。少了这个参数，调用方就不必猜
/// 「移动是否受规则配置影响」。
pub(crate) fn resolve_moves(
    state: &mut GameState,
    map: &MapData,
    commands: &[UnitCommand],
) {
    let mut intents = collect_intents(state, commands);
    if intents.is_empty() {
        return;
    }
    let mut ok = vec![true; intents.len()];
    let mut conflict = vec![false; intents.len()];

    // ---- 第 2/3 步：同一目标格被 ≥2 个单位请求 → 全部留原地、都消耗 1 AP ----
    // 分组时保持「目标首次出现」的顺序，保证同种子下事件顺序一致。
    let mut groups: Vec<(Coord, Vec<usize>)> = Vec::new();
    for (index, intent) in intents.iter().enumerate() {
        match groups.iter_mut().find(|(pos, _)| *pos == intent.to) {
            Some((_, list)) => list.push(index),
            None => groups.push((intent.to, vec![index])),
        }
    }
    for (pos, list) in &groups {
        if list.len() < 2 {
            continue;
        }
        for &index in list {
            ok[index] = false;
            conflict[index] = true;
        }
        state.push_event(GameEvent::MoveConflict {
            x: pos.x,
            y: pos.y,
            units: list.iter().map(|&index| intents[index].unit).collect(),
        });
    }

    // ---- 第 4 步：单请求者的地形检查（失败不消耗 AP） ----
    for index in 0..intents.len() {
        if !ok[index] {
            continue;
        }
        let unit_id = intents[index].unit;
        let Some(team) = state.unit(unit_id).map(|unit| unit.team) else {
            continue;
        };
        if let Some(reason) = terrain_rejection(map, team, intents[index].to) {
            ok[index] = false;
            intents[index].reject = Some(reason);
        }
    }

    // ---- 第 5 步：占用不动点迭代 ----
    loop {
        let mut changed = false;
        for index in 0..intents.len() {
            if !ok[index] {
                continue;
            }
            let Some(occupant) = state.alive_at(intents[index].to) else {
                continue;
            };
            let occupant_id = state.units[occupant].id;
            if occupant_id == intents[index].unit {
                continue;
            }
            // 占用者本 tick 是否会成功离开？只有「它的意图仍然成立、且起点正是本意图的目标格」才算。
            let leaves = intents.iter().enumerate().any(|(other, candidate)| {
                other != index
                    && ok[other]
                    && candidate.unit == occupant_id
                    && candidate.from == intents[index].to
            });
            if !leaves {
                ok[index] = false;
                intents[index].reject = Some("目标格被其他单位占用".to_string());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // ---- 第 6/7 步：执行 ----
    for index in 0..intents.len() {
        let unit_id = intents[index].unit;
        if ok[index] {
            if let Some(unit) = state.unit_mut(unit_id) {
                unit.pos = intents[index].to;
            }
            // 移动成功消耗 1 AP（规则 §2）。收集阶段只检查了「AP ≥ 1」，
            // 真正的扣费在这里发生，且必须在改坐标之后 —— `spend_ap` 会再次
            // 校验单位可用性，顺序反过来也不会出错，但先落地再扣费更好读。
            state.spend_ap(unit_id, 1);
            state.push_event(GameEvent::UnitMoved {
                unit: unit_id,
                from_x: intents[index].from.x,
                from_y: intents[index].from.y,
                to_x: intents[index].to.x,
                to_y: intents[index].to.y,
            });
        } else if conflict[index] {
            // 撞人不是非法动作：消耗 1 AP，留在原地，已有 move_conflict 事件说明原因。
            state.spend_ap(unit_id, 1);
        } else {
            let reason = intents[index]
                .reject
                .clone()
                .unwrap_or_else(|| "目标格不可进入".to_string());
            state.push_event(GameEvent::IllegalAction {
                unit: unit_id,
                action: intents[index].label.clone(),
                reason,
            });
        }
    }
}

/// 收集阶段：把 `Move` 指令变成意图，顺带把明显不合法的指令记成 `illegal_action`。
fn collect_intents(state: &mut GameState, commands: &[UnitCommand]) -> Vec<MoveIntent> {
    let mut intents: Vec<MoveIntent> = Vec::new();
    for command in commands {
        let dir = match &command.action {
            Action::Move(dir) => *dir,
            _ => continue,
        };
        let label = command.action.label();
        if let Some(reason) = move_rejection(state, command.unit, &intents) {
            state.push_event(GameEvent::IllegalAction {
                unit: command.unit,
                action: label,
                reason,
            });
            continue;
        }
        let Some(from) = state.unit(command.unit).map(|unit| unit.pos) else {
            continue;
        };
        intents.push(MoveIntent {
            unit: command.unit,
            from,
            to: from.step(dir),
            label,
            reject: None,
        });
    }
    intents
}

/// 移动意图的「行动能力」检查（与地形/占用无关，故在收集阶段做）。
fn move_rejection(state: &GameState, unit_id: EntityId, intents: &[MoveIntent]) -> Option<String> {
    let Some(unit) = state.unit(unit_id) else {
        return Some("单位不存在".to_string());
    };
    if !unit.alive {
        return Some("单位已死亡，不能移动".to_string());
    }
    if unit.ap_left == 0 {
        return Some("AP 不足".to_string());
    }
    if intents.iter().any(|intent| intent.unit == unit_id) {
        return Some("同一 tick 每个单位只结算一次移动".to_string());
    }
    None
}

/// 地形层面能否进入：越界 / 墙 / 虚空 / 敌方阵营。
///
/// 注意这里**不包含**「被单位占用」——那属于不动点迭代（§7.5），
/// 因为占用者的去留取决于其他意图。撞地形不消耗 AP 是明确的设计选择：
/// 这只可能来自 AI 规划错误，不该惩罚它的 AP。
fn terrain_rejection(map: &MapData, team: TeamId, to: Coord) -> Option<String> {
    if !map.in_bounds(to.x, to.y) {
        return Some("目标格越界".to_string());
    }
    match map.terrain_at(to.x, to.y) {
        Some(Terrain::Wall) => return Some("目标是墙".to_string()),
        Some(Terrain::Void) => return Some("目标是虚空".to_string()),
        Some(terrain) if !terrain.is_walkable() => {
            return Some("目标地形不可通行".to_string());
        }
        None => return Some("目标格越界".to_string()),
        _ => {}
    }
    match map.in_any_base(to.x, to.y) {
        Some(owner) if owner != team => Some("目标是敌方阵营".to_string()),
        _ => None,
    }
}
