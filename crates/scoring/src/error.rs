//! 库层错误类型。
//!
//! `aggregate` 的签名是冻结的（返回 `ScoringReport` 而不是 `Result`），因为它是纯聚合、
//! 对任意输入都能给出报告。但「分数体系」这个约束必须能被调用方在**运行前**检查，
//! 否则 2 队报告与 3 队报告混在一起比较就会静默出错。因此这里提供
//! [`ScoringError`] + [`validate_teams`]，由 CLI 在聚合前调用。

/// 评分层的错误。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScoringError {
    /// 队伍数不是 2 或 3：分数体系只有这两套，其他值无法定义「平均对手」。
    #[error("队伍数必须是 2 或 3（2 队与 3 队是两套不可比较的分数体系），得到 {0}")]
    BadTeamCount(u8),
}

/// 校验队伍数是否为受支持的分数体系。
pub fn validate_teams(teams: u8) -> Result<(), ScoringError> {
    match teams {
        2 | 3 => Ok(()),
        other => Err(ScoringError::BadTeamCount(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_teams_accepts_only_two_or_three() {
        assert!(validate_teams(2).is_ok());
        assert!(validate_teams(3).is_ok());
        assert_eq!(validate_teams(0), Err(ScoringError::BadTeamCount(0)));
        assert_eq!(validate_teams(9), Err(ScoringError::BadTeamCount(9)));
    }
}
