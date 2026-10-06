//! `sim::ai`：AI 与引擎之间的**唯一**接口层。
//!
//! ## 为什么把 trait 放在 `sim` 而不是独立的 `ai` crate
//!
//! 依赖方向是 `cli -> ai -> sim -> mapgen -> protocol`（见 `docs/internal-api.md` §0）。
//! 如果把 `TeamAi` trait 放在 `ai` crate 里，`sim` 就必须反依赖 `ai`，形成环：
//! `sim` 需要 `TeamAi` 才能被 `Sim::new` 注入，而 `ai` 的实现又需要 `sim` 的
//! `Observation`/`Rng`。把「接口 + 只读观测数据」下沉到 `sim`，让 `ai` 只实现 trait，
//! 环就断开了。代价是 `sim` 里出现了「给 AI 用的类型」，但它们是**纯数据**，
//! 不含任何决策逻辑，因此不会污染模拟核心的纯粹性。
//!
//! ## 信息边界：AI 只能看见 [`Observation`]
//!
//! [`Observation`] 是决策瞬间的**完整快照**（自带全部实体视图的拷贝），不是指向
//! 引擎内部状态的引用。这样设计有三个直接好处：
//!
//! 1. AI 不可能通过观测对象读到 `sim` 的内部可变状态，也就不会在结算中途「偷看未来」；
//! 2. 一队 AI 的 `decide` 拿到快照后可以随意保存/比较，不会因为其他队同时结算而失效；
//! 3. 快照字段与回放 `RenderFrame` 的语义一致（结算后状态），AI 开发者可以直接用回放调试 AI。
//!
//! 注意 `units`/`flags`/`bombs` 里装的是协议视图（[`UnitView`] 等）而不是内部实体结构：
//! 内部字段（AP、携带关系、炸弹所属）一旦泄漏给 AI，日后重构引擎就会破坏 AI 的 ABI。
//! 唯一需要额外信息的是「本回合还剩多少 AP」与「是否已攻击」，规则要求 AI 知道它们
//! 才能规划（见 `docs/rules.md` 2.1），所以 [`ObsUnit`] 在视图之上补了这两个字段。

use protocol::{BombView, Coord, Direction, EntityId, FlagView, TeamId, Terrain, UnitView};

/// 一支队伍的 AI。
///
/// 引擎只通过 `decide` 向 AI 索要动作，**不关心** AI 内部怎么想；同一 tick 内每队
/// 恰好调用一次（顺序见 `docs/rules.md` 第 9 节的轮换规则）。
///
/// 实现者必须满足的三条纪律（`docs/rules.md` §13）：
/// * 不得 panic（引擎不捕 panic，一次 AI 崩溃会毁掉整批对局）；
/// * 只能通过 [`Observation`] 获取信息（不得访问文件/网络/全局状态）；
/// * 未涉及的动作不提交即可（引擎把缺席单位视为等待）。
pub trait TeamAi {
    /// AI 名字：写进回放 `init` 行与 `MatchResult::ai_names`，用于统计与复盘。
    ///
    /// 返回 `&str` 而不是 `String` 是为了让实现可以直接返回字面量（零分配）。
    fn name(&self) -> &str;

    /// 决策：返回本队本 tick 的动作列表。
    ///
    /// 允许返回空 [`TeamActions`]（全队等待）。引擎会逐条校验动作合法性，
    /// 非法动作按等待处理并生成 `illegal_action` 事件——**不会**让 AI 的 bug 导致对局崩溃。
    fn decide(&mut self, obs: &Observation) -> TeamActions;
}

/// AI 可见的地图快照。
///
/// 字段与 `mapgen::MapData` 对齐，但刻意**不**依赖 `mapgen`（只用 `protocol` 类型）。
/// 这样 AI crate 不必同时依赖 `mapgen`，也保证 AI 无法拿到 `MapData` 上那些
/// 「地图生成期才需要」的方法（例如 `to_map_init`），避免 AI 依赖生成细节。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapView {
    pub width: u16,
    pub height: u16,
    /// 行优先地形数组，`idx = y * width + x`。
    pub terrain: Vec<Terrain>,
    /// 地图中心格（旗刷新区域的圆心）。
    pub center: Coord,
    /// 中心区域半径：旗只在 `manhattan(center) <= center_radius` 的格子出现。
    pub center_radius: u8,
    /// 各队 3×3 阵营区左上角，下标即队伍 ID。
    pub bases: Vec<Coord>,
}

