//! 聚合：`MatchResult[] → ScoringReport`。
//!
//! ## 聚合的两个维度
//!
//! 1. **按 AI 名字**（[`ScoringReport::by_ai_name`]）：跨槽位汇总同名 AI 的表现。
//!    这是「实力榜」的主视图，也是跨批次比较的对象。
//! 2. **按槽位**（[`ScoringReport::by_slot`]）：每个队伍位置的表现。
//!    槽位顺序在引擎里是轮换的（`(tick + slot) % teams`），所以槽位本身不应该有优势；
//!    如果某槽位胜率长期异常高，说明引擎的轮换/先手设计有问题——这个视图就是为此存在的
//!    「健康检查」，而不是实力排名。
//!
//! ## 原始指标的归一化（全部落到 [0,1]）
//!
//! * `win_rate = wins / matches`（平局既不算胜也不算负，只进 `draw_rate`）。
//! * `avg_score_share`：每局「本队得分 / 全场总分」，再对局数取平均。
//!   总分为 0（双方都没得分）时该局记 0.5——「没有信息」比「弱」更接近事实。
//!   注意是**先算每局占比再平均**，而不是「总得分/总全场得分」：
//!   后者会被得分高的个别局主导，前者对每个 AI 一视同仁。
//! * `avg_kill_ratio = kills / (kills + deaths)`（分母 0 时记 0.5）。
//!   用比值而不是绝对击杀数，是为了消除「局数不同」和「地图大小不同」的影响。
//!
//! 所有除法都有「分母为 0 → 0.5」的兜底，保证报告里永远不会出现 NaN（会序列化成 null）。

use std::collections::BTreeMap;

use protocol::{MatchResult, TeamId};
use serde::{Deserialize, Serialize};

use crate::config::ScoringConfig;
use crate::outcome::{HighestScoreRule, Outcome, OutcomeRule};
use crate::rating::{compute_rating, compute_strength, shrink_win_rate};

/// 某个 AI（按名字）的汇总。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AiAggregate {
    /// AI 注册名。
    pub ai_name: String,
    /// 该 AI 出战的局数。
    pub matches: u32,
    pub wins: u32,
    pub draws: u32,
    pub losses: u32,
    /// 原始胜率（未收缩），仅作展示；评分用的是收缩后的胜率。
    pub win_rate: f64,
    /// 平局率。
    pub draw_rate: f64,
    /// 场均得分（原始分数，不是占比）。
    pub avg_score: f64,
    /// 场均击杀。
    pub avg_kills: f64,
    /// 场均死亡。
    pub avg_deaths: f64,
    /// 场均全局回合数（可用于发现「速胜/拖满」的 AI 风格）。
    pub avg_ticks: f64,
    /// 综合实力 ∈ [min_strength, max_strength]，0.5 = 平均水准。
    pub strength: f64,
    /// log scale 分数：`1000 + 500·log10(strength/(1-strength))`。
    /// **只在同一 `ScoringReport.teams` 体系内可比。**
    pub rating: f64,
}

/// 某个槽位（队伍位置）的汇总。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlotAggregate {
    /// 槽位下标（0 起）。
    pub slot: u8,
    /// 该槽位出现最多的 AI 名字（同一槽位混用多个 AI 时取众数，平票按字典序）。
    pub ai_name: String,
    pub wins: u32,
    pub matches: u32,
    pub win_rate: f64,
    /// 与 [`AiAggregate::rating`] 同一套 log scale 映射。
    pub rating: f64,
}

/// 一次批量运行的评分报告，最终写成 `summary.json`。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoringReport {
    /// 参与聚合的局数。
    pub total_matches: u32,
    /// **分数体系**：2 或 3。2 队与 3 队的分数不可比较（见 crate 文档）。
    pub teams: u8,
    /// 规则版本：不同 `rules_version` 的批次不应放在同一张榜里比较。
    pub rules_version: u32,
    /// 地图生成版本：不同版本会让「同一颗种子」变成不同地图，同样不可混比。
    pub map_gen_version: u32,
    /// 生成本报告所用的评分配置（含权重与 `teams`），便于事后核对与复现。
    pub config: ScoringConfig,
    /// 按 AI 名字聚合，按 rating 降序（平票按名字升序，保证输出确定）。
    pub by_ai_name: Vec<AiAggregate>,
    /// 按槽位聚合，按槽位升序。
    pub by_slot: Vec<SlotAggregate>,
}

