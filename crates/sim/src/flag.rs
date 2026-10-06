//! 旗：刷新、拾取、掉落与得分（`docs/rules.md` §5、§6）。
//!
//! ## 旗没有归属
//!
//! 规则里旗不属于任何队伍：地上的旗**任何队都能拾取**（包括刚丢旗的原持有队）。
//! 因此这里没有「敌方旗/己方旗」的区分，只有「有没有携带者」。
//!
//! ## 刷新区与落点
//!
//! * 刷新：每 `flag_spawn_interval` 个 tick 检查一次（第 t tick 且 `t % interval == 0`、
//!   `t >= interval`），若场上旗数 < 上限（`max_flags == 0` 时等于队伍数），
//!   在**中心区域**（到中心曼哈顿距离 ≤ `flag_capture_radius`）随机选一个合法空格刷 1 面。
//! * 落点（携带者死亡）：死亡格周围 3×3（不含死亡格本身）→ 5×5（同样不含）→
//!   中心区域随机 → 死亡格本身 → 退场；
//!   全部失败时该旗暂时退场（不写进任何帧），等待下一个刷旗检查点重新出现。
//!
//! 「合法格」的统一定义见 [`is_free_for_flag`]：可通行、不属于任何阵营区、
//! 没有其他旗、没有存活单位、没有炸弹。注意死亡单位的尸体不算占用（它不在地图上）。
//!
//! ## 随机性
//!
//! 所有随机选择都走同一个 `&mut Rng`，并且**先构造候选列表（行优先顺序固定）
//! 再抽下标**，因此同种子必然得到同一落点。

use mapgen::{MapData, Rng};
use protocol::{Coord, EntityId, GameEvent, TeamId};

use crate::ai::{Action, UnitCommand};
use crate::state::{Flag, GameState};
use crate::RulesConfig;

/// 旗刷新检查（规则 §9 第 6 步）。
pub(crate) fn maybe_spawn_flag(
    state: &mut GameState,
    map: &MapData,
    rules: &RulesConfig,
    teams: u8,
    rng: &mut Rng,
) {
    let interval = rules.flag_spawn_interval;
    if interval == 0 {
        return;
    }
    let tick = state.tick;
    // 第 t tick 结算末尾检查，且必须已经过了一个完整间隔。
    if tick < interval || tick % interval != 0 {
        return;
    }
    if state.flags.len() >= rules.max_flags_for(teams) {
        return;
    }
    let candidates = center_cells(state, map, None);
    let Some(index) = rng.pick_index(&candidates) else {
        // 没有合法候选格 → 本 tick 不刷新（规则 §5.2）。
        return;
    };
    let pos = candidates[index];
    let id = state.alloc_flag_id();
    state.flags.push(Flag {
        id,
        pos,
        carrier: None,
    });
    state.push_event(GameEvent::FlagSpawned {
        flag: id,
        x: pos.x,
        y: pos.y,
    });
}

/// 拾旗结算（规则 §9 第 3 步的第四类动作）。
pub(crate) fn resolve_pickups(state: &mut GameState, commands: &[UnitCommand]) {
    for command in commands {
        if !matches!(&command.action, Action::PickFlag) {
            continue;
        }
        match pickup_check(state, command.unit) {
            Ok(flag_id) => {
                state.spend_ap(command.unit, 1);
                if let Some(index) = state.flags.iter().position(|flag| flag.id == flag_id) {
                    state.flags[index].carrier = Some(command.unit);
                }
                if let Some(unit) = state.unit_mut(command.unit) {
                    unit.carrying_flag = Some(flag_id);
                }
                state.push_event(GameEvent::FlagPicked {
                    unit: command.unit,
                    flag: flag_id,
                });
            }
            Err(reason) => state.push_event(GameEvent::IllegalAction {
                unit: command.unit,
                action: command.action.label(),
                reason,
            }),
        }
    }
}

/// 拾旗的合法性判定：返回可拾取的旗 ID，或拒绝原因。
fn pickup_check(state: &GameState, unit_id: EntityId) -> Result<EntityId, String> {
    let Some(unit) = state.unit(unit_id) else {
        return Err("单位不存在".to_string());
    };
    if !unit.alive {
        return Err("单位已死亡，不能拾旗".to_string());
    }
    if unit.ap_left == 0 {
        return Err("AP 不足".to_string());
    }
    if unit.carrying_flag.is_some() {
        // 规则 §5.3：已经拿着一面旗，不能再拾第二面（按非法动作处理）。
        return Err("已携带一面旗，不能再拾取".to_string());
    }
    match state
        .flags
        .iter()
        .find(|flag| flag.pos == unit.pos && flag.carrier.is_none())
    {
        Some(flag) => Ok(flag.id),
        None => Err("当前格没有可拾取的旗".to_string()),
    }
}

