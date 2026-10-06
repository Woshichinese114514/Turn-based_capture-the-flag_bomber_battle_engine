//! `defender` 基线 AI：守家。
//!
//! 规格（docs/rules.md §13）：待在己方阵营附近，不主动抢旗；攻击进入射程（曼哈顿 ≤3 且有视线）
//! 的敌人；被引走或残血时回阵营。
//!
//! ## 决策优先级（每个单位、每个 tick，按顺序消费 2 AP）
//!
//! 1. **攻击**：能打到就打（射程内、有视线）。攻击不改变位置，先规划它最划算；
//! 2. **站位**：
//!    * 离家中心超过 [`PATROL_RADIUS`]，或 HP ≤ 1（残血）→ 沿 BFS 最短路回己方阵营。
//!      阵营格内免疫爆炸伤害（docs/rules.md §1），而且死在阵营里离家近、复活后能立刻回位；
//!    * 否则若有敌人进入 [`THREAT_RADIUS`] → 贴上去打，但**不追出巡逻半径**（守家单位追出去就是失职）；
//!    * 否则在己方阵营内轮换防守站位（见下）。
//!
//! ## 为什么空闲时要在阵营内轮换站位
//!
//! 本游戏的得分方式是「把旗送回**自己的**阵营」（docs/rules.md §5），所以对手没有任何理由
//! 跑到你的阵营来——实测一局 3 队混合对局（defender vs greedy_flag vs random），6 个敌方单位
//! 距 defender 阵营中心最近也有 13 格，`press_enemy` 一次都没触发。也就是说守家 AI 的空闲时间
//! 远多于战斗时间，若空闲时完全静止，它就会退化成「一局不动的雕像」：既看不出守家行为，
//! 它的移动/合法性代码路径也永远不会被真实对局覆盖。
//!
//! 因此空闲时让单位在 [`base_cells`] 内轮换站位（每 [`PATROL_INTERVAL`] tick 全队移一格）：
//! 3 个单位散在 9 格阵营里而不是挤成一堆，被一发十字炸弹同时命中的概率更低；而且这时单位
//! 一步都不会走出阵营，完全符合「待在己方阵营附近」。
//!
//! ## 为什么「残血就回阵营」写死在代码里
//!
//! 这是 baseline，不是要打满分的 AI。血量 1 的单位在场上几乎必死（任何一次攻击都带走它），
//! 而阵亡会让对方白拿人头、己方还要等复活。回营是收益最稳的选择，且规则上完全合法。
//!
//! ## 为什么 defender 完全不碰旗
//!
//! 「守家」的语义就是不为旗子离开岗位：如果单位跑去抢旗，家里就空了。这也是三个 baseline
//! 行为差异的来源（random 乱来 / greedy_flag 专抢 / defender 专守），便于后续 AI 做 A/B 对比。

use std::collections::HashSet;

use protocol::{Coord, EntityId};
use sim::ai::{Observation, TeamActions, TeamAi};
use sim::Rng;

use crate::common::{
    base_cells, base_center, bfs_dist_field, greedy_step_toward, occupied_cells, random_legal_move,
    UnitPlan, ATTACK_RANGE,
};
use crate::AI_DEFENDER;

/// 巡逻半径：单位离己方阵营中心的曼哈顿距离超过它就必须回防。
///
/// 取 5：3×3 阵营区的最远格到中心距离为 2，半径 5 覆盖「家门口 + 一两格缓冲区」。
/// 再大就不是守家了，再小会被迫整天站在同一格（无法贴脸攻击靠过来的敌人，因为攻击需要距离 ≤3）。
const PATROL_RADIUS: i32 = 5;

/// 「敌人逼近」判定半径：敌人的曼哈顿距离 ≤ 这个值就当威胁，主动贴上去。
/// 取 6：比 `PATROL_RADIUS` 大 1，让单位在威胁还没进家门时就开始反应，但仍受巡逻半径约束。
const THREAT_RADIUS: i32 = 6;

/// 空闲时轮换防守站位的间隔（tick）。取 25：既让单位明显「活着」（一局轮换十几次），
/// 又不会为了走而走——防守站位本身不是收益来源，频繁移动反而浪费 AP。
const PATROL_INTERVAL: u32 = 25;

