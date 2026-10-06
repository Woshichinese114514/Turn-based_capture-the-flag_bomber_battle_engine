//! `sim` 的内部状态：实体、快照导出与状态修改接口。
//!
//! ## 为什么单独一层「内部实体」而不是直接用协议视图
//!
//! 协议视图（`UnitView` 等）是**只读导出**：它故意不含 AP、行动标记、炸弹归属等
//! 结算期需要的可变字段（见 `crates/protocol/src/view.rs` 的注释）。引擎需要一个
//! 可写的内部表示，同时保证「导出的快照与内部状态一致」这件事只在一个地方发生
//! （本模块的 `view()` 方法）。所有结算模块都只通过这里的方法读写实体，
//! 因此「某个字段忘了同步到快照」这类 bug 最多只会出现在本文件里。
//!
//! ## 遍历顺序即确定性
//!
//! `units` / `flags` / `bombs` 都是 `Vec`，按创建顺序排列，ID 单调递增。
//! 结算模块**必须**按下标顺序遍历它们（而不是 `HashMap`/`HashSet`），
//! 否则同种子两次运行会产生不同的伤害/掉落顺序，破坏逐字节可复现性。

use mapgen::{MapData, Rng};
use protocol::{
    BombView, Coord, EntityId, FlagView, GameEvent, RenderFrame, TeamId, UnitView,
};

use crate::ai::{MapView, ObsUnit, Observation};
use crate::{MatchConfig, RulesConfig};

/// 内部单位状态。
#[derive(Clone, Debug)]
pub(crate) struct Unit {
    /// 全局唯一 ID，整局不变（复活后仍是同一个 ID）。
    pub id: EntityId,
    pub team: TeamId,
    /// 当前坐标；死亡后保留死亡位置（仅用于快照展示）。
    pub pos: Coord,
    /// 当前 HP；死亡后为 0。
    pub hp: u8,
    pub alive: bool,
    /// 复活倒计时：`>0` 表示正在等待复活；存活单位恒为 0。
    pub respawn_timer: u32,
    /// 携带的旗 ID。
    pub carrying_flag: Option<EntityId>,
    /// 本 tick 是否已攻击过（每单位每 tick 至多攻击 1 次）。
    pub attacked_this_turn: bool,
    /// 本 tick 剩余 AP。
    pub ap_left: u8,
    /// 本 tick 内最后一次伤害来源（写进 `unit_died` 事件的 `by`；可能是自己的炸弹）。
    pub last_damage_by: Option<EntityId>,
}

impl Unit {
    /// 导出协议视图。
    pub fn view(&self) -> UnitView {
        UnitView {
            id: self.id,
            team: self.team,
            pos: self.pos,
            hp: self.hp,
            alive: self.alive,
            respawn_timer: self.respawn_timer,
            carrying_flag: self.carrying_flag,
            attacked_this_turn: self.attacked_this_turn,
        }
    }

    /// 是否还能行动（存活且有 AP）。
    pub fn can_act(&self) -> bool {
        self.alive && self.ap_left > 0
    }
}

/// 内部旗状态。
#[derive(Clone, Debug)]
pub(crate) struct Flag {
    pub id: EntityId,
    /// 旗所在格；被携带时必须等于携带者坐标（协议要求，见 `FlagView.pos`）。
    pub pos: Coord,
    /// 携带者单位 ID；`None` 表示旗在地上。
    pub carrier: Option<EntityId>,
}

impl Flag {
    pub fn view(&self) -> FlagView {
        FlagView {
            id: self.id,
            pos: self.pos,
            carrier: self.carrier,
        }
    }
}

/// 内部炸弹状态。
#[derive(Clone, Debug)]
pub(crate) struct Bomb {
    pub id: EntityId,
    pub pos: Coord,
    /// 放置者单位 ID（用于 `unit_died` 的 `by`：炸弹造成的死亡归功于放置者）。
    pub owner: EntityId,
    /// 放置者所属队伍（写进 `BombView.team`，UI 据此区分敌我）。
    pub team: TeamId,
    /// 剩余 tick 数：放置当回合为 `bomb_fuse`，之后每 tick 减 1，减到 0 的那个 tick 爆炸。
    pub timer: u8,
    pub radius: u8,
    /// 放置所在的 tick：该 tick 的倒计时阶段**跳过**这颗炸弹。
    ///
    /// 理由（`docs/rules.md` §4）：规则说「放置当回合计时 2，**之后**每 tick 减 1」，
    /// 所以放置的那一 tick 不减；否则回放里永远看不到 `timer = 2`，
    /// 与 `BombView.timer` 文档「能看到的取值是 2、1」矛盾。
    pub placed_tick: u32,
}

