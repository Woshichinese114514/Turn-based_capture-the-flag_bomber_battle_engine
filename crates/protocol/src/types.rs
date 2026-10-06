//! 基础类型：ID、坐标、方向、地形枚举、地图静态数据，以及地形编码约定。
//!
//! 本模块只做「数据 + 编解码 + 网格索引换算」，不做任何规则判定。
//! 之所以把地形的 JSON 编码写死在协议里（而不是让每个消费者自己解释），是因为
//! 地形数组在 25×25 的地图上有 625 项，用对象数组（`[{"type":"wall"},…]`）会让
//! init 行膨胀十几倍；用**小整数编码**既紧凑又便于 Web UI 直接查表上色。

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 实体（单位 / 旗帜 / 炸弹）统一 ID 类型。
///
/// 三类实体共用同一 ID 空间由各 crate 自行保证（`sim` 用独立计数器），协议层不做约束：
/// Web UI 只需要把 ID 当作不透明标识符用于查找与高亮。
pub type EntityId = u32;

/// 队伍 ID：2 队对局为 `0..=1`，3 队对局为 `0..=2`。
///
/// 刻意用 `u8` 而不是 `usize`：它会被写进 JSON，且在回放里出现频率极高（每个单位/炸弹/事件都有）。
pub type TeamId = u8;

/// 全局回合数（tick），从 0 或 1 开始由 `sim` 决定（当前实现：首帧 tick = 1）。
pub type Tick = u32;

/// 引擎支持的队伍数上限。
///
/// 规则本身只要求支持 2 队和 3 队；这里留到 4 是为了给「地形编码 3+team_id」留出空间，
/// 同时避免队伍 ID 与地形编码相互踩踏。若要支持更多队伍，必须同时升协议版本。
pub const MAX_TEAMS: u8 = 4;

/// 地形编码：空地（可通行、可放东西、旗可以掉在这里）。
pub const TERRAIN_CODE_EMPTY: u8 = 0;
/// 地形编码：墙（阻挡移动、阻挡视线、阻挡炸弹爆炸传播）。
pub const TERRAIN_CODE_WALL: u8 = 1;
/// 地形编码：虚空（不可通行、不可放置任何东西、旗不会掉在此）。
pub const TERRAIN_CODE_VOID: u8 = 2;
/// 阵营格的编码起点：阵营格实际编码 = `TERRAIN_CODE_BASE_OFFSET + team_id`。
///
/// 这样 2 队地图的阵营格是 3 / 4，3 队是 3 / 4 / 5，Web UI 一眼就能从整数反推队伍归属，
/// 不需要再去查队伍表；代价是队伍数被限制在 `MAX_TEAMS` 以内（规则只需要 3）。
pub const TERRAIN_CODE_BASE_OFFSET: u8 = 3;

/// 网格坐标。`x` 向右增长，`y` 向下增长，原点 `(0,0)` 在左上角。
///
/// 在序列化时通常以 `#[serde(flatten)]` 平铺进外层结构，因此回放 JSON 里看到的是
/// `{"id":1,"team":0,"x":3,"y":4,...}` 而不是嵌套的 `"pos":{"x":3,"y":4}`。
/// 这样做的原因：嵌套对象在几十个实体 × 300 tick 的回放里会显著增加体积，
/// 而且 Web UI 的字段访问（`u.x`）更直接。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Coord {
    pub x: i32,
    pub y: i32,
}

impl Coord {
    /// 构造坐标。
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// 曼哈顿距离。
    ///
    /// 引擎里所有「网格距离」的度量都统一用它：攻击射程（≤3）、旗刷新区域（到中心 ≤R）、
    /// 爆炸十字范围。**不要**在别处引入欧氏距离，否则规则判定与 UI 预览会不一致。
    pub const fn manhattan(self, other: Self) -> i32 {
        (self.x - other.x).abs() + (self.y - other.y).abs()
    }

    /// 沿某个方向走一格（不做边界检查，调用方负责）。
    pub const fn step(self, dir: Direction) -> Self {
        let (dx, dy) = dir.delta();
        Self {
            x: self.x + dx,
            y: self.y + dy,
        }
    }
}