pub struct DefenderAi {
    rng: Rng,
}

impl DefenderAi {
    /// 用 `ai_seed` 播种本局的随机源（随机性只用于被堵死时的兜底走位）。
    pub fn new(ai_seed: u64) -> Self {
        Self {
            rng: Rng::new(ai_seed),
        }
    }

    /// 朝一组目标（己方阵营全部格子，或一个防守站位）走 BFS 最短路的一步；
    /// 路径被单位堵死时退回贪心，`random_fallback` 为真时再退回随机合法移动。
    ///
    /// 为什么要有随机兜底：BFS 会把「被单位占住的格子」算作障碍（否则规划出来的路会要求穿人而过），
    /// 于是单位可能被队友堵在角落里拿到 `None`。回防时随机走一步反而可能挪开，避免整局钉死；
    /// 轮换站位时不用随机兜底——站位不值得为它乱走，堵着就等下一轮。
    fn move_toward(
        &mut self,
        plan: &mut UnitPlan<'_>,
        obs: &Observation,
        goals: &[Coord],
        occupied: &HashSet<Coord>,
        reserved: &HashSet<Coord>,
        random_fallback: bool,
    ) {
        let from = plan.original_pos();
        let field = bfs_dist_field(&obs.map, obs.team, goals, occupied);
        if matches!(field.nearest_source(from), Some((_, 0))) {
            return; // 已经站在目标格上（在阵营里 / 已在岗位上）
        }
        if let Some(dir) = field.step_toward(from, reserved) {
            if plan.try_move(obs, dir, occupied, reserved) {
                return;
            }
        }
        if let Some(dir) = greedy_step_toward(&obs.map, obs.team, from, goals, occupied, reserved) {
            if plan.try_move(obs, dir, occupied, reserved) {
                return;
            }
        }
        if random_fallback {
            if let Some(dir) =
                random_legal_move(&mut self.rng, &obs.map, obs.team, from, occupied, reserved)
            {
                plan.try_move(obs, dir, occupied, reserved);
            }
        }
    }

    /// 贴近逼近家门的敌人：只走一步，且不允许把自己走出巡逻半径。
    fn press_enemy(
        &self,
        plan: &mut UnitPlan<'_>,
        obs: &Observation,
        center: Coord,
        enemy_pos: Coord,
        occupied: &HashSet<Coord>,
        reserved: &HashSet<Coord>,
    ) {
        let from = plan.original_pos();
        if from.manhattan(enemy_pos) <= ATTACK_RANGE {
            return; // 已经在射程内，攻击由优先级 1 处理
        }
        if let Some(dir) =
            greedy_step_toward(&obs.map, obs.team, from, &[enemy_pos], occupied, reserved)
        {
            let next = from.step(dir);
            // 不 `return`：后面就是函数结尾，成功与否都不再做事（clippy 也会提示多余的 return）。
            if next.manhattan(center) <= PATROL_RADIUS {
                plan.try_move(obs, dir, occupied, reserved);
            }
        }
        // 追不到（被墙/单位挡住，或会追出巡逻半径）：留在原地待命，不硬追。
    }
}

/// 空闲单位本轮应占的防守站位（己方阵营内的一格）。
///
/// 用 `tick / PATROL_INTERVAL + unit_id` 作为轮换相位：全队每隔 [`PATROL_INTERVAL`] tick 一起
/// 换到「相邻的下一格」，3 个单位分别落在连续的 3 个阵营格上，天然散开。
/// 完全由快照（tick、unit_id、阵营格列表）决定，不含随机数，因此不影响可复现性。
fn patrol_post(home: &[Coord], unit_id: EntityId, tick: u32) -> Option<Coord> {
    if home.is_empty() {
        return None;
    }
    let rotation = (tick / PATROL_INTERVAL) as usize;
    let index = (rotation + unit_id as usize) % home.len();
    home.get(index).copied()
}