/// 一个桶的累加器：先把原始计数与和收集齐，最后一次性换算成比率，避免重复计算。
#[derive(Clone, Debug, Default)]
struct Acc {
    matches: u32,
    wins: u32,
    draws: u32,
    losses: u32,
    score_sum: f64,
    score_share_sum: f64,
    kills_sum: f64,
    deaths_sum: f64,
    kill_ratio_sum: f64,
    ticks_sum: f64,
}

impl Acc {
    /// 把一局中「某个队伍」的观测值加入桶。
    ///
    /// 参数多是有意的：这些量全部来自同一局的同一次归一化计算（得分、占比、击杀、死亡、
    /// 击杀比、tick），拆成结构体会让调用点每次都要先拼一个只用一次的对象，反而更难读。
    /// 本函数是私有实现细节，不对外暴露，因此这里显式允许 clippy 的 `too_many_arguments`。
    #[allow(clippy::too_many_arguments)]
    fn observe(
        &mut self,
        outcome: Outcome,
        score: f64,
        score_share: f64,
        kills: f64,
        deaths: f64,
        kill_ratio: f64,
        ticks: f64,
    ) {
        self.matches += 1;
        match outcome {
            Outcome::Win => self.wins += 1,
            Outcome::Draw => self.draws += 1,
            Outcome::Loss => self.losses += 1,
        }
        self.score_sum += score;
        self.score_share_sum += score_share;
        self.kills_sum += kills;
        self.deaths_sum += deaths;
        self.kill_ratio_sum += kill_ratio;
        self.ticks_sum += ticks;
    }

    fn mean(sum: f64, matches: u32) -> f64 {
        if matches == 0 {
            0.5
        } else {
            sum / f64::from(matches)
        }
    }

    fn win_rate(&self) -> f64 {
        Self::mean(f64::from(self.wins), self.matches)
    }

    fn draw_rate(&self) -> f64 {
        Self::mean(f64::from(self.draws), self.matches)
    }

    /// 走完「收缩 → strength → rating」流水线，返回 (strength, rating)。
    fn strength_and_rating(&self, config: &ScoringConfig) -> (f64, f64) {
        let shrunk = shrink_win_rate(
            f64::from(self.wins),
            f64::from(self.matches),
            config.win_rate_prior_matches,
        );
        let strength = compute_strength(
            shrunk,
            Self::mean(self.score_share_sum, self.matches),
            Self::mean(self.kill_ratio_sum, self.matches),
            config,
        );
        (strength, compute_rating(strength, config))
    }
}

/// 槽位累加器额外记录名字出现次数，用于在混用多个 AI 时给出众数名字。
#[derive(Clone, Debug, Default)]
struct SlotAcc {
    acc: Acc,
    name_counts: BTreeMap<String, u32>,
}

/// 把一个队伍的一局观测值喂给某个累加器（AI 桶与槽位桶共用这段归一化逻辑）。
#[allow(clippy::too_many_arguments)]
fn observe_team(
    acc: &mut Acc,
    result: &MatchResult,
    team: TeamId,
    outcome: Outcome,
    total_score: i64,
) {
    let idx = team as usize;
    // total_score > 0 才计算占比：总分 0（或异常负数）说明这一局没有区分信息，记 0.5。
    let score_share = if total_score > 0 {
        f64::from(result.scores[idx]) / total_score as f64
    } else {
        0.5
    };
    let kills = f64::from(result.kills.get(idx).copied().unwrap_or(0));
    let deaths = f64::from(result.deaths.get(idx).copied().unwrap_or(0));
    let kill_ratio = if kills + deaths > 0.0 {
        kills / (kills + deaths)
    } else {
        0.5
    };
    acc.observe(
        outcome,
        f64::from(result.scores[idx]),
        score_share,
        kills,
        deaths,
        kill_ratio,
        f64::from(result.ticks),
    );
}

