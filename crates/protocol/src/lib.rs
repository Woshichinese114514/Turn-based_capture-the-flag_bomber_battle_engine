//! # protocol —— 回放与结果契约（纯数据）
//!
//! 本 crate 是 Rust 引擎与 Web UI 之间**唯一的耦合点**：它只定义 serde 数据结构、
//! 常量，以及少量不改变协议语义的查询辅助函数（坐标换算、地形编解码）。
//! 这里**不允许**出现任何游戏规则判定（伤害、复活、得分、冲突结算都属于 `sim`），
//! 也不允许出现文件 IO（回放读写属于 `cli`）。
//!
//! ## 为什么这样分层
//!
//! * Web UI 只需要照抄本 crate 的字段定义即可解析回放，不需要读引擎代码；
//! * 规则演化时，`sim` 可以随意重写，只要 `ReplayInit` / `RenderFrame` / `MatchResult`
//!   三种结构不变，回放格式就不变；
//! * 回放格式一旦要变，必须同时升 `engine_version` / `rules_version` /
//!   `map_gen_version` 中的相关版本号（见 [`versions`]），双方按版本号决定解析策略。
//!
//! ## JSON 命名约定（务必与 Web UI 对齐）
//!
//! * 所有结构体字段使用 **snake_case**（`carrying_flag`、`respawn_timer`…）。
//! * 事件枚举使用 **内部标签**：`#[serde(tag = "type")]` + snake_case 变体名，
//!   因此一个事件长这样：`{"type":"unit_moved","unit":3,"from_x":1,...}`。
//! * 坐标一律是**网格整数坐标**（x 向右、y 向下，原点在左上角），Web UI 自己乘格子大小。
//!   为了 JSON 紧凑，坐标在结构体里以 `#[serde(flatten)]` 展开成平铺的 `x` / `y` 字段。
//! * 数组型字段的**下标即队伍 ID**：`scores[0]` 是 0 队分数，`kills[2]` 是 2 队击杀数。
//!
//! ## 模块划分
//!
//! * [`types`]：ID、坐标、方向、地形枚举、地图静态数据。
//! * [`view`]：每 tick 的动态实体视图（单位/旗/炸弹）与游戏事件。
//! * [`replay`]：回放三行式结构（init / frame / end）。
//! * [`result`]：单局结果 `MatchResult`（同时用于 `matches.jsonl` 与回放的 end 行）。
//! * [`versions`]：版本号三件套常量与升级约定。

pub mod replay;
pub mod result;
pub mod types;
pub mod versions;
pub mod view;

pub use replay::{ReplayInit, ReplayLine, TeamInfo};
pub use result::MatchResult;
pub use types::{
    Coord, Direction, EntityId, MapInit, TeamId, Terrain, Tick, MAX_TEAMS,
    TERRAIN_CODE_BASE_OFFSET, TERRAIN_CODE_EMPTY, TERRAIN_CODE_VOID, TERRAIN_CODE_WALL,
};
pub use view::{BombView, FlagView, GameEvent, RenderFrame, UnitView};
