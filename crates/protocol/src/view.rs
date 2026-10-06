//! 每个 tick 的动态实体视图与游戏事件。
//!
//! 这些结构是「引擎 → Web UI」的单向数据流：引擎保证视图内容是**已经完全结算后的状态**，
//! UI 不需要也不应该再算任何规则（例如：单位是否存活、旗在谁手上、炸弹还剩几 tick 都由引擎算好）。
//!
//! 之所以把「视图」，而不是完整的内部状态（单位 AP、行动队列、RNG 状态等）导出去，
//! 是为了让回放文件与内部实现解耦：内部重构不会破坏回放格式。

use serde::{Deserialize, Serialize};

use crate::types::{Coord, EntityId, TeamId, Tick};

/// 单位视图（每 tick 一个）。
///
/// 坐标以 `#[serde(flatten)]` 平铺，JSON 形如：
/// `{"id":0,"team":0,"x":3,"y":4,"hp":3,"alive":true,...}`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitView {
    /// 单位在其队伍内的稳定 ID（0、1、2），全局唯一。
    ///
    /// 刻意在整局内保持不变（死亡复活后 ID 不变），这样 UI 才能做「追踪某个单位」的高亮，
    /// 回放事件也能一直引用同一个 ID。
    pub id: EntityId,
    /// 所属队伍。
    pub team: TeamId,
    /// 单位所在格；死亡（`alive == false`）时保留死亡位置，UI 可选择画灰色虚影。
    #[serde(flatten)]
    pub pos: Coord,
    /// 当前 HP，范围 `0..=3`。
    pub hp: u8,
    /// 是否存活。死亡单位为 `false`，此时它在地图上不阻挡移动。
    pub alive: bool,
    /// 复活倒计时（tick）：`0` 表示未在倒计时（存活或刚复活），`>0` 表示还需等待的 tick 数。
    pub respawn_timer: u32,
    /// 携带的旗 ID；没拿旗为 `null`。
    pub carrying_flag: Option<EntityId>,
    /// 本回合是否已经攻击过（每回合每单位最多攻击 1 次，见规则 2.3）。
    /// UI 用它画「已攻击」小标记。
    pub attacked_this_turn: bool,
}

/// 旗视图（每 tick 一个，含场上与掉落/被携带的旗）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagView {
    /// 旗 ID，整局稳定。
    pub id: EntityId,
    /// 旗当前所在格。被携带时等于携带者所在格（引擎保证两者一致），
    /// 这样 UI 只按坐标画即可，不需要额外判断。
    #[serde(flatten)]
    pub pos: Coord,
    /// 携带者单位 ID；旗在地面上时为 `null`。
    ///
    /// 用 `Option<EntityId>` 而不是 `0` 表示「无」，因为单位 ID 0 是合法值。
    pub carrier: Option<EntityId>,
}

/// 炸弹视图（每 tick 一个）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BombView {
    /// 炸弹 ID。
    pub id: EntityId,
    /// 放置位置（炸弹不会移动）。
    #[serde(flatten)]
    pub pos: Coord,
    /// 放置者所属队伍；用于 UI 区分敌我炸弹（炸弹有友伤，颜色不能只按敌我区分）。
    pub team: TeamId,
    /// 倒计时：还需经过多少个 tick 结算才爆炸。
    ///
    /// 语义（务必与 `sim` 一致）：放置当回合为 `2`，此后每过一个 tick 减 1，
    /// 减到 `0` 的**那个 tick 结算阶段**爆炸，因此回放中能看到的取值是 `2`、`1`，看不到 `0`。
    pub timer: u8,
    /// 爆炸半径（十字，曼哈顿距离 ≤ radius 且不被墙阻挡）。
    pub radius: u8,
}