impl MapView {
    /// 从 `mapgen::MapData` 构造（`sim` 内部使用；AI 侧只需读取字段）。
    pub fn from_map_data(map: &mapgen::MapData) -> Self {
        Self {
            width: map.width,
            height: map.height,
            terrain: map.terrain.clone(),
            center: map.center,
            center_radius: map.center_radius,
            bases: map.bases.clone(),
        }
    }

    /// 行优先下标换算；越界返回 `None`。
    pub fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        Some(y as usize * self.width as usize + x as usize)
    }

    /// 坐标是否在地图内。
    pub fn in_bounds(&self, x: i32, y: i32) -> bool {
        self.index(x, y).is_some()
    }

    /// 取地形；越界返回 `None`（不 panic）。
    pub fn terrain_at(&self, x: i32, y: i32) -> Option<Terrain> {
        self.index(x, y).and_then(|i| self.terrain.get(i).copied())
    }

    /// 地形层面是否可站人。
    pub fn is_walkable(&self, x: i32, y: i32) -> bool {
        self.terrain_at(x, y).is_some_and(Terrain::is_walkable)
    }

    /// 地形层面是否阻挡视线（越界视为阻挡）。
    pub fn blocks_sight(&self, x: i32, y: i32) -> bool {
        self.terrain_at(x, y).is_none_or(Terrain::blocks_sight)
    }

    /// 某格是否在指定队伍的 3×3 阵营区内。
    pub fn in_base(&self, team: TeamId, x: i32, y: i32) -> bool {
        self.bases.get(team as usize).is_some_and(|b| {
            x >= b.x && x < b.x + crate::BASE_SIZE && y >= b.y && y < b.y + crate::BASE_SIZE
        })
    }

    /// 某格属于哪个队伍的阵营区。
    pub fn in_any_base(&self, x: i32, y: i32) -> Option<TeamId> {
        self.bases
            .iter()
            .position(|b| {
                x >= b.x && x < b.x + crate::BASE_SIZE && y >= b.y && y < b.y + crate::BASE_SIZE
            })
            .map(|i| i as TeamId)
    }

    /// 是否在中心区域内（旗刷新区）。
    pub fn in_center_region(&self, x: i32, y: i32) -> bool {
        self.in_bounds(x, y) && self.distance_to_center(x, y) <= self.center_radius as i32
    }

    /// 到地图中心的曼哈顿距离（不检查越界；越界坐标会得到一个偏大的数，便于当启发值用）。
    pub fn distance_to_center(&self, x: i32, y: i32) -> i32 {
        Coord::new(x, y).manhattan(self.center)
    }
}

/// AI 看到的单位：协议视图 + 两个「规则需要、但视图不导出」的字段。
///
/// 为什么不直接给 `UnitView`：
/// * `ap_left` 决定这个单位**现在还能不能做事**，是 AI 规划的核心输入；
/// * `attacked_this_turn` 虽然 `UnitView` 里也有，但语义上属于「本回合行动状态」，
///   与 `ap_left` 成对使用，放在一起能避免 AI 需要交叉比对两个结构。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObsUnit {
    pub id: EntityId,
    pub team: TeamId,
    pub pos: Coord,
    pub hp: u8,
    pub alive: bool,
    pub respawn_timer: u32,
    pub carrying_flag: Option<EntityId>,
    pub attacked_this_turn: bool,
    /// 本回合剩余 AP（0..=`ap_per_unit`）。
    pub ap_left: u8,
}

impl ObsUnit {
    /// 本回合能否发起攻击。
    ///
    /// 三个条件缺一不可（`docs/rules.md` 2.3 / 3.1）：
    /// 存活、还有 AP、且本回合**尚未攻击过**（每单位每回合至多攻击 1 次）。
    /// 距离与视线不在这个函数里判断：它们取决于目标是谁，属于 `sim` 的结算职责，
    /// 但 AI 可以用 `MapView` + `Coord::manhattan` 自行预估（射程 3、仅墙挡视线）。
    pub fn can_attack(&self) -> bool {
        self.alive && self.ap_left > 0 && !self.attacked_this_turn
    }

