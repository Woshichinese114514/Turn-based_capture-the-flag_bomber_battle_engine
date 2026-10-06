//! # scoring —— 把批量对局结果聚合成「log scale 实力分」
//!
//! 本 crate 只依赖 `protocol`（数据契约），不依赖 `sim` / `ai` / `cli`：
//! 输入是若干条 [`protocol::MatchResult`]，输出是一份 [`ScoringReport`]。
//! 这样评分逻辑可以脱离引擎单独测试，也可以在 Web 后端 / 离线脚本里复用。
//!
//! ## 一条数据流水线
//!
//! ```text
//! MatchResult[]                       // 每局结果（协议结构）
//!   └─ OutcomeRule::outcome()         // 胜/平/负分类（可替换口径）
//!   └─ 原始指标归一化                  // win_rate / avg_score_share / avg_kill_ratio ∈ [0,1]
//!   └─ shrink_win_rate()              // 小样本收缩（避免 3 胜 0 负 = 无限强）
//!   └─ compute_strength()             // 加权综合 → strength ∈ [min,max]
//!   └─ compute_rating()               // odds = s/(1-s); rating = 1000 + 500·log10(odds)
//! ```
//!
//! ## 为什么是 log scale（每 500 分 ≈ 10 倍实力）
//!
//! 需求要求「分数每高约 500 分，实力大约强 10 倍」。
//! 把 `strength` 解释为「对平均水准对手的期望胜率」，则
//! `odds = strength / (1 - strength)` 就是「强多少倍」：
//! `strength = 0.5 → odds = 1`（势均力敌）、`0.9 → odds = 9`（强 9 倍）。
//! 取以 10 为底的对数并乘 500：
//!
//! ```text
//! rating = 1000 + 500 · log10(strength / (1 - strength))
//! ```
//!
//! 于是 `log10` 每 +1（即 odds ×10）就恰好 +500 分：
//!
//! | strength | odds | rating |
//! |---|---|---|
//! | 0.5 | 1 | 1000（基准） |
//! | 10/11 | 10 | 1500（强 10 倍） |
//! | 100/101 | 100 | 2000（强 100 倍） |
//! | 1/11 | 1/10 | 500（弱 10 倍） |
//!
//! 分数可正可负（`strength < 0.5` 时为负），符合需求里「输出一个实数分数」。
//!
//! ## 为什么 2 队与 3 队是两套不可比较的体系
//!
//! `strength` 的参照系是「平均水平的对手」，而「平均」随队伍数变化：
//!
//! * 2 队局：平均对手 = 1 个对手，0.5 胜率 = 与对手五五开；
//! * 3 队局：平均对手 = 2 个对手，0.5 胜率 = 每局能和另外两队之和掰手腕，
//!   且 3 队局更容易出现「并列最高分 → 平局」。
//!
//! 同样是 0.5，两者对应的博弈强度完全不同，因此**禁止跨体系比较**：
//! [`ScoringReport::teams`] 标注该报告属于哪个体系；
//! 2 队体系作为「实力基准」（在 `rules_version` / `map_gen_version` 相同的前提下可跨批次比较），
//! 3 队体系只在同一 `teams` 值内比较。

pub mod aggregate;
pub mod config;
pub mod error;
pub mod outcome;
pub mod rating;

pub use aggregate::{aggregate, AiAggregate, ScoringReport, SlotAggregate};
pub use config::ScoringConfig;
pub use error::ScoringError;
pub use outcome::{HighestScoreRule, Outcome, OutcomeRule};
pub use rating::{compute_rating, compute_strength, shrink_win_rate};