/// 把一批对局结果聚合成评分报告。
///
/// * `teams`：**分数体系**（2 或 3）。调用方应先跑
///   [`crate::ScoringError`] / [`crate::error::validate_teams`] 校验。
/// * `rules_version` / `map_gen_version`：写进报告，提醒读者「这份分能跟谁比」。
/// * `config`：权重与映射参数；报告里会把 `config.teams` 覆盖为传入的 `teams`，
///   保证「报告标注的体系」与「实际聚合的体系」永远一致。
///
/// 判定口径固定使用默认的 [`HighestScoreRule`]：与 `sim` 的默认胜负判定一致，
/// 且聚合必须通过 `OutcomeRule` 得到 W/D/L，不允许在别处重复实现。
pub fn aggregate(
    results: &[MatchResult],
    teams: u8,
    rules_version: u32,
    map_gen_version: u32,
    config: &ScoringConfig,
) -> ScoringReport {
    let rule = HighestScoreRule;
    let slot_count = usize::from(teams);

    let mut by_ai: BTreeMap<String, Acc> = BTreeMap::new();
    let mut by_slot: Vec<SlotAcc> = vec![SlotAcc::default(); slot_count];

    for result in results {
        let team_total = result.team_count();
        // 本局全场总分：用于把每队得分归一成「占比」。
        let total_score: i64 = result.scores.iter().map(|&s| i64::from(s)).sum();

        // `team` 不只是下标：它同时是 `TeamId`、`rule.outcome` 与 `ai_name_of` 的入参，
        // 而且循环上界来自**本局结果**（`team_total`）而不是 `by_slot` 的长度，
        // 所以不能改成 `by_slot.iter().enumerate()`（结果队伍数少于报告槽位数时会漏算 AI 榜）。
        #[allow(clippy::needless_range_loop)]
        for team in 0..team_total {
            let team_id = team as TeamId;
            let outcome = rule.outcome(result, team_id);
            let name = result.ai_name_of(team_id).to_string();

            // ① 按 AI 名字聚合：跨槽位汇总同名 AI 的所有出场。
            observe_team(
                by_ai.entry(name.clone()).or_default(),
                result,
                team_id,
                outcome,
                total_score,
            );

            // ② 按槽位聚合：只统计声明体系内的槽位；超出声明队伍数的队伍
            //    仍然进 AI 榜（数据不该丢），但不进槽位榜（避免下标越界）。
            if team < slot_count {
                let slot = &mut by_slot[team];
                *slot.name_counts.entry(name).or_insert(0) += 1;
                observe_team(&mut slot.acc, result, team_id, outcome, total_score);
            }
        }
    }

    // 报告里的配置以传入的 `teams` 为准（见函数文档）。
    let mut report_config = config.clone();
    report_config.teams = teams;

    let mut ai_list: Vec<AiAggregate> = by_ai
        .into_iter()
        .map(|(ai_name, acc)| {
            let (strength, rating) = acc.strength_and_rating(&report_config);
            AiAggregate {
                ai_name,
                matches: acc.matches,
                wins: acc.wins,
                draws: acc.draws,
                losses: acc.losses,
                win_rate: acc.win_rate(),
                draw_rate: acc.draw_rate(),
                avg_score: Acc::mean(acc.score_sum, acc.matches),
                avg_kills: Acc::mean(acc.kills_sum, acc.matches),
                avg_deaths: Acc::mean(acc.deaths_sum, acc.matches),
                avg_ticks: Acc::mean(acc.ticks_sum, acc.matches),
                strength,
                rating,
            }
        })
        .collect();
    // 排序：rating 降序；同分按名字升序。BTreeMap 已保证同分时进入顺序确定，
    // 但显式排序让「输出逐字节可复现」不依赖容器实现细节。
    ai_list.sort_by(|a, b| {
        b.rating
            .partial_cmp(&a.rating)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.ai_name.cmp(&b.ai_name))
    });

    let slot_list: Vec<SlotAggregate> = by_slot
        .iter()
        .enumerate()
        .map(|(slot, slot_acc)| {
            let acc = &slot_acc.acc;
            let (_, rating) = acc.strength_and_rating(&report_config);
            SlotAggregate {
                slot: slot as u8,
                ai_name: mode_name(&slot_acc.name_counts),
                wins: acc.wins,
                matches: acc.matches,
                win_rate: acc.win_rate(),
                rating,
            }
        })
        .collect();

    ScoringReport {
        total_matches: results.len() as u32,
        teams,
        rules_version,
        map_gen_version,
        config: report_config,
        by_ai_name: ai_list,
        by_slot: slot_list,
    }
}