/// 找出进入威胁半径的、离己方阵营中心最近的敌人位置。
///
/// 用位置（`Coord`）而不是 `EntityId` 返回：位置是即时快照，`press_enemy` 只需要一个目标点，
/// 而且「最近的」比「第一个」更符合守家的直觉（先挡最危险的那个）。
fn nearest_threat(obs: &Observation, center: Coord) -> Option<Coord> {
    let mut best: Option<(i32, EntityId, Coord)> = None;
    for unit in &obs.units {
        if !unit.alive || unit.team == obs.team {
            continue;
        }
        let distance = unit.pos.manhattan(center);
        if distance > THREAT_RADIUS {
            continue;
        }
        // 先比距离，距离相同比 id：保证同快照下结果确定，不依赖 `obs.units` 的顺序（可复现性）。
        if best.is_none_or(|(best_distance, best_id, _)| {
            (distance, unit.id) < (best_distance, best_id)
        }) {
            best = Some((distance, unit.id, unit.pos));
        }
    }
    best.map(|(_, _, pos)| pos)
}

impl TeamAi for DefenderAi {
    fn name(&self) -> &str {
        AI_DEFENDER
    }

    fn decide(&mut self, obs: &Observation) -> TeamActions {
        let map = &obs.map;
        let mut actions = TeamActions::new();
        let occupied_all = occupied_cells(obs, None);
        let mut reserved: HashSet<Coord> = HashSet::new();
        let home = base_cells(map, obs.team);
        let home_center = base_center(&home).unwrap_or(map.center);

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

            // 优先级 1：能打就打。先规划攻击；若之后还要移动，`UnitPlan` 会用新位置重新校验
            // 攻击合法性（结算顺序是 移动→攻击，docs/rules.md §7.8），不会产生非法动作。
            if let Some(target) = plan.best_attack_target(obs, map) {
                plan.try_attack(obs, map, target);
            }

            // 优先级 2：站位。
            let distance_home = unit.pos.manhattan(home_center);
            let wounded = unit.hp <= 1;
            if distance_home > PATROL_RADIUS || wounded {
                // 被引走或残血：回阵营（阵营内免疫爆炸，且离家近便于复活后归位）。
                self.move_toward(&mut plan, obs, &home, &occupied, &reserved, true);
            } else if let Some(enemy_pos) = nearest_threat(obs, home_center) {
                self.press_enemy(&mut plan, obs, home_center, enemy_pos, &occupied, &reserved);
            } else if let Some(post) = patrol_post(&home, unit.id, obs.tick) {
                // 空闲：轮换防守站位（只在真的不在岗位上时才走，避免每 tick 抖动）。
                if unit.pos != post {
                    self.move_toward(&mut plan, obs, &[post], &occupied, &reserved, false);
                }
            }

            // 优先级 3：移动后再补一次攻击（移动失败时位置未变，同样合法；移动成功则打新位置的目标）。
            if plan.ap_left() > 0 {
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
    use crate::common::test_support::{
        count_event, count_illegal, obs_unit, observation, observation_with, replay_jsonl,
        run_match, test_map,
    };
    use protocol::GameEvent;
    use sim::ai::Action;

    /// 见 `random.rs` 里同名的说明：只用于捕捉合法性判定的整体退化。
    const ILLEGAL_BUDGET: usize = 60;

    /// 9×9 空地；0 队阵营在左上 3×3（中心 (1,1)），1 队阵营在右上 3×3。
    /// 地形全空，让测试只验证站位逻辑本身，不被墙/视线干扰。
    fn base_map() -> sim::ai::MapView {
        test_map(9, 9, &[0u8; 81], vec![Coord::new(0, 0), Coord::new(6, 0)])
    }

    /// 本 tick 第一个移动动作的目的地（`from` 是该单位的原始位置）。
    fn move_dest(from: Coord, actions: &TeamActions) -> Option<Coord> {
        actions
            .commands
            .iter()
            .find_map(|command| match &command.action {
                Action::Move(dir) => Some(from.step(*dir)),
                _ => None,
            })
    }

    #[test]
    fn defender_ai_plays_full_match_without_panic() {
        let outcome = run_match(AI_DEFENDER, 2, 0x5EED_0021, true);
        assert_eq!(outcome.result.team_count(), 2);
        assert!(
            outcome.result.ticks > 0 && outcome.result.ticks <= 300,
            "整局 tick 数应在 (0, max_ticks] 内，实际 {}",
            outcome.result.ticks
        );
        let illegal = count_illegal(&outcome);
        assert!(
            illegal <= ILLEGAL_BUDGET,
            "defender AI 非法动作数 {illegal} 超出预算 {ILLEGAL_BUDGET}"
        );
        // 「不做事的 AI」也能通过上面所有断言，所以必须正面断言它真的动过：
        // 真实对局里敌人几乎不会靠近阵营（见模块文档的实测），移动全部来自阵营内轮换站位。
        assert!(
            count_event(&outcome, |event| matches!(
                event,
                GameEvent::UnitMoved { .. }
            )) > 0,
            "整局至少要有移动事件，否则 defender 等于没实现"
        );
    }

    #[test]
    fn defender_ai_same_seed_reproduces_exactly() {
        let first = run_match(AI_DEFENDER, 2, 0x5EED_0022, true);
        let second = run_match(AI_DEFENDER, 2, 0x5EED_0022, true);
        assert_eq!(first.result, second.result, "同 ai_seed 必须得到同一局结果");
        assert_eq!(
            replay_jsonl(&first),
            replay_jsonl(&second),
            "同 ai_seed 的回放必须逐字节一致"
        );
    }

    #[test]
    fn defender_ai_handles_three_teams() {
        let outcome = run_match(AI_DEFENDER, 3, 0x5EED_0023, false);
        assert_eq!(outcome.result.team_count(), 3);
    }

    #[test]
    fn defender_ai_mixes_with_other_baselines() {
        // 混合对局（defender vs greedy_flag vs random）比同型对局更接近真实使用：
        // 注册表造出的三种 AI 必须能在同一局里共存且不 panic。
        let registry = crate::register_all();
        let config = sim::MatchConfig::new(3, 0x5EED_0024);
        let names = [AI_DEFENDER, crate::AI_GREEDY_FLAG, crate::AI_RANDOM];
        let ais: Vec<Box<dyn TeamAi>> = names
            .iter()
            .enumerate()
            .map(|(team, name)| {
                registry
                    .create(name, config.ai_seed_for(team as u8))
                    .expect("三个内置 AI 都必须已注册")
            })
            .collect();
        let outcome = sim::play(config, ais, true).expect("混合对局不应失败");
        assert_eq!(outcome.result.team_count(), 3);
        assert!(count_illegal(&outcome) <= ILLEGAL_BUDGET * 3);
    }

    // 以下测试用合成 `Observation` 直接覆盖规格（docs/rules.md §13）里的每条行为分支。
    // 为什么必须这么做：真实对局里敌人几乎不会进入守家的威胁半径（见模块文档实测），
    // 「回防 / 贴敌 / 攻击 / 巡逻」这些分支在整局测试里根本不会被走到。

    #[test]
    fn defender_returns_home_when_dragged_too_far() {
        // (1,7) 距阵营中心 (1,1) 6 格 > PATROL_RADIUS(5) → 必须回防。
        let far = Coord::new(1, 7);
        let obs = observation(
            base_map(),
            vec![obs_unit(0, 0, far.x, far.y, 2)],
            Vec::new(),
        );
        let actions = DefenderAi::new(1).decide(&obs);
        let dest = move_dest(far, &actions).expect("超巡逻半径时应当走一步回阵营");
        // 1 点 AP 只能走一格，所以只能断言「朝阵营逼近」，不能要求一步跨进 3×3 阵营区。
        assert!(
            dest.manhattan(Coord::new(1, 1)) < far.manhattan(Coord::new(1, 1)),
            "必须朝阵营中心靠近：{far:?} → {dest:?}"
        );
        assert_eq!(dest, Coord::new(1, 6), "同列直线回防应取最短一步");
    }

    #[test]
    fn defender_returns_home_when_wounded() {
        // (3,3) 距阵营中心只有 4 格（未超巡逻半径），但 HP=1 → 残血仍要回阵营。
        let hurt = Coord::new(3, 3);
        let mut unit = obs_unit(0, 0, hurt.x, hurt.y, 2);
        unit.hp = 1;
        let obs = observation(base_map(), vec![unit], Vec::new());
        let actions = DefenderAi::new(1).decide(&obs);
        let dest = move_dest(hurt, &actions).expect("残血单位应当走一步回阵营");
        assert!(
            dest.manhattan(Coord::new(1, 1)) < hurt.manhattan(Coord::new(1, 1)),
            "残血单位必须朝阵营中心靠近：{hurt:?} → {dest:?}"
        );
    }

    #[test]
    fn defender_presses_enemy_inside_threat_radius() {
        // 守卫在阵营中心 (1,1)，敌人在 (1,5)：距中心 4 ≤ THREAT_RADIUS(6) 但距守卫 4 > 射程 3。
        let guard = Coord::new(1, 1);
        let obs = observation(
            base_map(),
            vec![obs_unit(0, 0, guard.x, guard.y, 2), obs_unit(1, 1, 1, 5, 2)],
            Vec::new(),
        );
        let actions = DefenderAi::new(1).decide(&obs);
        assert!(
            !actions
                .commands
                .iter()
                .any(|command| matches!(&command.action, Action::Attack(_))),
            "敌人距守卫 4 格（超出射程 3），本 tick 不应攻击"
        );
        let dest = move_dest(guard, &actions).expect("威胁进圈时应当贴上去");
        assert_eq!(dest, Coord::new(1, 2), "应当朝敌人方向前进一步");
    }

    #[test]
    fn defender_attacks_enemy_in_range() {
        // 敌人 (1,3) 距守卫 (1,1) 为 2 格、直线无遮挡 → 射程内且有视线，必须攻击。
        let obs = observation(
            base_map(),
            vec![obs_unit(0, 0, 1, 1, 2), obs_unit(9, 1, 1, 3, 2)],
            Vec::new(),
        );
        let actions = DefenderAi::new(1).decide(&obs);
        assert!(
            actions
                .commands
                .iter()
                .any(|command| matches!(&command.action, Action::Attack(target) if *target == 9)),
            "射程内且有视线的敌人必须被攻击，实际动作 {actions:?}"
        );
    }

    #[test]
    fn idle_defender_rotates_guard_posts_inside_base() {
        // tick=1 → 轮换相位 0，0 号单位的岗位是阵营格列表第 0 格（左上角）；
        // 它从 (1,1) 出发，必须走一步且仍留在己方阵营内。
        let from = Coord::new(1, 1);
        let obs = observation_with(
            0,
            1,
            base_map(),
            vec![obs_unit(0, 0, from.x, from.y, 2)],
            Vec::new(),
        );
        let actions = DefenderAi::new(1).decide(&obs);
        let dest = move_dest(from, &actions).expect("空闲单位应当走向自己的防守岗位");
        assert!(
            obs.map.in_base(0, dest.x, dest.y),
            "巡逻一步必须仍在己方阵营内，实际 {dest:?}"
        );
    }

    #[test]
    fn idle_defender_already_on_post_wastes_no_ap() {
        // 0 号单位的岗位就是 (0,0)，它已经站在那里：无敌人、无威胁 → 不该有任何动作。
        let obs = observation_with(0, 1, base_map(), vec![obs_unit(0, 0, 0, 0, 2)], Vec::new());
        let actions = DefenderAi::new(1).decide(&obs);
        assert!(
            actions.commands.is_empty(),
            "已在岗位上且无威胁时不应浪费 AP，实际动作 {actions:?}"
        );
    }

    #[test]
    fn patrol_post_stays_inside_base_and_is_deterministic() {
        let home = vec![
            Coord::new(0, 0),
            Coord::new(1, 0),
            Coord::new(2, 0),
            Coord::new(0, 1),
            Coord::new(1, 1),
            Coord::new(2, 1),
            Coord::new(0, 2),
            Coord::new(1, 2),
            Coord::new(2, 2),
        ];
        for tick in [0u32, 1, 24, 25, 26, 299] {
            for unit_id in 0..3u32 {
                let post = patrol_post(&home, unit_id, tick).expect("非空阵营必有岗位");
                assert!(home.contains(&post), "岗位必须取自阵营格列表");
            }
        }
        // 同 tick、同 id 必须同结果（不引入随机性，保证回放可复现）。
        assert_eq!(patrol_post(&home, 1, 30), patrol_post(&home, 1, 30));
        // 岗位随时间轮换（每 PATROL_INTERVAL tick 换一格）。
        assert_ne!(patrol_post(&home, 0, 0), patrol_post(&home, 0, 25));
        // 空阵营不 panic 而是返回 None。
        assert_eq!(patrol_post(&[], 0, 0), None);
    }
}