    /// 从协议视图 + 内部行动状态构造。
    pub fn from_view(view: &UnitView, ap_left: u8) -> Self {
        Self {
            id: view.id,
            team: view.team,
            pos: view.pos,
            hp: view.hp,
            alive: view.alive,
            respawn_timer: view.respawn_timer,
            carrying_flag: view.carrying_flag,
            attacked_this_turn: view.attacked_this_turn,
            ap_left,
        }
    }
}

/// 一队 AI 在某个 tick 看到的全部信息。
///
/// 字段全部是快照（owned），`map` 是深拷贝：25×25 只有 625 个 `Terrain`（1 字节枚举级别），
/// 对每队每 tick 拷一次完全可接受；换来的是「AI 不可能隔着一层引用读到正在变化的引擎状态」
/// 这一强保证。若将来地图变大到拷贝成为瓶颈，再考虑 `Arc<MapData>` 共享。
#[derive(Clone, Debug)]
pub struct Observation {
    /// 当前 tick（从 1 开始）。
    pub tick: u32,
    /// 本局最大 tick 数（AI 可据此决定末期是否冒险抢分）。
    pub max_ticks: u32,
    /// 本 observation 的视角队伍（决策者自己的队伍 ID）。
    pub team: TeamId,
    /// 地图快照。
    pub map: MapView,
    /// 各队当前分数，下标即队伍 ID。
    pub scores: Vec<i32>,
    /// 全图单位（含死亡单位，便于 AI 计算「敌方多久后复活」）。
    pub units: Vec<ObsUnit>,
    /// 场上所有旗（含被携带的）。
    pub flags: Vec<FlagView>,
    /// 场上所有炸弹。
    pub bombs: Vec<BombView>,
    /// 自己队伍的单位 ID，**升序**。
    ///
    /// 单独给一份的理由：AI 最常见的循环是「遍历我的单位」，而 `units` 里混着敌人与尸体；
    /// 升序则保证 AI 在同等条件下的决策顺序稳定（避免因遍历顺序不同导致同种子不同结果）。
    pub my_units: Vec<EntityId>,
}

impl Observation {
    /// 按 ID 找单位（含死亡单位）。
    pub fn me(&self, unit: EntityId) -> Option<&ObsUnit> {
        self.units.iter().find(|u| u.id == unit)
    }

    /// 敌对且**存活**的单位。
    ///
    /// 死亡单位被过滤掉：规则规定死亡单位不在地图上（不可被攻击、不阻挡移动），
    /// 让 AI 从一开始就看不到它们可以少写一类判断。需要预判复活时间时用
    /// `units` 全量列表自行筛选。
    pub fn enemies(&self) -> impl Iterator<Item = &ObsUnit> + '_ {
        let team = self.team;
        self.units
            .iter()
            .filter(move |u| u.team != team && u.alive)
    }

    /// 自己队伍的全部单位（含存活与死亡）。
    pub fn teammates(&self) -> impl Iterator<Item = &ObsUnit> + '_ {
        let team = self.team;
        self.units.iter().filter(move |u| u.team == team)
    }

    /// 掉落在地上的旗（`carrier == None`）。
    ///
    /// 名字沿用契约（`enemy_flags_on_ground`），语义是「可被任何人拾取的旗」——
    /// 规则里旗没有归属，任何队（含原持有队）都能捡（`docs/rules.md` 5.3）。
    pub fn enemy_flags_on_ground(&self) -> impl Iterator<Item = &FlagView> + '_ {
        self.flags.iter().filter(|f| f.carrier.is_none())
    }
}

/// 一条单位指令：把「哪个单位」和「做什么」绑在一起。
///
/// 引擎按 `unit` 找实体，所以 AI 不需要关心实体在 `sim` 内部的下标；ID 在整局内稳定，
/// 死亡复活后也不变，因此 AI 可以放心跨 tick 记住 ID。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitCommand {
    pub unit: EntityId,
    pub action: Action,
}