/// 得分检查（规则 §9 第 7 步）：携带者站在**己方**阵营区内 → 该队 +1，旗消失。
///
/// 不消耗 AP（规则 §5.4），并且可以在一 tick 内连续触发多次（若该队同时有两面旗入营）。
pub(crate) fn check_scores(state: &mut GameState, map: &MapData) {
    // 先收集（不可在遍历 `units` 的同时修改 `flags`/`scores`）。
    let mut scored: Vec<(EntityId, TeamId, EntityId)> = Vec::new();
    for unit in &state.units {
        if !unit.alive {
            continue;
        }
        let Some(flag_id) = unit.carrying_flag else {
            continue;
        };
        if !map.in_base(unit.team, unit.pos.x, unit.pos.y) {
            continue;
        }
        scored.push((unit.id, unit.team, flag_id));
    }
    for (unit_id, team, flag_id) in scored {
        let Some(index) = state.flags.iter().position(|flag| flag.id == flag_id) else {
            continue;
        };
        state.flags.remove(index);
        if let Some(unit) = state.unit_mut(unit_id) {
            unit.carrying_flag = None;
        }
        let new_score = match state.scores.get_mut(team as usize) {
            Some(slot) => {
                *slot += 1;
                *slot
            }
            None => continue,
        };
        state.push_event(GameEvent::Score {
            team,
            unit: unit_id,
            flag: flag_id,
            new_score,
        });
    }
}

/// 携带者死亡 → 旗掉落（规则 §6）。
///
/// `origin` 是死亡位置；`flag_id` 是掉落的那面旗。若所有候选都失败，
/// 旗**暂时退场**：从 `flags` 中移除、不发 `flag_dropped` 事件，
/// 直到下一个刷旗检查点按正常规则重新出现。
pub(crate) fn drop_carried_flag(
    state: &mut GameState,
    map: &MapData,
    rng: &mut Rng,
    origin: Coord,
    flag_id: EntityId,
) {
    let Some(index) = state.flags.iter().position(|flag| flag.id == flag_id) else {
        return;
    };
    // 先解绑：旗落地后不属于任何人（即使随后退场）。
    state.flags[index].carrier = None;
    match pick_drop_cell(state, map, origin, flag_id, rng) {
        Some(pos) => {
            if let Some(flag) = state.flags.get_mut(index) {
                flag.pos = pos;
            }
            state.push_event(GameEvent::FlagDropped {
                flag: flag_id,
                x: pos.x,
                y: pos.y,
            });
        }
        None => {
            state.flags.remove(index);
        }
    }
}

/// 选掉落格：3×3 → 5×5 → 中心区域 → 死亡格本身 → 失败（`None`）。
///
/// 两个随机圈都**显式排除死亡格本身**：规则 §6 把「放死亡格本身」单列为最后一级兜底
/// （「全失败 → 放死亡格本身」）。若把死亡格混进第一级随机池，后面那一级就永远走不到
/// （死亡格必然属于自己的 3×3），规范的阶梯就失去意义；排除之后每一级都可被真实触发，
/// 语义也与「周围 3×3」的字面（周围 = 邻域，不含中心）一致。
fn pick_drop_cell(
    state: &GameState,
    map: &MapData,
    origin: Coord,
    flag_id: EntityId,
    rng: &mut Rng,
) -> Option<Coord> {
    for radius in [1, 2] {
        let mut cells = Vec::new();
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let pos = Coord::new(origin.x + dx, origin.y + dy);
                if pos == origin {
                    continue;
                }
                if is_free_for_flag(state, map, pos, Some(flag_id)) {
                    cells.push(pos);
                }
            }
        }
        if !cells.is_empty() {
            return rng.pick_index(&cells).map(|index| cells[index]);
        }
    }
    let cells = center_cells(state, map, Some(flag_id));
    if let Some(index) = rng.pick_index(&cells) {
        return Some(cells[index]);
    }
    // 兜底：死亡格本身（尸体不阻挡，因此只需它本身合法且不在阵营区）。
    if is_free_for_flag(state, map, origin, Some(flag_id)) {
        return Some(origin);
    }
    None
}

/// 中心区域内的合法候选格（行优先，顺序固定）。
fn center_cells(state: &GameState, map: &MapData, exclude_flag: Option<EntityId>) -> Vec<Coord> {
    let mut cells = Vec::new();
    for y in 0..map.height as i32 {
        for x in 0..map.width as i32 {
            if !map.in_center_region(x, y) {
                continue;
            }
            let pos = Coord::new(x, y);
            if is_free_for_flag(state, map, pos, exclude_flag) {
                cells.push(pos);
            }
        }
    }
    cells
}

/// 「合法格」的统一判定。
///
/// `exclude_flag` 用于忽略**正在掉落的这面旗**：它此刻就停在死亡格上，
/// 否则掉落候选里会永远排除掉死亡格附近所有位置。
fn is_free_for_flag(
    state: &GameState,
    map: &MapData,
    pos: Coord,
    exclude_flag: Option<EntityId>,
) -> bool {
    if !map.is_walkable(pos.x, pos.y) {
        return false;
    }
    if map.in_any_base(pos.x, pos.y).is_some() {
        return false;
    }
    // 用 `flag_at` 定位该格上的旗；`exclude_flag` 是为了忽略「正在掉落/正在被拾取」的这面旗。
    if let Some(index) = state.flag_at(pos) {
        if Some(state.flags[index].id) != exclude_flag {
            return false;
        }
    }
    if state.alive_at(pos).is_some() {
        return false;
    }
    if state.bomb_at(pos) {
        return false;
    }
    true
}
