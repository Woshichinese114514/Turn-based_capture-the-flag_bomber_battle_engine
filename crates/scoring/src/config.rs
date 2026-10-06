//! 评分配置：权重、收缩先验、以及 log scale 映射的两个参数。

use serde::{Deserialize, Serialize};

/// 评分参数。**所有权重与映射参数都放在这里**，不散落在聚合代码里，
/// 这样 `manifest.json` 只要序列化一份配置，就能完整复现一份历史报告。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoringConfig {
    /// 胜率权重，默认 0.6。胜率是最直接的实力信号，因此权重最大。
    pub weight_win_rate: f64,
    /// 场均得分占比权重，默认 0.25。它衡量「即使没赢也打得多凶」，
    /// 在局数少、平局多的批处理里比胜率更平滑。
    pub weight_score_share: f64,
    /// 击杀/死亡比权重，默认 0.15。它带来一点「战斗效率」信息，
    /// 但权重最低：炸弹友伤与自杀会让击杀比噪声很大。
    pub weight_kill_ratio: f64,
    /// 胜率小样本收缩的先验局数，默认 5.0。
    ///
    /// 公式 `(wins + 0.5·prior) / (matches + prior)`：相当于「先给每个 AI 记 5 局五五开」。
    /// 没有它的话，`3 胜 0 负` 会得到 win_rate = 1 → 被 clamp 到 0.98 → 1999 分，
    /// 让「运气好」看起来像「实力强」；有了先验，3 胜 0 负只能到 ~1100 分，
    /// 需要几十局才能把分数拉开——这正是统计上健康的行为。
    pub win_rate_prior_matches: f64,
    /// 基准分：`strength == 0.5`（平均水准）对应的分数，默认 1000.0。
    pub baseline_rating: f64,
    /// 每十倍实力对应的分数，默认 500.0（见 crate 文档的公式推导）。
    pub points_per_decade: f64,
    /// `strength` 下限，默认 0.02。避免 `log10(0) = -∞`。
    pub min_strength: f64,
    /// `strength` 上限，默认 0.98。避免 `log10(∞) = +∞`；
    /// 也意味着纯理论最强分约为 `1000 + 500·log10(49) ≈ 1849.5`。
    pub max_strength: f64,
    /// 该报告所属的队伍数体系（2 或 3）。
    ///
    /// **不是「参与队伍数」这么简单**：它同时决定了这份分数能跟谁比较。
    /// 2 队体系是实力基准；3 队体系的分数只允许在 `teams == 3` 的报告之间比较。
    /// 未来若要给 3 队体系换一个 `baseline_rating`，也是通过这个字段分流。
    pub teams: u8,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            weight_win_rate: 0.6,
            weight_score_share: 0.25,
            weight_kill_ratio: 0.15,
            win_rate_prior_matches: 5.0,
            baseline_rating: 1000.0,
            points_per_decade: 500.0,
            min_strength: 0.02,
            max_strength: 0.98,
            teams: 2,
        }
    }
}

impl ScoringConfig {
    /// 按队伍数构造默认配置，并标注分数体系。
    ///
    /// 只提供 2/3 两种合法体系；其他值返回错误而**不是 panic**，
    /// 因为 CLI 需要把它翻译成人类可读的报错。
    pub fn for_teams(teams: u8) -> Result<Self, crate::ScoringError> {
        crate::error::validate_teams(teams)?;
        Ok(Self {
            teams,
            ..Self::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_frozen_contract() {
        let c = ScoringConfig::default();
        // 冻结契约：默认权重 0.6/0.25/0.15、prior 5、baseline 1000、每十倍 500 分。
        assert_eq!(c.weight_win_rate, 0.6);
        assert_eq!(c.weight_score_share, 0.25);
        assert_eq!(c.weight_kill_ratio, 0.15);
        assert_eq!(c.win_rate_prior_matches, 5.0);
        assert_eq!(c.baseline_rating, 1000.0);
        assert_eq!(c.points_per_decade, 500.0);
        assert_eq!(c.min_strength, 0.02);
        assert_eq!(c.max_strength, 0.98);
        assert_eq!(c.teams, 2);
    }

    #[test]
    fn for_teams_rejects_other_team_counts() {
        assert_eq!(ScoringConfig::for_teams(2).expect("2 队合法").teams, 2);
        assert_eq!(ScoringConfig::for_teams(3).expect("3 队合法").teams, 3);
        assert!(ScoringConfig::for_teams(1).is_err());
        assert!(ScoringConfig::for_teams(4).is_err());
    }
}