impl fmt::Display for Coord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({},{})", self.x, self.y)
    }
}

/// 单位移动的四个方向（只有上下左右，没有对角线）。
///
/// 序列化为 `"up"|"down"|"left"|"right"`：AI 的动作会被 CLI 记录成人类可读日志，
/// 也会出现在非法动作事件里，字符串比 `0..=3` 更利于排查问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// y 减小。
    Up,
    /// y 增大。
    Down,
    /// x 减小。
    Left,
    /// x 增大。
    Right,
}

impl Direction {
    /// 方向向量 `(dx, dy)`。
    pub const fn delta(self) -> (i32, i32) {
        match self {
            Direction::Up => (0, -1),
            Direction::Down => (0, 1),
            Direction::Left => (-1, 0),
            Direction::Right => (1, 0),
        }
    }

    /// 四个方向，用于 AI 遍历候选动作或 UI 画方向指示器。
    pub const ALL: [Direction; 4] = [
        Direction::Up,
        Direction::Down,
        Direction::Left,
        Direction::Right,
    ];

    /// 人类可读名字，用于事件描述（"向上"/"left"），避免日志里出现 `Up` 这种 Rust 名字。
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::Up => "up",
            Direction::Down => "down",
            Direction::Left => "left",
            Direction::Right => "right",
        }
    }
}

/// 地形类型。
///
/// **JSON 编码是一维数组里的小整数**（见 `MapInit::terrain`）：
/// `0=空`、`1=墙`、`2=虚空`、`3+team_id=阵营`。
/// 反序列化同时接受整数和字符串（`"empty"`/`"wall"`/`"void"`/`"base:1"`/`"team2"`），
/// 这样手写测试夹具（fixture）时不必去数编码表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Terrain {
    /// 空地：可通行、可放炸弹、旗可以刷/掉在这里。
    Empty,
    /// 墙：阻挡移动、阻挡视线、阻挡（挡住）炸弹十字爆炸的继续传播。
    Wall,
    /// 虚空：不可通行、不可放置任何东西；旗不会掉在这里。
    Void,
    /// 阵营格：出生点 / 复活点 / 得分区，携带 `team_id`。
    TeamBase(TeamId),
}

impl Terrain {
    /// 转换为协议编码（数组里实际存储的整数）。
    pub const fn code(self) -> u8 {
        match self {
            Terrain::Empty => TERRAIN_CODE_EMPTY,
            Terrain::Wall => TERRAIN_CODE_WALL,
            Terrain::Void => TERRAIN_CODE_VOID,
            Terrain::TeamBase(team) => TERRAIN_CODE_BASE_OFFSET + team,
        }
    }

    /// 从协议编码还原地形。编码超出范围（例如 `9` 或 `3+MAX_TEAMS`）返回 `None`，
    /// 由调用方决定是报错还是兜底成 `Empty`（Web UI 选择警告 + 兜底，Rust 端选择报错）。
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            TERRAIN_CODE_EMPTY => Some(Terrain::Empty),
            TERRAIN_CODE_WALL => Some(Terrain::Wall),
            TERRAIN_CODE_VOID => Some(Terrain::Void),
            c if c >= TERRAIN_CODE_BASE_OFFSET && c < TERRAIN_CODE_BASE_OFFSET + MAX_TEAMS => {
                Some(Terrain::TeamBase(c - TERRAIN_CODE_BASE_OFFSET))
            }
            _ => None,
        }
    }

    /// 该地形上是否可以站人（空地或任意阵营格）。
    ///
    /// 注意：这只考虑**地形本身**，不考虑「敌方阵营不能进入」「格子上有别的单位」等规则；
    /// 那些属于 `sim` 的判定，协议层故意不管，避免两边逻辑分叉。
    pub const fn is_walkable(self) -> bool {
        matches!(self, Terrain::Empty | Terrain::TeamBase(_))
    }

    /// 该地形是否阻挡视线。
    ///
    /// 规则只规定「墙阻挡视线」；虚空按当前规则不阻挡（虚空是不可通行的地板缺口，
    /// 不是实体障碍）。这个决定写在协议层，是为了让 `sim` 的视线判定和 UI 的
    /// 射程预览用同一份定义。
    pub const fn blocks_sight(self) -> bool {
        matches!(self, Terrain::Wall)
    }

    /// 若这是阵营格，返回所属队伍。
    pub const fn base_team(self) -> Option<TeamId> {
        match self {
            Terrain::TeamBase(t) => Some(t),
            _ => None,
        }
    }
}

