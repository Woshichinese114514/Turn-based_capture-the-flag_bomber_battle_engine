//! `random` 基线 AI：纯随机，但**只提交合法动作**。
//!
//! 规格（docs/rules.md §13）：随机移动、随机攻击射程内敌人、随机放炸弹、随机拾旗，
//! 必须遵纪守法。用 `ai_seed` 播种自己的 [`sim::Rng`]。
//!
//! ## 随机性与可复现性
//!
//! 「纯随机」不是「不可复现」：本 AI 的所有随机数都来自自己那个用 `ai_seed` 播种的 RNG，
//! 不碰 `sim` 的 RNG、不用系统时间、不用线程本地随机。因此同一局配置跑两次，
//! 每个 tick 的选择序列完全相同 → 回放逐字节一致（docs/rules.md §14.14 的硬要求）。
//!
//! ## 为什么随机也要「守法」
//!
//! 被引擎拒绝的动作会生成 `illegal_action` 事件（docs/rules.md §7.7），基线 AI 的验收
//! 标准之一就是「不产生非法动作（除设计允许的少数）」。所以这里不是「随便挑一个方向扔出去」，
//! 而是从**当前快照下必然合法**的候选里抽：可站立的空目标格、射程+视线内的敌人、
//! 非阵营格上的炸弹、脚下的地上旗。合法性判定集中在 [`crate::common::UnitPlan`]。
//!
//! ## 决策结构（每个单位、每个 tick）
//!
//! 1. 在「移动 / 攻击 / 放炸弹 / 拾旗」四类里**均匀抽一类**（只抽当前可执行的那些类），
//!    再在该类里均匀抽一个具体动作——这样四类行为的期望频率接近，符合「纯随机基线」的定位；
//! 2. 若还有剩余 AP（单位有 2 AP），再抽一次第二个动作。
//!    第二个动作的合法性由 `UnitPlan` 按「第一个动作成功 / 失败」两种假设保守校验，
//!    因此第一次移动因冲突失败时，第二次攻击不会变成非法动作。

use std::collections::HashSet;

use protocol::Coord;
use sim::ai::{Observation, TeamActions, TeamAi};
use sim::Rng;

use crate::common::{occupied_cells, UnitPlan};
use crate::AI_RANDOM;

/// 每 tick 允许为单个单位规划的动作数上限（= `RulesConfig.ap_per_unit`）。
///
/// 这里再写一遍常量而不是信任 `ap_left` 永远是 2：`UnitPlan` 本身用 `ap_left` 做预算，
/// 这个上限只是「不要把 2 AP 花在两个同类动作上」的可读性上限。若将来 AP 变化需要同步。
const MAX_ACTIONS_PER_UNIT: usize = 2;

pub struct RandomAi {
    rng: Rng,
}

impl RandomAi {
    /// 用 `ai_seed` 播种本局的随机源（由 CLI 按 `ai_seed = ai_salt ^ ((team)<<32) ^ 0x9E37_79B9` 算出）。
    pub fn new(ai_seed: u64) -> Self {
        Self {
            rng: Rng::new(ai_seed),
        }
    }

    /// 抽一个动作并登记到 `plan`；没有任何可执行候选时返回 `false`（单位本 tick 什么都不做）。
    fn apply_random_action(
        &mut self,
        plan: &mut UnitPlan<'_>,
        obs: &Observation,
        occupied: &HashSet<Coord>,
        reserved: &HashSet<Coord>,
    ) -> bool {
        // 候选先各自算好：`UnitPlan` 的查询方法都已经把「引擎结算顺序」的坑考虑进去了
        // （例如移动会改变攻击/炸弹/拾旗发生的位置，规划器会用两个位置校验）。
        let moves = plan.legal_moves(obs, occupied, reserved);
        let attacks = plan.legal_attack_targets(obs, &obs.map);
        let bomb = plan.can_bomb(obs);
        let pick = plan.can_pick(obs);

        let mut categories: Vec<Category> = Vec::with_capacity(4);
        if !moves.is_empty() {
            categories.push(Category::Move);
        }
        if !attacks.is_empty() {
            categories.push(Category::Attack);
        }
        if bomb {
            categories.push(Category::Bomb);
        }
        if pick {
            categories.push(Category::Pick);
        }
        if categories.is_empty() {
            return false;
        }

        let category = categories[self.rng.gen_index(categories.len())];
        match category {
            Category::Move => {
                let dir = moves[self.rng.gen_index(moves.len())];
                plan.try_move(obs, dir, occupied, reserved)
            }
            Category::Attack => {
                let target = attacks[self.rng.gen_index(attacks.len())];
                plan.try_attack(obs, &obs.map, target)
            }
            Category::Bomb => plan.try_bomb(obs),
            Category::Pick => plan.try_pick(obs),
        }
    }
}

