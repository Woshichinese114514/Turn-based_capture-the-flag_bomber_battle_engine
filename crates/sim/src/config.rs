//! 规则参数、对局配置与错误类型。
//!
//! 这里的所有数值都是**规则真源**（`docs/rules.md`）在代码里的落点：引擎任何地方
//! 需要「每队几个单位」「炸弹 fuse 多长」这类常数时，都必须从 [`RulesConfig`] 读，
//! 不允许在结算代码里写裸数字。这样做的原因有两个：
//!
//! 1. 规则文档与代码的对应关系可以逐字段核对（review 时只看这一个文件）；
//! 2. 测试可以构造「非默认规则」来验证边界（例如 `respawn_ticks = 0` 立刻复活）。
//!
//! [`MatchConfig`] 则描述「这一局」的输入：地图种子、队伍数、tick 上限、胜者判定。
//! 它**不含** AI 实例本身（AI 由 `Sim::new` 单独注入），因此可以被序列化、
//! 被 CLI 打印、被 scoring 汇总。

use mapgen::MapGenError;
use protocol::TeamId;

/// 一局对局的规则参数（默认值即 `docs/rules.md` 的标准规则）。
///
/// 字段全部 `pub`：`cli` 会按命令行参数改写它们（例如 `--max-flags`），
/// 引擎则只读取。构造默认值请用 `RulesConfig::default()`，不要在业务代码里手写
/// 结构体字面量——那样会在规则升级时漏掉新字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RulesConfig {
    /// 每队单位数（标准 3）。
    pub units_per_team: u8,
    /// 单位最大 HP（标准 3，无回血）。
    pub unit_max_hp: u8,
    /// 死亡后复活需要的 tick 数（标准 10）。
    pub respawn_ticks: u32,
    /// 每单位每 tick 的行动点（标准 2）。
    pub ap_per_unit: u8,
    /// 攻击射程（曼哈顿距离上限，标准 3）。
    pub attack_range: i32,
    /// 攻击伤害（标准 1）。
    pub attack_damage: u8,
    /// 炸弹引信 tick 数（标准 2：放置当回合计数 2，之后每 tick 减 1，减到 0 时爆炸）。
    pub bomb_fuse: u8,
    /// 炸弹十字爆炸半径（标准 2，被墙阻挡）。
    pub bomb_radius: u8,
    /// 炸弹伤害（标准 2，可友伤；阵营格免疫）。
    pub bomb_damage: u8,
    /// 旗刷新间隔（标准 5：第 t tick 结算末尾 `t % 5 == 0` 且 `t >= 5` 时检查）。
    pub flag_spawn_interval: u32,
    /// 中心区域半径（标准 4）：也是旗的刷新区与得分判定用的「中心区域」。
    pub flag_capture_radius: u8,
    /// 阵营区边长（标准 3，必须与 `protocol`/`mapgen` 的约定一致）。
    pub base_size: u8,
    /// 携带旗的单位能否攻击（标准 `false`：旗手只能移动/放炸弹/拾旗）。
    pub flag_carrier_can_attack: bool,
    /// 场上旗数上限；`0` 表示「自动 = 队伍数」。
    pub max_flags: u8,
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            units_per_team: 3,
            unit_max_hp: 3,
            respawn_ticks: 10,
            ap_per_unit: 2,
            attack_range: 3,
            attack_damage: 1,
            bomb_fuse: 2,
            bomb_radius: 2,
            bomb_damage: 2,
            flag_spawn_interval: 5,
            flag_capture_radius: 4,
            base_size: 3,
            flag_carrier_can_attack: false,
            // 0 = 自动（等于队伍数）：默认规则下 2 队场上最多 2 面旗。
            max_flags: 0,
        }
    }
}

impl RulesConfig {
    /// 场上旗数上限：`max_flags == 0` 时返回队伍数。
    ///
    /// 之所以用「0 = 自动」而不是直接在 `Default` 里写 2，是因为默认规则要求
    /// 上限跟随队伍数（3 队时是 3 面旗），而 `RulesConfig` 本身不知道队伍数。
    pub fn max_flags_for(&self, teams: u8) -> usize {
        if self.max_flags == 0 {
            teams as usize
        } else {
            self.max_flags as usize
        }
    }

    /// 阵营区边长（`i32` 便于坐标运算）。
    pub fn base_size_i32(&self) -> i32 {
        self.base_size as i32
    }
}

