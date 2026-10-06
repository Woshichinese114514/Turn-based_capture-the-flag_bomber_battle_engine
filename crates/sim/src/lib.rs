//! # sim —— 游戏模拟核心（纯逻辑）
//!
//! `sim` 是本项目的「规则引擎」：它把 [`MatchConfig`] + 一组 [`TeamAi`] 变成一条
//! 确定性的对局，并产出回放（[`ReplayBundle`]）与结果（`protocol::MatchResult`）。
//! 它**不**做渲染、不做网络、不依赖具体 AI（只认识 [`TeamAi`] trait），也不读文件
//! （回放落盘属于 `cli`）。
//!
//! ## 一局的生命周期
//!
//! ```text
//! MatchConfig + Vec<Box<dyn TeamAi>>
//!         │
//!         ├─ 生成地图（mapgen，带版本号）        ← 种子只在这里用一次
//!         ├─ 建初始状态（每队 units_per_team 个单位在己方阵营）
//!         │
//!         ▼
//!   step() × max_ticks   ← 每 tick 9 个阶段（见 engine 模块）
//!         │                （每 tick 结束产出一个 RenderFrame）
//!         ▼
//!   MatchResult（分数 / 击杀 / 死亡 / 胜者）
//! ```
//!
//! ## 确定性（本项目的第一原则）
//!
//! 「同配置跑两次，回放逐字节相同」是硬性验收项（`docs/rules.md` §14 测试 14）。
//! 为此本 crate 遵守三条自我约束：
//!
//! 1. **唯一随机源**：整局只用一个 [`Rng`]，由 `MatchConfig::seed` 播种。
//!    炸弹引爆、旗刷新、掉旗落点都从它取数；绝不使用系统时间、线程本地随机或第二个发生器。
//! 2. **唯一遍历顺序**：所有集合（单位、旗、炸弹、候选格）都按固定顺序遍历
//!    （创建顺序 / 行优先 / ID 升序），且**不**依赖 `HashMap` 的迭代顺序。
//! 3. **无隐藏状态**：`Sim` 的全部可变状态都在 `state` 模块里，没有全局变量或缓存；
//!    [`Rng`] 的推进次数完全由代码路径决定。
//!
//! ## 模块划分
//!
//! * [`ai`]：`TeamAi` trait 与 AI 可见的只读快照（冻结契约，见 `docs/internal-api.md` §3.1）。
//! * `config`：规则参数 [`RulesConfig`]、对局配置 [`MatchConfig`]、错误 [`SimError`]。
//! * `state`：内部实体（单位 / 旗 / 炸弹）与快照导出。
//! * `conflict`：tick 内移动意图的不动点冲突结算（规则 §7）。
//! * `combat`：攻击判定（射程 + 视线）与伤害 / 死亡结算。
//! * `bomb`：炸弹放置、倒计时与十字爆炸（规则 §4）。
//! * `flag`：旗刷新、拾取、掉落与得分（规则 §5、§6）。
//! * `engine`：[`Sim`] 的 9 阶段 tick 流水线与 [`play`] 便捷入口。
//! * `replay`：[`ReplayBundle`] 与 JSONL 序列化。

pub mod ai;

mod bomb;
mod combat;
mod config;
mod conflict;
mod engine;
mod flag;
mod replay;
mod state;

#[cfg(test)]
mod tests;

pub use ai::{Action, MapView, ObsUnit, Observation, TeamActions, TeamAi, UnitCommand};
pub use config::{MatchConfig, RulesConfig, SimError, WinCondition, WinScoreRule};
pub use engine::{play, PlayOutcome, Sim};
pub use mapgen::Rng;
pub use replay::ReplayBundle;

/// 阵营区边长（3×3）。
///
/// 与 `protocol::TeamInfo::base_contains` 里写死的 3、`mapgen::BASE_SIZE` 必须一致。
/// 之所以在三处保留常量而不互相 `use`：阵营尺寸是**协议级**约定（地形编码、得分区、
/// 复活点都依赖它），任何一处改动都意味着协议升级；让它显式重复出现，反而更容易在
/// review 时被同时发现。类型取 `i32`，因为所有坐标运算都在 `i32` 上进行。
pub const BASE_SIZE: i32 = 3;