impl Bomb {
    pub fn view(&self) -> BombView {
        BombView {
            id: self.id,
            pos: self.pos,
            team: self.team,
            timer: self.timer,
            radius: self.radius,
        }
    }
}

/// 一局对局的全部可变状态。
///
/// 这里只放「会随 tick 变化」的数据；地图与规则分别由 `Sim` 持有（本结构的方法按需接收）。
pub(crate) struct GameState {
    /// 全部单位（含死亡），ID 单调递增，下标即遍历顺序。
    pub units: Vec<Unit>,
    /// 场上旗（含被携带的）。
    pub flags: Vec<Flag>,
    /// 场上炸弹。
    pub bombs: Vec<Bomb>,
    /// 各队分数，下标即队伍 ID。
    pub scores: Vec<i32>,
    /// 各队击杀数（只统计敌方击杀）。
    pub kills: Vec<u32>,
    /// 各队死亡数。
    pub deaths: Vec<u32>,
    /// 已结算的 tick 数（从 0 开始，第一个 tick 结算后为 1）。
    pub tick: u32,
    /// 本 tick 累积的事件（下一帧导出后清空）。
    pub events: Vec<GameEvent>,
    /// 下一个旗 ID（从 1 起，与 demo 回放一致）。
    next_flag_id: EntityId,
    /// 下一个炸弹 ID（从 1 起）。
    next_bomb_id: EntityId,
}

impl GameState {
    /// 建局：每队 `units_per_team` 个单位，按行优先摆在己方 3×3 阵营区内。
    ///
    /// 单位 ID 全局从 1 开始递增（team0 → 1,2,3；team1 → 4,5,6；…），
    /// 与 `samples/demo_2p.jsonl` 的约定一致。若 `units_per_team` 超过阵营格数（9），
    /// 多余单位会被丢弃（`Sim::new` 校验地图时不会检查这条，属极端配置，记录注释即可）。
    ///
    /// 调用方必须保证 `map.base_of(team)` 对每个队伍都存在（`Sim::new` 已校验）。
    pub fn new(teams: u8, rules: &RulesConfig, map: &MapData) -> Self {
        let size = rules.base_size_i32();
        let mut units = Vec::new();
        let mut next_id: EntityId = 1;
        for team in 0..teams {
            let base = map
                .base_of(team)
                .expect("Sim::new 已校验每个队伍都有阵营区");
            for slot in 0..rules.units_per_team as i32 {
                if slot >= size * size {
                    break;
                }
                // 行优先：先填满阵营区第一行，再第二行……保证布局稳定、可复现。
                let pos = Coord::new(base.x + slot % size, base.y + slot / size);
                units.push(Unit {
                    id: next_id,
                    team,
                    pos,
                    hp: rules.unit_max_hp,
                    alive: true,
                    respawn_timer: 0,
                    carrying_flag: None,
                    attacked_this_turn: false,
                    // 初始 AP 为 0：tick 1 的第 1 阶段会统一重置为 ap_per_unit。
                    ap_left: 0,
                    last_damage_by: None,
                });
                next_id += 1;
            }
        }
        Self {
            units,
            flags: Vec::new(),
            bombs: Vec::new(),
            scores: vec![0; teams as usize],
            kills: vec![0; teams as usize],
            deaths: vec![0; teams as usize],
            tick: 0,
            events: Vec::new(),
            next_flag_id: 1,
            next_bomb_id: 1,
        }
    }

    /// 进入新 tick（规则 §9 第 1 步）：清空事件、重置行动状态。
    ///
    /// 死亡单位的 AP 置 0 —— 这同时实现了「刚复活的单位本回合不能行动」：
    /// 复活发生在第 5 阶段，此时本 tick 的决策（第 2 阶段）早已结束，
    /// 而它的 AP 因为当时还死着而被置 0；下一个 tick 才会拿到 AP。
    pub fn begin_tick(&mut self, rules: &RulesConfig) {
        self.tick += 1;
        self.events.clear();
        for unit in &mut self.units {
            unit.attacked_this_turn = false;
            unit.last_damage_by = None;
            unit.ap_left = if unit.alive { rules.ap_per_unit } else { 0 };
        }
    }

