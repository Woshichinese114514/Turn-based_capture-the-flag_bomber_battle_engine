//! `greedy_flag` 基线 AI：抢旗优先。
//!
//! 规格（docs/rules.md §13）：前往最近的旗 → 拾起 → 沿最短路径回己方阵营；拿旗时不攻击。
//!
//! ## 决策优先级（每个单位、每个 tick，按顺序消费 2 AP）
//!
//! 1. **脚下就是地上旗** → 拾旗（结算顺序里拾旗最后执行，所以拾旗这一 tick 不再规划移动，
//!    否则走出去会让拾旗落空——`UnitPlan` 也会拒绝这种组合）；
//! 2. **移动**：携带旗（或本 tick 已规划拾旗）→ 走向己方阵营任一格（进区即自动得分）；
//!    未携带 → 走向最近的一面地上旗，场上没有可见旗时去地图中心待命（旗在中心区刷新）；
//! 3. **攻击**：还有剩 AP、且自己不是（也不会成为）拿旗者时，打射程内（≤3 且有视线）的敌人。
//!
//! ## 为什么拿旗者不攻击
//!
//! 默认规则 `flag_carrier_can_attack = false`（docs/rules.md §5）：拿旗者攻击会被引擎判非法。
//! 而且抢旗 AI 的收益来自“把旗送回家”，不是换命——真被堵住了，靠队友去解围比丢掉旗子更划算。
//!
//! ## 为什么移动用 BFS 而不是曼哈顿贪心
//!
//! 规则要求“沿最短路径返回阵营”。纯贪心（每步只求曼哈顿距离更近）会被墙挡住来回抖动
//! （`greedy_step_toward` 只接受严格更近的一步，遇到凹形墙就没有更近的一步可走）。
//! 所以主路径用 [`bfs_dist_field`] 的多源 BFS：目标集是“己方阵营全部格子”或“所有可见地上旗”，
//! 天然同时解决“走哪面旗最近”和“怎么绕墙”两个问题；BFS 不可达时（目标被单位堵住）
//! 才退回贪心、再退回随机，避免单位整局傻站着。

use std::collections::HashSet;

use protocol::{Coord, EntityId};
use sim::ai::{Observation, TeamActions, TeamAi};
use sim::Rng;

use crate::common::{
    base_cells, bfs_dist_field, greedy_step_toward, ground_flag_at, occupied_cells,
    random_legal_move, UnitPlan,
};
use crate::AI_GREEDY_FLAG;

pub struct GreedyFlagAi {
    rng: Rng,
}

impl GreedyFlagAi {
    /// 用 `ai_seed` 播种本局的随机源（随机性只用于被堵死时的兜底走位，决策本身是确定性的）。
    pub fn new(ai_seed: u64) -> Self {
        Self {
            rng: Rng::new(ai_seed),
        }
    }

    /// 沿 BFS 距离场朝目标走一步（多目标时自动选最近的那个）；
    /// 距离场不可用（目标被占满 / 不可达）时依次退回贪心、随机合法移动。
    ///
    /// 返回 BFS 选中的目标下标（`Some` 表示「这个单位本 tick 打算去 `goals[i]`」）。
    /// 调用方用它把该目标标记为「已被队友认领」，避免全队挤向同一面旗。
    fn advance_toward(
        &mut self,
        plan: &mut UnitPlan<'_>,
        obs: &Observation,
        goals: &[Coord],
        occupied: &HashSet<Coord>,
        reserved: &HashSet<Coord>,
    ) -> Option<usize> {
        let from = plan.original_pos();
        let field = bfs_dist_field(&obs.map, obs.team, goals, occupied);
        let mut chosen = None;
        match field.nearest_source(from) {
            // 距离 0：已经站在目标格上（例如拿旗者已经进了己方阵营），本 tick 不用再动。
            Some((source, 0)) => chosen = Some(source),
            Some((source, _)) => {
                chosen = Some(source);
                if let Some(dir) = field.step_toward(from, reserved) {
                    if plan.try_move(obs, dir, occupied, reserved) {
                        return chosen;
                    }
                }
            }
            None => {}
        }
        // 兜底一：贪心靠近（把单位从“被堵住的死点”挪开）。
        if let Some(dir) = greedy_step_toward(&obs.map, obs.team, from, goals, occupied, reserved) {
            plan.try_move(obs, dir, occupied, reserved);
            return chosen;
        }
        // 兜底二：随机合法移动。随机走位有可能把堵路的单位熬走，也比原地不动强。
        if let Some(dir) =
            random_legal_move(&mut self.rng, &obs.map, obs.team, from, occupied, reserved)
        {
            plan.try_move(obs, dir, occupied, reserved);
        }
        chosen
    }
}

impl TeamAi for GreedyFlagAi {
    fn name(&self) -> &str {
        AI_GREEDY_FLAG
    }

