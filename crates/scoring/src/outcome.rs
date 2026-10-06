//! 胜/平/负判定口径。
//!
//! `sim` 已经算出了 `MatchResult.winner`，为什么评分层还要再判一次？
//!
//! * **集中分类**：W/D/L 的分类只允许在一个地方出现。将来接 ELO/TrueSkill 时，
//!   「平局按 0.5 分算」「胜负判负但击杀多者也给一点分」这类调整只需换一个
//!   [`OutcomeRule`] 实现，聚合代码一行不用改。
//! * **允许口径与引擎不完全一致**：引擎的 `winner` 是规则层结论；评分层可以有自己
//!   的统计口径（例如未来把「并列最高分」视为双方各 0.5 胜）。默认两者一致。
//! * **防御性**：`winner` 可能因为将来新增规则而为 `None` 或指向越界队伍，
//!   `OutcomeRule` 必须能给出确定的分类而不是 panic。

use protocol::{MatchResult, TeamId};
use serde::{Deserialize, Serialize};

/// 单局对某队而言的结果分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Win,
    Draw,
    Loss,
}

/// 胜/平/负的判定口径（扩展点）。
pub trait OutcomeRule {
    /// 判定 `team` 在 `result` 中的结果。
    ///
    /// 实现**不得 panic**：越界队伍 ID 必须返回 [`Outcome::Loss`]。
    fn outcome(&self, result: &MatchResult, team: TeamId) -> Outcome;
}

/// 默认口径：与 `sim::WinCondition::HighestScore` 语义一致。
///
/// ```text
/// 本队分数 == 最高分 且 最高分唯一  -> Win
/// 本队分数 == 最高分 且 多队并列    -> Draw
/// 否则                              -> Loss
/// ```
///
/// 与 `MatchResult.winner` 的关系：`winner` 已经表达了「唯一最高分」，
/// 这里重新按分数判断是为了让 `OutcomeRule` 成为一个自洽的、可替换的判据
/// （而不是对 `winner` 的转发），同时在 `scores` 与 `winner` 不一致时
/// 以 `scores` 为准——`scores` 是原始数据，`winner` 是派生结论。
#[derive(Clone, Copy, Debug, Default)]
pub struct HighestScoreRule;

impl OutcomeRule for HighestScoreRule {
    fn outcome(&self, result: &MatchResult, team: TeamId) -> Outcome {
        let Some(&my_score) = result.scores.get(team as usize) else {
            // 该队伍在本局没有成绩（数组太短）→ 只能算负，不能 panic。
            return Outcome::Loss;
        };
        let Some(best) = result.scores.iter().copied().max() else {
            // 空分数数组：没有任何队伍有分，按平局处理（不存在赢家）。
            return Outcome::Draw;
        };
        if my_score != best {
            return Outcome::Loss;
        }
        let tied = result.scores.iter().filter(|&&s| s == best).count();
        if tied == 1 {
            Outcome::Win
        } else {
            // 并列最高分 = 平局（与 docs/rules.md 第 8 节一致）。
            Outcome::Draw
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(scores: Vec<i32>, winner: Option<TeamId>) -> MatchResult {
        MatchResult {
            match_index: 0,
            seed: 1,
            map_gen_version: 1,
            ticks: 300,
            kills: vec![0; scores.len()],
            deaths: vec![0; scores.len()],
            ai_names: vec!["a".into(); scores.len()],
            scores,
            winner,
        }
    }

    #[test]
    fn unique_highest_score_wins() {
        let r = HighestScoreRule;
        let m = result(vec![3, 1], Some(0));
        assert_eq!(r.outcome(&m, 0), Outcome::Win);
        assert_eq!(r.outcome(&m, 1), Outcome::Loss);
    }

    #[test]
    fn tied_highest_score_is_draw_for_the_tied() {
        let r = HighestScoreRule;
        // 2 队 2:2 并列 → 双方平局
        let m = result(vec![2, 2], None);
        assert_eq!(r.outcome(&m, 0), Outcome::Draw);
        assert_eq!(r.outcome(&m, 1), Outcome::Draw);
        // 3 队 2:2:1 并列最高 → 0、1 平局，2 负
        let m = result(vec![2, 2, 1], None);
        assert_eq!(r.outcome(&m, 0), Outcome::Draw);
        assert_eq!(r.outcome(&m, 1), Outcome::Draw);
        assert_eq!(r.outcome(&m, 2), Outcome::Loss);
        // 3 队全 0 → 三队并列最高 → 全是平局
        let m = result(vec![0, 0, 0], None);
        assert_eq!(r.outcome(&m, 2), Outcome::Draw);
    }

    #[test]
    fn out_of_range_team_is_loss_not_panic() {
        let r = HighestScoreRule;
        let m = result(vec![1, 0], Some(0));
        assert_eq!(r.outcome(&m, 7), Outcome::Loss);
        // 空分数数组不 panic：队伍 0 没有出现在结果里 → 无成绩，记为负
        // （而不是平局：平局意味着「打平了」，但我们根本没有该队的数据）。
        let empty = result(vec![], None);
        assert_eq!(r.outcome(&empty, 0), Outcome::Loss);
    }

    #[test]
    fn scores_take_precedence_over_derived_winner_field() {
        let r = HighestScoreRule;
        // 故意让 winner 与 scores 矛盾：评分口径以原始 scores 为准。
        let m = result(vec![1, 5], Some(0));
        assert_eq!(r.outcome(&m, 1), Outcome::Win);
        assert_eq!(r.outcome(&m, 0), Outcome::Loss);
    }
}