    // ---- 查找 ----

    pub fn index_of(&self, id: EntityId) -> Option<usize> {
        self.units.iter().position(|u| u.id == id)
    }

    pub fn unit(&self, id: EntityId) -> Option<&Unit> {
        self.units.iter().find(|u| u.id == id)
    }

    pub fn unit_mut(&mut self, id: EntityId) -> Option<&mut Unit> {
        self.units.iter_mut().find(|u| u.id == id)
    }

    /// 该格上的**存活**单位下标（死亡单位不在地图上，不阻挡移动）。
    pub fn alive_at(&self, pos: Coord) -> Option<usize> {
        self.units.iter().position(|u| u.alive && u.pos == pos)
    }

    /// 该格上的旗下标（不论是否被携带）。
    pub fn flag_at(&self, pos: Coord) -> Option<usize> {
        self.flags.iter().position(|f| f.pos == pos)
    }

    /// 该格上是否有炸弹（炸弹可堆叠，只需要 bool）。
    pub fn bomb_at(&self, pos: Coord) -> bool {
        self.bombs.iter().any(|b| b.pos == pos)
    }

    pub fn alloc_flag_id(&mut self) -> EntityId {
        let id = self.next_flag_id;
        self.next_flag_id += 1;
        id
    }

    pub fn alloc_bomb_id(&mut self) -> EntityId {
        let id = self.next_bomb_id;
        self.next_bomb_id += 1;
        id
    }

    /// 扣 AP：成功返回 `true`；单位不存在/已死/AP 不足返回 `false`（不修改状态）。
    pub fn spend_ap(&mut self, id: EntityId, ap: u8) -> bool {
        match self.unit_mut(id) {
            Some(unit) if unit.can_act() && unit.ap_left >= ap => {
                unit.ap_left -= ap;
                true
            }
            _ => false,
        }
    }

    pub fn push_event(&mut self, event: GameEvent) {
        self.events.push(event);
    }

    /// 某队存活单位数（`all_dead_loses` 与测试使用）。
    pub fn alive_count(&self, team: TeamId) -> usize {
        self.units
            .iter()
            .filter(|u| u.alive && u.team == team)
            .count()
    }

    // ---- 批量访问（复活阶段用） ----

    /// 全部单位（只读，按 ID 升序 = 创建顺序）。
    pub fn units(&self) -> &[Unit] {
        &self.units
    }

    /// 全部单位（可变，按 ID 升序）。
    ///
    /// 只暴露切片而不是整体替换：调用方（复活阶段）只需要逐单位改字段，
    /// 不应当增删单位 —— 单位集合在 `new` 之后是固定的。
    pub fn units_mut(&mut self) -> &mut [Unit] {
        &mut self.units
    }

    /// 复活：把单位放回地图 `pos`，回满血、清空倒计时与伤害来源。
    ///
    /// AP 保持 0：复活发生在 tick 的第 5 阶段，而第 2 阶段的决策早已结束，
    /// 因此「刚复活本回合不能行动」由「下一个 tick 的 `begin_tick` 才给 AP」自然保证。
    /// 返回 `false` 表示单位不存在（正常流程不会发生）。
    pub fn respawn_unit(&mut self, id: EntityId, pos: Coord, hp: u8) -> bool {
        match self.unit_mut(id) {
            Some(unit) => {
                unit.alive = true;
                unit.hp = hp;
                unit.pos = pos;
                unit.respawn_timer = 0;
                unit.ap_left = 0;
                unit.attacked_this_turn = false;
                unit.last_damage_by = None;
                true
            }
            None => false,
        }
    }

    // ---- 死亡结算 ----

