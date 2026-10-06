//! 回放三行式结构：`init` / `frame` / `end`。
//!
//! ## 文件格式（JSONL：每行一个 JSON 对象，行尾 `\n`）
//!
//! ```text
//! {"type":"init",  ...}        第 1 行，且只出现一次
//! {"type":"frame", ...}        每个全局 tick 一行，tick 单调递增、逐 1 递增
//! {"type":"frame", ...}
//! ...
//! {"type":"end",   ...}        最后 1 行，且只出现一次
//! ```
//!
//! ## 为什么用 JSONL 而不是一个大 JSON 数组
//!
//! * 顺序流式读取即可播放（不需要先读完整个文件才出第一帧）；
//! * 出错时能定位到具体行号；
//! * 追加写、grep 排查、`head -1` 看 init 都很方便；
//! * 半截文件（例如程序被 kill）仍然能解析出已完整写出的帧，UI 容错更容易。
//!
//! Web UI 侧的实现要求：把整份文件读进内存后按 `tick` 建索引，从而支持
//! O(1) 跳转（进度条拖动、输入 tick 直接跳）。这是选择「每帧全量快照」格式的直接原因。

use serde::{Deserialize, Serialize};

use crate::result::MatchResult;
use crate::types::{Coord, MapInit, TeamId};
use crate::view::RenderFrame;

/// 参赛队伍信息（只在 `init` 行出现）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamInfo {
    /// 队伍 ID（0 起）。
    pub team_id: TeamId,
    /// 该队使用的 AI 名字（注册表里的名字，例如 `"greedy_flag"`）。
    ///
    /// 允许为空字符串（AI 名字是可选的展示信息），但引擎一定会写，UI 缺失时显示「未知」。
    #[serde(default)]
    pub ai_name: String,
    /// 该队 3×3 阵营区的**左上角**坐标（不是中心）。
    ///
    /// 约定左上角是为了让 UI 直接画 `x..x+3, y..y+3` 的矩形，不需要再做 -1/+1 换算。
    /// 阵营格在地形数组里也已经是 `TeamBase(team_id)`，两者必须一致（由 `mapgen` 保证）。
    pub base_x: i32,
    pub base_y: i32,
}

impl TeamInfo {
    /// 阵营区（3×3）是否包含某格。
    ///
    /// 阵营尺寸固定为 3，写成常量而不是可配置项：地形编码与地图生成、得分判定、
    /// 复活点选择都依赖这个尺寸，配置化会引入「地形是 3×3 但规则以为 4×4」的不一致风险。
    pub fn base_contains(&self, x: i32, y: i32) -> bool {
        const BASE_SIZE: i32 = 3;
        x >= self.base_x && x < self.base_x + BASE_SIZE && y >= self.base_y && y < self.base_y + BASE_SIZE
    }

    /// 阵营区中心坐标（UI 画队伍标记用）。
    pub fn base_center(&self) -> Coord {
        const BASE_SIZE: i32 = 3;
        Coord::new(self.base_x + BASE_SIZE / 2, self.base_y + BASE_SIZE / 2)
    }
}

/// 回放的 `init` 行：对局开始时的全部静态信息。
///
/// 「静态」指它在整局内不变：地图、队伍、AI 名字、规则参数快照。
/// 每帧动态信息在 [`RenderFrame`]，最终结果在 [`MatchResult`]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayInit {
    /// 引擎版本：模拟核心逻辑变更时 +1（不改变规则语义的重构可以不加）。
    pub engine_version: u32,
    /// 规则版本：伤害/复活/得分/AP 等规则变更时 +1。跨版本的成绩不可直接比较。
    pub rules_version: u32,
    /// 地图生成版本：地图生成算法变更时 +1。旧回放按旧版本解析或直接拒绝。
    pub map_gen_version: u32,
    /// 地图静态数据（宽高 + 行优先地形数组）。
    pub map: MapInit,
    /// 每队一条（下标即队伍 ID，与 `scores` 的顺序一致）。
    pub teams: Vec<TeamInfo>,
    /// 本局最大全局回合数（胜负上限）。
    pub max_ticks: u32,
    /// 旗刷新检查间隔（每 F 回合检查一次是否补旗）。
    pub flag_spawn_interval: u32,
    /// 中心区域半径：旗只会在「到地图中心曼哈顿距离 ≤ 该值」的格子里刷新。
    pub center_radius: u8,
    /// 本局地图种子（`u64`）。
    ///
    /// 契约要求 `end` 行必须带种子；`init` 也带上是为了 UI 在播放中就能显示/复制种子，
    /// 且便于「同种子复现」。UI 应以 `end.seed` 为准，若缺失则回退到 `init.seed`。
    #[serde(default)]
    pub seed: u64,
}