impl Serialize for Terrain {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 统一序列化成整数：回放体积敏感，且 Web UI 用整数查表最快。
        serializer.serialize_u8(self.code())
    }
}

impl<'de> Deserialize<'de> for Terrain {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// 宽容的反序列化访问器：整数、负数（错误）、字符串都接受。
        struct TerrainVisitor;

        impl<'de> de::Visitor<'de> for TerrainVisitor {
            type Value = Terrain;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    "地形编码：0=空 1=墙 2=虚空 3+队伍ID=阵营，或字符串 empty/wall/void/base:N/teamN"
                )
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Terrain, E> {
                let code = u8::try_from(v).map_err(|_| {
                    E::invalid_value(de::Unexpected::Unsigned(v), &"0..=255 的地形编码")
                })?;
                Terrain::from_code(code).ok_or_else(|| {
                    E::invalid_value(de::Unexpected::Unsigned(v), &self)
                })
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Terrain, E> {
                if v < 0 {
                    return Err(E::invalid_value(de::Unexpected::Signed(v), &self));
                }
                self.visit_u64(v as u64)
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Terrain, E> {
                parse_terrain_str(v)
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Str(v), &self))
            }
        }

        deserializer.deserialize_any(TerrainVisitor)
    }
}

/// 解析地形的字符串写法（手写夹具与调试日志友好）。
///
/// 接受：`empty`/`none`/`floor`、`wall`、`void`、`base:N`、`team:N`、`team_base:N`
/// （`N` 为队伍 ID）；大小写不敏感。
fn parse_terrain_str(raw: &str) -> Option<Terrain> {
    let s = raw.trim().to_ascii_lowercase();
    match s.as_str() {
        "empty" | "none" | "floor" | "0" => Some(Terrain::Empty),
        "wall" | "1" => Some(Terrain::Wall),
        "void" | "2" => Some(Terrain::Void),
        other => {
            let suffix = other
                .strip_prefix("base:")
                .or_else(|| other.strip_prefix("base"))
                .or_else(|| other.strip_prefix("team:"))
                .or_else(|| other.strip_prefix("team_base:"))
                .or_else(|| other.strip_prefix("team_base"))
                .or_else(|| other.strip_prefix("team"))?;
            let suffix = suffix.trim_start_matches([':', '_', ' ']);
            let team: u8 = suffix.parse().ok()?;
            if team < MAX_TEAMS {
                Some(Terrain::TeamBase(team))
            } else {
                None
            }
        }
    }
}

/// 地图静态数据。
///
/// 只在回放的 `init` 行里出现一次；每个 `frame` 行**不重复**地图，因为地图在对局中不变。
/// Web UI 拿到后按 `width × height` 建索引，之后所有静态绘制（墙、空地、阵营、中心区）
/// 都从这里取。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapInit {
    /// 地图宽度（列数）。
    pub width: u16,
    /// 地图高度（行数）。
    pub height: u16,
    /// 生成这张地图的算法版本号；UI 可据此判断是否需要换解析/渲染策略。
    pub map_gen_version: u32,
    /// **行优先**地形数组，长度必须等于 `width * height`：
    /// 下标 `i = y * width + x`。用行优先是因为它和 Web UI 的 `ImageData`/逐行绘制顺序一致。
    pub terrain: Vec<Terrain>,
}