/// 胜者判定方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinCondition {
    /// 只比分数：最高分唯一则胜，并列最高则平局。
    HighestScore,
    /// 先比分数；并列最高时再比击杀数；仍并列则平局。
    HighestScoreThenKills,
}

/// 可替换的胜负判定规则（保留给扩展：目前与 [`WinCondition`] 同构）。
///
/// 默认 [`WinScoreRule::HighestScore`]。单独留一个类型是为了让后续新增判定
/// （例如「先到 N 分即胜」）不必改动 [`WinCondition`] 这个已冻结的枚举。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WinScoreRule {
    /// 最高分获胜。
    #[default]
    HighestScore,
    /// 最高分并列时比击杀数。
    HighestScoreThenKills,
}

/// 一局对局的输入配置（不含 AI 实例）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchConfig {
    /// 对局序号（批量跑分时用于定位，写进 `MatchResult`）。
    pub match_index: u32,
    /// 地图种子：地图生成、以及 sim 的唯一 RNG 都用它。
    pub seed: u64,
    /// 每队 AI 的随机种子（下标即队伍），由 CLI 按种子计划算出。
    pub ai_seeds: Vec<u64>,
    /// 每队 AI 的名字（写进回放 `init` 行与结果）。
    pub ai_names: Vec<String>,
    /// 队伍数（2 或 3；更多队伍是协议预留，规则尚未定义）。
    pub teams: u8,
    /// 地图宽（格）。
    pub width: u16,
    /// 地图高（格）。
    pub height: u16,
    /// tick 上限（标准 300）：达到即结束。
    pub max_ticks: u32,
    /// 规则参数。
    pub rules: RulesConfig,
    /// 地图生成算法版本（写进回放；不同版本地形不同）。
    pub map_gen_version: u32,
    /// 胜者判定方式。
    pub win_condition: WinCondition,
    /// 可选规则：全灭判负（默认 `false`）。为真时，只剩一支队伍有存活单位即结束。
    pub all_dead_loses: bool,
}

impl MatchConfig {
    /// 构造一局的默认配置：25×25、`max_ticks = 300`、标准规则、最新地图版本。
    ///
    /// `ai_seeds` 由 `seed` 推导（每队不同），保证「同一种子 → 同一批 AI 种子」，
    /// 这是可复现性的前提：CLI 不需要额外传种子，只需传地图种子。
    /// 推导公式与 `ai_salt` 的约定一致：`seed ^ (team << 32) ^ 0x9E37_79B9`。
    pub fn new(teams: u8, seed: u64) -> Self {
        let ai_seeds = (0..teams)
            .map(|team| seed ^ ((team as u64) << 32) ^ 0x9E37_79B9)
            .collect();
        let ai_names = (0..teams).map(|team| format!("ai{team}")).collect();
        Self {
            match_index: 0,
            seed,
            ai_seeds,
            ai_names,
            teams,
            width: 25,
            height: 25,
            max_ticks: 300,
            rules: RulesConfig::default(),
            map_gen_version: mapgen::MAP_GEN_VERSION,
            win_condition: WinCondition::HighestScore,
            all_dead_loses: false,
        }
    }

    /// 取某队的 AI 种子；越界返回 0（不 panic：AI 数量不匹配由 `Sim::new` 负责报错）。
    pub fn ai_seed_for(&self, team: TeamId) -> u64 {
        self.ai_seeds.get(team as usize).copied().unwrap_or(0)
    }
}

/// 构造 / 启动一局对局时的错误。
///
/// 全部是「配置错误」，与规则结算无关：结算期不会失败（非法动作只会生成事件）。
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    /// 队伍数不是 2 或 3。
    #[error("队伍数必须是 2 或 3，得到 {0}")]
    BadTeamCount(u8),
    /// 传入的 AI 实例数与队伍数不一致。
    #[error("AI 实例数量 {got} 与队伍数 {expected} 不一致")]
    AiCountMismatch { got: usize, expected: usize },
    /// 地图生成失败（尺寸过小 / 密度非法 / 连通性修复失败等）。
    #[error("地图生成失败：{0}")]
    MapGen(#[from] MapGenError),
    /// 请求了不支持的地图生成版本。
    #[error("不支持的地图生成版本 {0}")]
    UnsupportedMapGenVersion(u32),
    /// 生成出的地图数据自校验失败（地形长度、阵营区地形等）。
    #[error("地图数据非法：{0}")]
    BadMap(String),
}