/// 回放行：三行式枚举。
///
/// 用 `#[serde(tag = "type")]` 内部标签：JSON 里靠 `"type"` 字段区分
/// `"init"` / `"frame"` / `"end"`。
///
/// 允许附加未知字段（不做 `deny_unknown_fields`）：这样引擎侧新增可选字段时，
/// 旧版 Web UI 仍能解析（前向兼容），符合「按版本号决定解析策略」的约定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplayLine {
    /// 第 1 行。
    Init(ReplayInit),
    /// 每个 tick 一行。
    Frame(RenderFrame),
    /// 最后 1 行。
    End(MatchResult),
}

impl ReplayLine {
    /// 该行对应的 `type` 字符串，与 JSON 字段一致。
    pub const fn type_name(&self) -> &'static str {
        match self {
            ReplayLine::Init(_) => "init",
            ReplayLine::Frame(_) => "frame",
            ReplayLine::End(_) => "end",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Terrain;

    fn demo_init() -> ReplayInit {
        ReplayInit {
            engine_version: 1,
            rules_version: 1,
            map_gen_version: 1,
            map: MapInit {
                width: 2,
                height: 2,
                map_gen_version: 1,
                terrain: vec![Terrain::TeamBase(0), Terrain::Empty, Terrain::Wall, Terrain::Void],
            },
            teams: vec![
                TeamInfo {
                    team_id: 0,
                    ai_name: "random".into(),
                    base_x: 0,
                    base_y: 0,
                },
                TeamInfo {
                    team_id: 1,
                    ai_name: "defender".into(),
                    base_x: 0,
                    base_y: 0,
                },
            ],
            max_ticks: 300,
            flag_spawn_interval: 5,
            center_radius: 4,
            seed: 42,
        }
    }

    #[test]
    fn replay_line_uses_type_tag() {
        let line = ReplayLine::Init(demo_init());
        let json = serde_json::to_string(&line).expect("序列化");
        assert!(json.starts_with("{\"type\":\"init\""), "init 行：{json}");
        let back: ReplayLine = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(back, line);

        let frame = ReplayLine::Frame(RenderFrame {
            tick: 1,
            scores: vec![0, 0],
            units: vec![],
            flags: vec![],
            bombs: vec![],
            events: vec![],
        });
        assert!(serde_json::to_string(&frame)
            .expect("序列化")
            .contains("\"type\":\"frame\""));
    }

    #[test]
    fn base_contains_uses_top_left_corner() {
        let team = TeamInfo {
            team_id: 0,
            ai_name: "x".into(),
            base_x: 1,
            base_y: 2,
        };
        assert!(team.base_contains(1, 2), "左上角属于阵营");
        assert!(team.base_contains(3, 4), "右下角属于阵营");
        assert!(!team.base_contains(4, 4), "阵营右侧一格不属于阵营");
        assert_eq!(team.base_center(), Coord::new(2, 3));
    }

    #[test]
    fn init_accepts_missing_optional_seed() {
        // 前向/后向兼容：旧版 init 行没有 seed 字段时不应解析失败。
        let mut value = serde_json::to_value(ReplayLine::Init(demo_init())).expect("to_value");
        value.as_object_mut().expect("object").remove("seed");
        let back: ReplayLine = serde_json::from_value(value).expect("缺少 seed 也要能解析");
        match back {
            ReplayLine::Init(init) => assert_eq!(init.seed, 0, "缺失字段兜底为 0"),
            other => panic!("应为 init 行，得到 {other:?}"),
        }
    }
}