/// 单位可提交的动作。
///
/// 只覆盖规则里存在的五个动作；「等待」是显式动作（便于 AI 表达「我故意不动」，
/// 也让非法动作日志更清晰），但引擎把「没有指令的单位」同样视为等待。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// 向相邻格移动一格（1 AP）。
    Move(Direction),
    /// 攻击某个敌人（1 AP，需射程 3 内且中间无墙）。
    Attack(EntityId),
    /// 在自己当前格放置炸弹（1 AP，阵营格内禁止）。
    PlaceBomb,
    /// 拾取当前格上的旗（1 AP）。
    PickFlag,
    /// 等待（0 AP）。
    Wait,
}

impl Action {
    /// 人类可读标签，用于 `illegal_action` 事件的 `action` 字段。
    ///
    /// 格式与 `docs/replay-format.md` / `samples/demo_2p.jsonl` 保持一致：
    /// `move(up)` / `attack(3)` / `place_bomb` / `pick_flag` / `wait`。
    /// 之所以用 `String` 而不是 `&'static str`：`attack` 需要内插目标 ID，
    /// 无法静态分配；这条路径只在**非法**动作上走，性能不敏感。
    pub fn label(&self) -> String {
        match self {
            Action::Move(dir) => format!("move({})", dir.as_str()),
            Action::Attack(target) => format!("attack({target})"),
            Action::PlaceBomb => "place_bomb".to_string(),
            Action::PickFlag => "pick_flag".to_string(),
            Action::Wait => "wait".to_string(),
        }
    }
}

/// 一支队伍一个 tick 提交的动作集合。
///
/// 提供链式构造方法（`move_unit`/`attack`/…）是为了让 AI 实现写得短：
/// 它们都返回 `&mut Self`，可以 `actions.move_unit(a).attack(b, c);` 连着写。
///
/// **不去重**：同一单位提交多条指令是允许的（例如先移动再放炸弹），
/// 由引擎按规则顺序结算并在 AP/条件不足时报非法动作。
/// 若在这里去重，AI 就失去了「先移动再攻击」的表达能力。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamActions {
    pub commands: Vec<UnitCommand>,
}

impl TeamActions {
    /// 空动作集（= 全队等待）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加任意动作。
    pub fn push(&mut self, unit: EntityId, action: Action) -> &mut Self {
        self.commands.push(UnitCommand { unit, action });
        self
    }

    /// 向某方向移动一格。
    pub fn move_unit(&mut self, unit: EntityId, dir: Direction) -> &mut Self {
        self.push(unit, Action::Move(dir))
    }

    /// 攻击目标。
    pub fn attack(&mut self, unit: EntityId, target: EntityId) -> &mut Self {
        self.push(unit, Action::Attack(target))
    }

    /// 放炸弹。
    pub fn place_bomb(&mut self, unit: EntityId) -> &mut Self {
        self.push(unit, Action::PlaceBomb)
    }

    /// 拾旗。
    pub fn pick_flag(&mut self, unit: EntityId) -> &mut Self {
        self.push(unit, Action::PickFlag)
    }