/// 取出现次数最多的名字；平票取字典序最小，保证确定性。
fn mode_name(counts: &BTreeMap<String, u32>) -> String {
    counts
        .iter()
        .max_by(|(name_a, count_a), (name_b, count_b)| {
            // 次数升序比较；次数相同则名字「降序」，这样 max_by 会选中名字升序最小的那个。
            count_a.cmp(count_b).then_with(|| name_b.cmp(name_a))
        })
        .map(|(name, _)| name.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(
        index: u32,
        scores: Vec<i32>,
        kills: Vec<u32>,
        deaths: Vec<u32>,
        winner: Option<TeamId>,
        names: &[&str],
    ) -> MatchResult {
        MatchResult {
            match_index: index,
            seed: 1000 + u64::from(index),
            map_gen_version: 1,
            ticks: 300,
            scores,
            kills,
            deaths,
            winner,
            ai_names: names.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// 测试 19：手造 3 局，逐项校对胜/平/负、平均分、占比、kill ratio 与评分。
    #[test]
    fn aggregate_matches_hand_computed_values() {
        let results = vec![
            // 局 1：random 赢 2:1，击杀 3:1，死亡 1:3
            mk(0, vec![2, 1], vec![3, 1], vec![1, 3], Some(0), &["random", "defender"]),
            // 局 2：1:1 平局
            mk(1, vec![1, 1], vec![2, 2], vec![2, 2], None, &["random", "defender"]),
            // 局 3：defender 赢 3:0，击杀 0:4，死亡 4:0
            mk(2, vec![0, 3], vec![0, 4], vec![4, 0], Some(1), &["random", "defender"]),
        ];
        let config = ScoringConfig::default();
        let report = aggregate(&results, 2, 1, 1, &config);

        assert_eq!(report.total_matches, 3);
        assert_eq!(report.teams, 2, "2 队体系");
        assert_eq!(report.rules_version, 1);
        assert_eq!(report.map_gen_version, 1);
        assert_eq!(report.config.teams, 2, "报告标注体系必须与聚合体系一致");
        assert_eq!(report.by_ai_name.len(), 2);
        assert_eq!(report.by_slot.len(), 2);

        // defender 的得分占比/击杀比更高，rating 应排在前面。
        assert_eq!(report.by_ai_name[0].ai_name, "defender");
        assert_eq!(report.by_ai_name[1].ai_name, "random");

        let random = report
            .by_ai_name
            .iter()
            .find(|a| a.ai_name == "random")
            .expect("random 桶存在");
        assert_eq!(random.matches, 3);
        assert_eq!((random.wins, random.draws, random.losses), (1, 1, 1));
        assert!((random.win_rate - 1.0 / 3.0).abs() < 1e-12);
        assert!((random.draw_rate - 1.0 / 3.0).abs() < 1e-12);
        assert!((random.avg_score - 1.0).abs() < 1e-12); // (2+1+0)/3
        assert!((random.avg_kills - 5.0 / 3.0).abs() < 1e-12);
        assert!((random.avg_deaths - 7.0 / 3.0).abs() < 1e-12);
        assert!((random.avg_ticks - 300.0).abs() < 1e-12);
        // 收缩胜率 = (1 + 2.5) / (3 + 5) = 0.4375
        // strength = 0.6*0.4375 + 0.25*((2/3+0.5+0)/3) + 0.15*((0.75+0.5+0)/3)
        //          = 0.2625 + 0.25*0.388888... + 0.15*0.416666... = 0.422222...
        let expected_strength =
            0.6 * 0.4375 + 0.25 * ((2.0 / 3.0 + 0.5) / 3.0) + 0.15 * ((0.75 + 0.5) / 3.0);
        assert!(
            (random.strength - expected_strength).abs() < 1e-12,
            "strength {} vs {}",
            random.strength,
            expected_strength
        );
        let expected_rating =
            1000.0 + 500.0 * (expected_strength / (1.0 - expected_strength)).log10();
        assert!((random.rating - expected_rating).abs() < 1e-9);
        // 综合指标低于平均 → 分数低于基准 1000。
        assert!(random.rating < 1000.0, "rating = {}", random.rating);

        let defender = report
            .by_ai_name
            .iter()
            .find(|a| a.ai_name == "defender")
            .expect("defender 桶存在");
        assert_eq!(defender.matches, 3);
        assert_eq!((defender.wins, defender.draws, defender.losses), (1, 1, 1));
        assert!((defender.avg_score - 5.0 / 3.0).abs() < 1e-12); // (1+1+3)/3
        // 占比 = ((1/3) + 0.5 + 1) / 3，击杀比 = (0.25 + 0.5 + 1) / 3
        let expected_strength =
            0.6 * 0.4375 + 0.25 * ((1.0 / 3.0 + 0.5 + 1.0) / 3.0) + 0.15 * ((0.25 + 0.5 + 1.0) / 3.0);
        assert!((defender.strength - expected_strength).abs() < 1e-12);
        assert!(defender.rating > 1000.0, "rating = {}", defender.rating);

        // 槽位榜：每槽 3 局、1 胜，名字取众数。
        assert_eq!(report.by_slot[0].slot, 0);
        assert_eq!(report.by_slot[0].ai_name, "random");
        assert_eq!(report.by_slot[0].matches, 3);
        assert_eq!(report.by_slot[0].wins, 1);
        assert!((report.by_slot[0].win_rate - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(report.by_slot[1].ai_name, "defender");
        assert_eq!(report.by_slot[1].wins, 1);
    }

    /// 3 队体系：报告标注 teams = 3，三个槽位都在，且分数不与 2 队混在一起。
    #[test]
    fn three_team_report_has_its_own_system() {
        let results = vec![
            mk(0, vec![2, 1, 0], vec![2, 1, 0], vec![0, 1, 2], Some(0),
               &["greedy_flag", "defender", "random"]),
            mk(1, vec![0, 3, 3], vec![0, 1, 1], vec![2, 0, 0], None,
               &["greedy_flag", "defender", "random"]),
        ];
        let config = ScoringConfig::for_teams(3).expect("3 队");
        let report = aggregate(&results, 3, 1, 1, &config);
        assert_eq!(report.teams, 3);
        assert_eq!(report.config.teams, 3);
        assert_eq!(report.by_slot.len(), 3);
        assert_eq!(report.by_ai_name.len(), 3);
        // 局 2 是 0:3:3 并列最高 → 两个 3 分队平局，greedy_flag 负。
        let greedy = report
            .by_ai_name
            .iter()
            .find(|a| a.ai_name == "greedy_flag")
            .expect("存在");
        assert_eq!((greedy.wins, greedy.draws, greedy.losses), (1, 0, 1));
        let defender = report
            .by_ai_name
            .iter()
            .find(|a| a.ai_name == "defender")
            .expect("存在");
        // 局 1 负于 greedy_flag，局 2 与 random 并列最高分平局。
        assert_eq!((defender.wins, defender.draws, defender.losses), (0, 1, 1));
    }

    /// 零得分局（0:0）不会产生 NaN，占比按 0.5 处理。
    #[test]
    fn zero_score_match_uses_half_share_and_no_nan() {
        let results = vec![mk(0, vec![0, 0], vec![0, 0], vec![0, 0], None, &["a", "b"])];
        let report = aggregate(&results, 2, 1, 1, &ScoringConfig::default());
        for ai in &report.by_ai_name {
            assert!(ai.rating.is_finite(), "{} rating 非有限", ai.ai_name);
            assert!(ai.strength.is_finite());
            assert_eq!(ai.draws, 1);
        }
    }

    /// 空输入：不 panic，返回空报告但保留体系标注。
    #[test]
    fn empty_results_produce_empty_report() {
        let report = aggregate(&[], 2, 1, 1, &ScoringConfig::default());
        assert_eq!(report.total_matches, 0);
        assert!(report.by_ai_name.is_empty());
        assert_eq!(report.by_slot.len(), 2, "槽位榜按体系给满，便于 UI 占位");
        assert_eq!(report.by_slot[0].matches, 0);
    }

    /// 结果里的队伍数多于声明的体系时：AI 榜全收，槽位榜不越界。
    #[test]
    fn extra_teams_do_not_break_slot_indexing() {
        let results = vec![mk(0, vec![1, 0, 0], vec![1, 0, 0], vec![0, 0, 0], Some(0),
                                &["a", "b", "c"])];
        let report = aggregate(&results, 2, 1, 1, &ScoringConfig::default());
        assert_eq!(report.by_ai_name.len(), 3);
        assert_eq!(report.by_slot.len(), 2);
    }
}