    fn decide(&mut self, obs: &Observation) -> TeamActions {
        let map = &obs.map;
        let mut actions = TeamActions::new();
        let occupied_all = occupied_cells(obs, None);
        let mut reserved: HashSet<Coord> = HashSet::new();
        // 本 tick 已被队友认领的旗：避免全队挤向同一面旗（互相堵路、白费 AP）。
        let mut claimed: HashSet<EntityId> = HashSet::new();
        let home = base_cells(map, obs.team);

        for &unit_id in &obs.my_units {
            let Some(unit) = obs.me(unit_id) else {
                continue;
            };
            if !unit.alive {
                continue;
            }
            let mut occupied = occupied_all.clone();
            occupied.remove(&unit.pos);
            let mut plan = UnitPlan::new(unit);

            // 优先级 1：拾旗。
            if unit.carrying_flag.is_none() {
                if let Some(flag_id) = ground_flag_at(obs, unit.pos) {
                    if plan.try_pick(obs) {
                        claimed.insert(flag_id);
                    }
                }
            }

            // 优先级 2：移动。`carrying` 同时考虑“已经拿着”和“本 tick 刚规划拾旗”。
            // 注意：规划拾旗后 `plan.try_move` 本来就会被拒（`UnitPlan` 保守校验），
            // 这里提前判断只是为了不去浪费一次 BFS。
            let carrying = unit.carrying_flag.is_some() || plan.has_pick();
            // 未携带旗时的候选旗；BFS 会从中挑最近的一面。
            let flag_candidates: Vec<(Coord, EntityId)> = if carrying {
                Vec::new()
            } else {
                // FlagView 没有归属队字段：地上旗对任何队伍都是可抢目标（引擎只按“谁先拾到”判归属）。
                obs.flags
                    .iter()
                    .filter(|flag| flag.carrier.is_none() && !claimed.contains(&flag.id))
                    .map(|flag| (flag.pos, flag.id))
                    .collect()
            };
            let goals: Vec<Coord> = if carrying {
                if home.is_empty() {
                    // 理论上阵营区恒非空；真为空时退化为回中心，绝不让单位无处可去。
                    vec![map.center]
                } else {
                    home.clone()
                }
            } else if flag_candidates.is_empty() {
                // 场上暂时没有可见的地上旗：去中心区待命（旗在中心刷新，离得近能第一时间拿到）。
                vec![map.center]
            } else {
                flag_candidates.iter().map(|(pos, _)| *pos).collect()
            };

            let chosen = self.advance_toward(&mut plan, obs, &goals, &occupied, &reserved);
            // 认领 BFS 实际选中的那面旗（而不是曼哈顿最近的那面），避免「认领了却不去」把队友挡在门外。
            if let Some((_, flag_id)) = chosen.and_then(|index| flag_candidates.get(index)) {
                claimed.insert(*flag_id);
            }

            // 优先级 3：剩余 AP 打人。拿旗者/已规划拾旗者不打（见模块文档）。
            if plan.ap_left() > 0 && !plan.has_pick() {
                if let Some(target) = plan.best_attack_target(obs, map) {
                    plan.try_attack(obs, map, target);
                }
            }

            if let Some(dir) = plan.planned_move_dir() {
                reserved.insert(unit.pos.step(dir));
            }
            for action in plan.actions() {
                actions.push(unit_id, action.clone());
            }
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{count_event, count_illegal, replay_jsonl, run_match};
    use protocol::GameEvent;

    /// 见 `random.rs` 里同名的说明：只用于捕捉合法性判定的整体退化。
    /// 实测：2 队 5 个种子 2..8 次；3 队 5 个种子 33..47 次——三方同时抢中心同一面旗时，
    /// 后手单位的拾旗必然失败，这是「抢旗竞争」而非 AI 判定错误。
    const ILLEGAL_BUDGET: usize = 60;

    #[test]
    fn greedy_flag_ai_plays_full_match_without_panic() {
        let outcome = run_match(AI_GREEDY_FLAG, 2, 0x5EED_0011, true);
        assert_eq!(outcome.result.team_count(), 2);
        assert!(
            outcome.result.ticks > 0 && outcome.result.ticks <= 300,
            "整局 tick 数应在 (0, max_ticks] 内，实际 {}",
            outcome.result.ticks
        );
        let illegal = count_illegal(&outcome);
        assert!(
            illegal <= ILLEGAL_BUDGET,
            "greedy_flag AI 非法动作数 {illegal} 超出预算 {ILLEGAL_BUDGET}"
        );
        // 行为底线：抢旗 AI 整局必须真的拾起过旗并把旗送回阵营（有得分）。
        // 否则它可能只是「会走路但不去抢旗」——那种退化上面所有断言都发现不了。
        assert!(
            count_event(&outcome, |event| matches!(
                event,
                GameEvent::FlagPicked { .. }
            )) > 0,
            "greedy_flag 整局一次都没拾旗"
        );
        assert!(
            outcome.result.scores.iter().any(|score| *score > 0),
            "greedy_flag 整局一分未得（分数 {:?}），抢旗-回营链路可能断了",
            outcome.result.scores
        );
    }

    #[test]
    fn greedy_flag_ai_same_seed_reproduces_exactly() {
        let first = run_match(AI_GREEDY_FLAG, 2, 0x5EED_0012, true);
        let second = run_match(AI_GREEDY_FLAG, 2, 0x5EED_0012, true);
        assert_eq!(first.result, second.result, "同 ai_seed 必须得到同一局结果");
        assert_eq!(
            replay_jsonl(&first),
            replay_jsonl(&second),
            "同 ai_seed 的回放必须逐字节一致"
        );
    }

    #[test]
    fn greedy_flag_ai_handles_three_teams() {
        let outcome = run_match(AI_GREEDY_FLAG, 3, 0x5EED_0013, false);
        assert_eq!(outcome.result.team_count(), 3);
    }
}