    /// 等待。
    pub fn wait(&mut self, unit: EntityId) -> &mut Self {
        self.push(unit, Action::Wait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::Terrain;

    fn tiny_map() -> MapView {
        // 3×3 全空地，中心 (1,1)，阵营 (0,0) 与 (1,0) 只用于测试 in_base。
        MapView {
            width: 3,
            height: 3,
            terrain: vec![Terrain::Empty; 9],
            center: Coord::new(1, 1),
            center_radius: 1,
            bases: vec![Coord::new(0, 0)],
        }
    }

    fn unit(id: EntityId, team: TeamId, ap: u8, alive: bool, attacked: bool) -> ObsUnit {
        ObsUnit {
            id,
            team,
            pos: Coord::new(1, 1),
            hp: if alive { 3 } else { 0 },
            alive,
            respawn_timer: if alive { 0 } else { 5 },
            carrying_flag: None,
            attacked_this_turn: attacked,
            ap_left: ap,
        }
    }

    fn obs() -> Observation {
        Observation {
            tick: 3,
            max_ticks: 300,
            team: 0,
            map: tiny_map(),
            scores: vec![1, 2],
            units: vec![
                unit(0, 0, 2, true, false),
                unit(1, 0, 0, true, true),
                unit(2, 0, 1, false, false),
                unit(3, 1, 2, true, false),
                unit(4, 1, 2, false, false),
            ],
            flags: vec![
                FlagView {
                    id: 10,
                    pos: Coord::new(1, 1),
                    carrier: None,
                },
                FlagView {
                    id: 11,
                    pos: Coord::new(2, 2),
                    carrier: Some(3),
                },
            ],
            bombs: vec![],
            my_units: vec![0, 1, 2],
        }
    }

    #[test]
    fn observation_queries_respect_alive_and_team() {
        let o = obs();
        // enemies 只给敌对且存活的：4 号（敌）已死，被过滤。
        let enemies: Vec<EntityId> = o.enemies().map(|u| u.id).collect();
        assert_eq!(enemies, vec![3]);
        // teammates 含死亡单位。
        let mates: Vec<EntityId> = o.teammates().map(|u| u.id).collect();
        assert_eq!(mates, vec![0, 1, 2]);
        // 地上只有 10 号旗。
        let ground: Vec<EntityId> = o.enemy_flags_on_ground().map(|f| f.id).collect();
        assert_eq!(ground, vec![10]);
        assert_eq!(o.me(3).map(|u| u.team), Some(1));
        assert!(o.me(99).is_none());
    }

    #[test]
    fn can_attack_needs_alive_ap_and_not_yet_attacked() {
        assert!(unit(0, 0, 2, true, false).can_attack());
        assert!(!unit(0, 0, 0, true, false).can_attack(), "没 AP 不能攻击");
        assert!(!unit(0, 0, 2, true, true).can_attack(), "本回合已攻击");
        assert!(!unit(0, 0, 2, false, false).can_attack(), "死亡不能攻击");
    }

    #[test]
    fn map_view_helpers() {
        let m = tiny_map();
        assert_eq!(m.index(2, 0), Some(2));
        assert_eq!(m.index(0, 1), Some(3), "行优先");
        assert_eq!(m.index(3, 0), None);
        assert!(m.in_bounds(2, 2));
        assert!(!m.in_bounds(-1, 0));
        assert!(m.in_base(0, 0, 0) && m.in_base(0, 2, 2), "阵营左上角与右下角");
        assert!(!m.in_base(1, 0, 0), "1 队不存在 → 不是阵营格");
        assert_eq!(m.in_any_base(0, 0), Some(0));
        assert_eq!(m.in_any_base(5, 5), None, "越界不属于任何阵营");
        assert!(m.in_center_region(1, 1));
        assert!(!m.in_center_region(0, 0));
        assert_eq!(m.distance_to_center(2, 2), 2);
        assert!(m.is_walkable(1, 1));
        assert!(!m.blocks_sight(1, 1));
        assert!(m.blocks_sight(-5, -5), "越界视为阻挡视线");
    }

    #[test]
    fn action_labels_match_replay_format() {
        assert_eq!(Action::Move(Direction::Up).label(), "move(up)");
        assert_eq!(Action::Move(Direction::Left).label(), "move(left)");
        assert_eq!(Action::Attack(3).label(), "attack(3)");
        assert_eq!(Action::PlaceBomb.label(), "place_bomb");
        assert_eq!(Action::PickFlag.label(), "pick_flag");
        assert_eq!(Action::Wait.label(), "wait");
    }

    #[test]
    fn team_actions_builders_are_chainable() {
        let mut actions = TeamActions::new();
        actions
            .move_unit(0, Direction::Right)
            .attack(0, 3)
            .place_bomb(1)
            .pick_flag(1)
            .wait(2);
        assert_eq!(actions.commands.len(), 5);
        assert_eq!(
            actions.commands[0],
            UnitCommand {
                unit: 0,
                action: Action::Move(Direction::Right)
            }
        );
        assert_eq!(
            actions.commands[1],
            UnitCommand {
                unit: 0,
                action: Action::Attack(3)
            }
        );
        assert_eq!(actions.commands[4].action, Action::Wait);
        assert!(TeamActions::default().commands.is_empty());
    }
}