/// 一个 tick 的渲染帧。
///
/// 这是回放里出现次数最多的行（每 tick 一行），因此只包含**动态**信息：
/// 地图静态数据在 `init` 行里，分数是「当前分数」（不是增量），
/// 单位/旗/炸弹是「本 tick 结算后的完整快照」（不是差分）。
///
/// 选择全量快照而不是差分的原因：Web UI 需要能 O(1) 跳到任意 tick（进度条拖动、
/// 输入 tick 直接跳转），差分格式会强迫 UI 从头重放。代价是文件更大——对于
/// 20 个实体 × 300 tick 完全可接受。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderFrame {
    /// 当前全局回合数（从 1 开始，单调递增，逐帧 +1）。
    pub tick: Tick,
    /// 各队分数，**下标即队伍 ID**（`scores[0]` 是 0 队分数）。
    pub scores: Vec<i32>,
    /// 所有单位视图（含死亡单位，便于 UI 画虚影/墓碑）。
    pub units: Vec<UnitView>,
    /// 所有旗视图（含被携带的旗）。
    pub flags: Vec<FlagView>,
    /// 所有炸弹视图。
    pub bombs: Vec<BombView>,
    /// 本 tick 发生的事件列表。
    ///
    /// 事件是「可选的动画素材」：UI 只用它播动画与写事件日志，
    /// 即使完全忽略事件也能正确渲染（因为实体视图已是最终状态）。
    pub events: Vec<GameEvent>,
}

/// 游戏事件。
///
/// 用**内部标签**序列化：`#[serde(tag = "type")]`，因此 JSON 里靠 `type` 字段区分类型，
/// 例如 `{"type":"unit_attacked","attacker":0,"target":5,"damage":1}`。
/// 事件名与字段名都是 snake_case，和 Web UI 的事件描述函数一一对应。
///
/// 坐标字段刻意展开成 `from_x`/`from_y`/`to_x`/`to_y` 这类平铺整数，而不是嵌套对象或
/// `Coord` 的 flatten：内部标签枚举（internally tagged）与 `flatten` 组合时 serde 需要
/// 缓冲整个对象，既慢又容易在嵌套结构上出错；事件数量多，这里选择最朴素可靠的写法。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GameEvent {
    /// 单位成功移动一格。
    UnitMoved {
        unit: EntityId,
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
    },
    /// 单位攻击命中。注意：命中必定造成 `damage` 点伤害（没有闪避/暴击规则）。
    UnitAttacked {
        attacker: EntityId,
        target: EntityId,
        damage: u8,
    },
    /// 单位死亡（HP 归 0）。`by` 是最后一次造成伤害的来源（可能是自己的炸弹），可能为 `null`。
    UnitDied {
        unit: EntityId,
        team: TeamId,
        by: Option<EntityId>,
    },
    /// 单位复活（在己方阵营内满血出现）。
    UnitRespawned {
        unit: EntityId,
        team: TeamId,
        x: i32,
        y: i32,
    },
    /// 单位拾旗成功。
    FlagPicked { unit: EntityId, flag: EntityId },
    /// 旗掉落（携带者死亡）到指定格。
    FlagDropped { flag: EntityId, x: i32, y: i32 },
    /// 旗在地图中心区域刷新。
    FlagSpawned { flag: EntityId, x: i32, y: i32 },
    /// 得分：携带者进入己方阵营，旗消失，队伍分数 +1。`new_score` 是加分后的分数。
    Score {
        team: TeamId,
        unit: EntityId,
        flag: EntityId,
        new_score: i32,
    },
    /// 炸弹被放置。
    BombPlaced {
        unit: EntityId,
        bomb: EntityId,
        x: i32,
        y: i32,
        timer: u8,
    },
    /// 炸弹爆炸。`hit_units` 是被本次爆炸实际扣血的单位（阵营格免疫者不在其中）。
    BombExploded {
        bomb: EntityId,
        x: i32,
        y: i32,
        radius: u8,
        hit_units: Vec<EntityId>,
    },
    /// 移动冲突：同一 tick 有多个单位请求进入同一格，全部留在原地（但已消耗 AP）。
    ///
    /// 单独建模成事件而不是「移动失败」，是为了让 UI 能在冲突格上打一个显眼的爆闪标记——
    /// 这是本游戏最需要被复盘的时刻（谁和谁撞了、为什么没换位成功）。
    MoveConflict {
        x: i32,
        y: i32,
        units: Vec<EntityId>,
    },
    /// 非法动作：AI 提交了不合法指令（AP 不足、距离太远、目标已死…），引擎按等待处理。
    ///
    /// 记录 `action` 与 `reason` 是为了让 AI 开发者能从回放里定位自己 AI 的 bug，
    /// 而不是只看到「单位没动」。
    IllegalAction {
        unit: EntityId,
        action: String,
        reason: String,
    },
}