/// 可随机选择的动作类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Move,
    Attack,
    Bomb,
    Pick,
}

impl TeamAi for RandomAi {
    fn name(&self) -> &str {
        AI_RANDOM
    }

    fn decide(&mut self, obs: &Observation) -> TeamActions {
        let mut actions = TeamActions::new();
        // 全体存活单位占用的格子：随机移动也不踩到别人（踩空格的走法才不会被判「目标格被占用」）。
        let occupied_all = occupied_cells(obs, None);
        // 本 tick 已被己方其它单位预定为目标格的位置，避免自家两个单位撞在一起（move_conflict）。
        let mut reserved: HashSet<Coord> = HashSet::new();

        for &unit_id in &obs.my_units {
            let Some(unit) = obs.me(unit_id) else {
                continue;
            };
            if !unit.alive {
                // 死亡单位不在图上、AP 为 0：不提交任何动作（复活由 sim 负责）。
                continue;
            }

            // 自己站的格子对自己不是障碍。
            let mut occupied = occupied_all.clone();
            occupied.remove(&unit.pos);

            let mut plan = UnitPlan::new(unit);
            let mut planned_actions = 0usize;
            while planned_actions < MAX_ACTIONS_PER_UNIT
                && self.apply_random_action(&mut plan, obs, &occupied, &reserved)
            {
                planned_actions += 1;
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

    /// 只允许「设计允许的少数」非法动作（tick 内连锁死亡 / 抢同一面旗）。阈值取得很宽，
    /// 目的是捕捉合法性判定整体退化（例如误判射程/视线、踩到别人格子），而不是精确计数。
    /// 实测：2 队对局 5 个种子全部为 0。
    const ILLEGAL_BUDGET: usize = 60;

    #[test]
    fn random_ai_plays_full_match_without_panic() {
        let outcome = run_match(AI_RANDOM, 2, 0x5EED_0001, true);
        assert_eq!(outcome.result.team_count(), 2);
        assert!(
            outcome.result.ticks > 0 && outcome.result.ticks <= 300,
            "整局 tick 数应在 (0, max_ticks] 内，实际 {}",
            outcome.result.ticks
        );
        let illegal = count_illegal(&outcome);
        assert!(
            illegal <= ILLEGAL_BUDGET,
            "random AI 非法动作数 {illegal} 超出预算 {ILLEGAL_BUDGET}"
        );
        // 「什么都不做的 AI」能通过上面所有断言（非法动作自然是 0），所以正面断言它真的动过。
        // 实测一局 300 tick 约 500+ 次移动、200 次放炸弹。
        assert!(
            count_event(&outcome, |event| matches!(
                event,
                GameEvent::UnitMoved { .. }
            )) > 0,
            "random 基线整局一次都没移动，说明它退化成空实现了"
        );
    }

    #[test]
    fn random_ai_same_seed_reproduces_exactly() {
        let first = run_match(AI_RANDOM, 2, 0x5EED_0002, true);
        let second = run_match(AI_RANDOM, 2, 0x5EED_0002, true);
        assert_eq!(first.result, second.result, "同 ai_seed 必须得到同一局结果");
        assert_eq!(
            replay_jsonl(&first),
            replay_jsonl(&second),
            "同 ai_seed 的回放必须逐字节一致"
        );
    }

    #[test]
    fn random_ai_handles_three_teams() {
        // 3 队是独立的规则体系（轮换先手、最多 3 面旗），需要覆盖。
        let outcome = run_match(AI_RANDOM, 3, 0x5EED_0003, false);
        assert_eq!(outcome.result.team_count(), 3);
    }
}