impl MapInit {
    /// 按行优先规则把 `(x, y)` 换算成数组下标；越界返回 `None`。
    ///
    /// 坐标换算集中在这一个函数里，避免各处各写一遍 `y * width + x`（曾经最容易出错的点）。
    pub fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        Some(y as usize * self.width as usize + x as usize)
    }

    /// 坐标是否在地图范围内。
    pub fn in_bounds(&self, x: i32, y: i32) -> bool {
        self.index(x, y).is_some()
    }

    /// 取某格地形；越界或数组长度不足时返回 `None`（容错，不 panic）。
    pub fn terrain_at(&self, x: i32, y: i32) -> Option<Terrain> {
        self.index(x, y).and_then(|i| self.terrain.get(i).copied())
    }

    /// 该格是否可站立（地形层面，不含单位/阵营规则）。
    pub fn is_walkable(&self, x: i32, y: i32) -> bool {
        self.terrain_at(x, y).is_some_and(Terrain::is_walkable)
    }

    /// 该格是否阻挡视线（地形层面）。越界视为阻挡，防止射线判定泄漏到地图外。
    pub fn blocks_sight(&self, x: i32, y: i32) -> bool {
        self.terrain_at(x, y).is_none_or(Terrain::blocks_sight)
    }

    /// 校验地图数据自洽：长度必须等于 `width * height`，且尺寸非零。
    ///
    /// 返回人类可读的错误说明，供 CLI/解析器报错使用（不要在库层 panic）。
    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            return Err(format!(
                "地图尺寸非法：{}x{}（宽高必须大于 0）",
                self.width, self.height
            ));
        }
        let expected = self.width as usize * self.height as usize;
        if self.terrain.len() != expected {
            return Err(format!(
                "地形数组长度 {} 与 width*height = {} 不一致（行优先：idx = y*width + x）",
                self.terrain.len(),
                expected
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_codes_round_trip() {
        for code in 0u8..TERRAIN_CODE_BASE_OFFSET + MAX_TEAMS {
            let terrain = Terrain::from_code(code).expect("编码在合法范围内");
            assert_eq!(terrain.code(), code, "地形编解码必须可逆");
        }
        assert!(Terrain::from_code(TERRAIN_CODE_BASE_OFFSET + MAX_TEAMS).is_none());
        assert!(Terrain::from_code(200).is_none());
    }

    #[test]
    fn terrain_serde_accepts_int_and_string() {
        let wall: Terrain = serde_json::from_str("1").expect("整数编码");
        assert_eq!(wall, Terrain::Wall);
        let base: Terrain = serde_json::from_str("\"base:2\"").expect("字符串写法");
        assert_eq!(base, Terrain::TeamBase(2));
        let empty: Terrain = serde_json::from_str("\"empty\"").expect("字符串写法");
        assert_eq!(empty, Terrain::Empty);
        assert!(serde_json::from_str::<Terrain>("\"lava\"").is_err());
        assert!(serde_json::from_str::<Terrain>("99").is_err());
    }

    #[test]
    fn coord_index_is_row_major() {
        let map = MapInit {
            width: 3,
            height: 4,
            map_gen_version: 1,
            terrain: vec![Terrain::Empty; 12],
        };
        assert_eq!(map.index(0, 0), Some(0));
        assert_eq!(map.index(2, 0), Some(2));
        assert_eq!(map.index(0, 1), Some(3), "行优先：idx = y*width + x");
        assert_eq!(map.index(2, 3), Some(11));
        assert_eq!(map.index(3, 0), None);
        assert_eq!(map.index(-1, 0), None);
        assert!(map.validate().is_ok());
    }

    #[test]
    fn validate_detects_short_terrain_array() {
        let map = MapInit {
            width: 3,
            height: 3,
            map_gen_version: 1,
            terrain: vec![Terrain::Empty; 8],
        };
        let err = map.validate().expect_err("长度不足必须报错");
        assert!(err.contains("地形数组长度"), "错误信息应可读：{err}");
    }

    #[test]
    fn direction_steps_and_manhattan() {
        let p = Coord::new(5, 5);
        assert_eq!(p.step(Direction::Up), Coord::new(5, 4));
        assert_eq!(p.step(Direction::Right), Coord::new(6, 5));
        assert_eq!(p.manhattan(Coord::new(7, 8)), 5);
    }
}
