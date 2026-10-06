//! 单局结果 `MatchResult`：同时用于 `matches.jsonl` 的每一行与回放的 `end` 行。
//!
//! 为什么让两者共用同一个结构：CLI 的 `matches.jsonl` 是「批量统计的输入」，
//! 回放的 `end` 行是「单局复盘的结论」，如果两者字段不一致，就会出现
//! 「批量统计说 A 赢了、回放说平局」这种最难以排查的 bug。
//! 共用结构 + 共用序列化代码可以从根上避免这类不一致。
//!
//! `match_index`（局索引）在回放 `end` 行里是冗余信息（文件名已经能看出来），
//! 但保留它可以让 Web UI 在同一页面加载多份回放时直接显示局号。

use serde::{Deserialize, Serialize};

use crate::types::TeamId;

/// 一局对战的完整结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchResult {
    /// 局索引（0 起，按批量运行的顺序编号，与 `replays/match_00000.jsonl` 对应）。
    pub match_index: u32,
    /// 本局地图种子。用它 + `map_gen_version` 就能复现同一张地图。
    pub seed: u64,
    /// 生成本局地图的地图生成版本（便于统计时排除跨版本混跑的批次）。
    pub map_gen_version: u32,
    /// 本局实际跑完的全局回合数（可能小于 `max_ticks`；提前结束的情况见下）。
    ///
    /// 当前规则不会提前结束（没有「全灭即结束」的默认行为），因此正常情况下等于 `max_ticks`；
    /// 保留该字段是为了将来加入提前结算规则时不破坏回放格式。
    pub ticks: u32,
    /// 各队最终得分，**下标即队伍 ID**。
    pub scores: Vec<i32>,
    /// 各队击杀数（造成最后一击使敌人死亡的次数，含友伤？——见 `sim` 注释：只统计敌方击杀）。
    pub kills: Vec<u32>,
    /// 各队死亡次数（含自炸/友伤导致的死亡）。
    pub deaths: Vec<u32>,
    /// 赢家队伍 ID；`null` 表示平局。
    ///
    /// 并列最高分即为平局（多个队伍都是最高分）。默认不启用「击杀数细分」等决胜规则，
    /// 但 `sim` 的胜负判定留了钩子（见 `sim::WinCondition`）。
    pub winner: Option<TeamId>,
    /// 各队 AI 名字，**下标即队伍 ID**，与 `scores` 顺序一致。
    pub ai_names: Vec<String>,
}

impl MatchResult {
    /// 队伍数（按分数数组长度推断）。
    pub fn team_count(&self) -> usize {
        self.scores.len()
    }

    /// 某队是否获胜（平局时所有队伍都为 `false`）。
    pub fn is_winner(&self, team: TeamId) -> bool {
        self.winner == Some(team)
    }

    /// 该局是否为平局。
    pub fn is_draw(&self) -> bool {
        self.winner.is_none()
    }

    /// 某队 AI 名字（下标越界时返回 `"unknown"`，避免统计代码 panic）。
    pub fn ai_name_of(&self, team: TeamId) -> &str {
        self.ai_names
            .get(team as usize)
            .map(String::as_str)
            .unwrap_or("unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(winner: Option<TeamId>) -> MatchResult {
        MatchResult {
            match_index: 3,
            seed: 123,
            map_gen_version: 1,
            ticks: 300,
            scores: vec![2, 1],
            kills: vec![4, 3],
            deaths: vec![3, 4],
            winner,
            ai_names: vec!["random".into(), "defender".into()],
        }
    }

    #[test]
    fn match_result_json_shape_is_stable() {
        let json = serde_json::to_value(result(Some(0))).expect("序列化");
        assert_eq!(json["match_index"], 3);
        assert_eq!(json["seed"], 123);
        assert_eq!(json["winner"], 0);
        assert_eq!(json["scores"], serde_json::json!([2, 1]));
        let draw = serde_json::to_value(result(None)).expect("序列化");
        assert!(draw["winner"].is_null(), "平局必须序列化为 null");
    }

    #[test]
    fn helpers_do_not_panic_on_short_arrays() {
        let mut r = result(Some(1));
        r.ai_names.clear();
        assert_eq!(r.ai_name_of(0), "unknown");
        assert!(r.is_winner(1));
        assert!(!r.is_draw());
        assert_eq!(r.team_count(), 2);
    }
}