impl GameEvent {
    /// 事件的 snake_case 类型名（与 JSON `type` 字段一致）。
    ///
    /// 手写而不依赖 serde 反射，是为了让 Web UI 的事件描述函数能用同一批字符串做分支，
    /// 同时给事件日志提供一个稳定的排序/过滤键。
    pub const fn type_name(&self) -> &'static str {
        match self {
            GameEvent::UnitMoved { .. } => "unit_moved",
            GameEvent::UnitAttacked { .. } => "unit_attacked",
            GameEvent::UnitDied { .. } => "unit_died",
            GameEvent::UnitRespawned { .. } => "unit_respawned",
            GameEvent::FlagPicked { .. } => "flag_picked",
            GameEvent::FlagDropped { .. } => "flag_dropped",
            GameEvent::FlagSpawned { .. } => "flag_spawned",
            GameEvent::Score { .. } => "score",
            GameEvent::BombPlaced { .. } => "bomb_placed",
            GameEvent::BombExploded { .. } => "bomb_exploded",
            GameEvent::MoveConflict { .. } => "move_conflict",
            GameEvent::IllegalAction { .. } => "illegal_action",
        }
    }

    /// 事件是否与某支队伍相关（用于事件日志按队伍过滤）。
    ///
    /// 「相关」的定义偏宽松：移动/攻击的发起方、死亡单位、旗的拾取者、得分队伍都算。
    /// 旗帜掉落、炸弹爆炸这类没有明确单一归属的事件返回 `None`，表示「不分队，始终显示」。
    pub fn related_team(&self) -> Option<TeamId> {
        // 说明：事件里没有队伍字段的（移动、拾旗）由调用方结合单位表判断；
        // 这里只处理事件本身携带队伍信息的情况，避免本函数依赖单位快照。
        match self {
            GameEvent::UnitDied { team, .. } | GameEvent::UnitRespawned { team, .. } => Some(*team),
            GameEvent::Score { team, .. } => Some(*team),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips_through_json() {
        let frame = RenderFrame {
            tick: 7,
            scores: vec![1, 0],
            units: vec![UnitView {
                id: 3,
                team: 1,
                pos: Coord::new(4, 5),
                hp: 2,
                alive: true,
                respawn_timer: 0,
                carrying_flag: Some(9),
                attacked_this_turn: true,
            }],
            flags: vec![FlagView {
                id: 9,
                pos: Coord::new(4, 5),
                carrier: Some(3),
            }],
            bombs: vec![BombView {
                id: 1,
                pos: Coord::new(2, 2),
                team: 0,
                timer: 2,
                radius: 2,
            }],
            events: vec![GameEvent::UnitMoved {
                unit: 3,
                from_x: 3,
                from_y: 5,
                to_x: 4,
                to_y: 5,
            }],
        };
        let json = serde_json::to_string(&frame).expect("序列化");
        // 坐标必须平铺，而不是嵌套成 "pos":{...}
        assert!(json.contains("\"x\":4"), "坐标应平铺：{json}");
        assert!(!json.contains("\"pos\""), "不应出现 pos 嵌套字段：{json}");
        // 事件必须使用 type 标签
        assert!(json.contains("\"type\":\"unit_moved\""), "事件用 type 标签：{json}");
        let back: RenderFrame = serde_json::from_str(&json).expect("反序列化");
        assert_eq!(back, frame, "视图必须 JSON 往返一致");
    }

    #[test]
    fn event_type_names_match_serde_tags() {
        // 事件类型名必须与 JSON 的 type 字段一致，否则 UI 过滤/日志会失配。
        let samples = vec![
            (
                GameEvent::FlagDropped {
                    flag: 1,
                    x: 0,
                    y: 0,
                },
                "flag_dropped",
            ),
            (
                GameEvent::BombExploded {
                    bomb: 1,
                    x: 0,
                    y: 0,
                    radius: 2,
                    hit_units: vec![],
                },
                "bomb_exploded",
            ),
            (
                GameEvent::IllegalAction {
                    unit: 1,
                    action: "attack".into(),
                    reason: "ap".into(),
                },
                "illegal_action",
            ),
        ];
        for (event, expected) in samples {
            let json = serde_json::to_string(&event).expect("序列化");
            assert!(
                json.contains(&format!("\"type\":\"{expected}\"")),
                "事件 {expected} 的 JSON 标签不匹配：{json}"
            );
            assert_eq!(event.type_name(), expected);
        }
    }
}