    /// 让单位死亡（HP 归 0）并完成全部副作用。
    ///
    /// 集中在这里的原因：攻击（`combat`）与爆炸（`bomb`）都会造成死亡，两边都必须
    /// 一致地处理「掉旗、击杀计数、`unit_died` 事件」，否则很容易出现
    /// 「被炸弹炸死不掉旗」这类不对称 bug。
    ///
    /// * `by`：造成致命伤的单位（可能是自己），用于击杀统计与事件；
    /// * 击杀只在 `by` 属于**敌方**时计入（`kills` 只统计敌方击杀，见 `MatchResult.kills`）；
    /// * 携带的旗交给 `flag::drop_carried_flag` 按规则 §6 处理（3×3 → 5×5 → 中心区域 → 掉线）。
    pub fn kill_unit(
        &mut self,
        victim: EntityId,
        by: Option<EntityId>,
        map: &MapData,
        rules: &RulesConfig,
        rng: &mut Rng,
    ) {
        let Some(index) = self.index_of(victim) else {
            return;
        };
        if !self.units[index].alive {
            return;
        }
        let team = self.units[index].team;
        let pos = self.units[index].pos;
        let carried = self.units[index].carrying_flag.take();
        {
            let unit = &mut self.units[index];
            unit.hp = 0;
            unit.alive = false;
            unit.respawn_timer = rules.respawn_ticks;
            unit.ap_left = 0;
            unit.attacked_this_turn = false;
            unit.last_damage_by = by;
        }
        if let Some(slot) = self.deaths.get_mut(team as usize) {
            *slot += 1;
        }
        if let Some(killer) = by {
            let killer_team = self.unit(killer).map(|u| u.team);
            if let Some(killer_team) = killer_team {
                if killer_team != team {
                    if let Some(slot) = self.kills.get_mut(killer_team as usize) {
                        *slot += 1;
                    }
                }
            }
        }
        self.events.push(GameEvent::UnitDied {
            unit: victim,
            team,
            by,
        });
        if let Some(flag_id) = carried {
            // 掉落只需要地图与 RNG；`rules` 里的数值不参与落点选择（规则 §6 固定 3×3→5×5→中心）。
            crate::flag::drop_carried_flag(self, map, rng, pos, flag_id);
        }
    }

    // ---- 快照导出 ----

    /// 取走本 tick 事件并构造渲染帧（调用后 `events` 清空）。
    /// 构造对外可见的旗视图列表。
    ///
    /// 协议要求「旗被携带时 `FlagView` 坐标必须等于携带者坐标」
    /// （docs/replay-format.md 的校验规则，tools/validate_replay.py 会逐帧检查）。
    /// 内部 `Flag.pos` 只在刷新/掉落时更新，拾取后不再跟着单位移动，
    /// 所以这里统一做一次坐标归一化；`take_frame` 与 `observation` 共用它，
    /// 避免两条出口各写一遍而漏掉其中一条。
    fn flag_views(&self) -> Vec<FlagView> {
        self.flags
            .iter()
            .map(|flag| {
                let mut view = flag.view();
                if let Some(carrier) = view.carrier {
                    if let Some(unit) = self.unit(carrier) {
                        view.pos = unit.pos;
                    }
                }
                view
            })
            .collect()
    }

    /// 取出当前 tick 的回放帧（事件被清空，交给调用方）。
    pub fn take_frame(&mut self) -> RenderFrame {
        RenderFrame {
            tick: self.tick,
            scores: self.scores.clone(),
            units: self.units.iter().map(Unit::view).collect(),
            flags: self.flag_views(),
            bombs: self.bombs.iter().map(Bomb::view).collect(),
            events: std::mem::take(&mut self.events),
        }
    }

    /// 为某队构造决策快照。
    ///
    /// 必须在任何动作结算**之前**调用（规则 §9 第 2 步要求「决策前快照」），
    /// 否则 AI 会看到本 tick 的部分结算结果，同种子也可能因队伍决策顺序不同而分叉。
    pub fn observation(&self, team: TeamId, config: &MatchConfig, map: &MapData) -> Observation {
        let units: Vec<ObsUnit> = self
            .units
            .iter()
            .map(|unit| ObsUnit::from_view(&unit.view(), unit.ap_left))
            .collect();
        // 单位按 ID 递增创建，因此这里天然升序；仍然显式排序，
        // 让契约（my_units 升序）不依赖创建顺序这一隐含前提。
        let mut my_units: Vec<EntityId> = self
            .units
            .iter()
            .filter(|unit| unit.team == team)
            .map(|unit| unit.id)
            .collect();
        my_units.sort_unstable();
        Observation {
            tick: self.tick,
            max_ticks: config.max_ticks,
            team,
            map: MapView::from_map_data(map),
            scores: self.scores.clone(),
            units,
            flags: self.flag_views(),
            bombs: self.bombs.iter().map(Bomb::view).collect(),
            my_units,
        }
    }
}
