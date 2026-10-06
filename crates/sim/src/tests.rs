//! `docs/rules.md` §14 测试清单里属于 sim 的第 5–14 条。
//!
//! 测试策略：规则级测试（5–13）**不走整局**，而是手工搭一个平坦地图 + `GameState`，
//! 然后直接调用对应的结算函数。理由是这样每个断言只对应一条规则，失败时能一眼看出
//! 是哪条规则破了；整局跑动由第 14 条（可复现性）覆盖。
//!
//! 平坦地图是手工构造的 `MapData`：除了阵营格全是 `Empty`，因此不用担心地图生成器
//! 恰好把墙放在了测试想走的那一格上。

use mapgen::{MapData, Rng};
use protocol::{Coord, Direction, EntityId, GameEvent, Terrain};

use crate::ai::{Action, Observation, TeamActions, TeamAi, UnitCommand};
use crate::state::{Flag, GameState};
use crate::{MatchConfig, RulesConfig, Sim};

// ---------------------------------------------------------------- 测试脚手架

/// 手工构造一张平坦地图：除阵营格（`TeamBase`）外全是 `Empty`。
fn flat_map(width: u16, height: u16, teams: u8) -> MapData {
    let mut terrain = vec![Terrain::Empty; width as usize * height as usize];
    let bases: Vec<Coord> = (0..teams)
        .map(|team| base_corner(width, height, team))
        .collect();
    // 阵营格标成 TeamBase，与 mapgen 的约定保持一致（地形与 bases 必须互相印证）。
    for (team, base) in bases.iter().enumerate() {
        for dy in 0..3 {
            for dx in 0..3 {
                let idx = (base.y + dy) as usize * width as usize + (base.x + dx) as usize;
                terrain[idx] = Terrain::TeamBase(team as u8);
            }
        }
    }
    MapData {
        width,
        height,
        map_gen_version: mapgen::MAP_GEN_VERSION,
        terrain,
        bases,
        center: Coord::new(width as i32 / 2, height as i32 / 2),
        center_radius: 4,
    }
}

/// 阵营区左上角；与 `mapgen` 的布局约定一致（2 队：左上 / 右下）。
fn base_corner(width: u16, height: u16, team: u8) -> Coord {
    match team {
        0 => Coord::new(1, 1),
        1 => Coord::new(width as i32 - 4, height as i32 - 4),
        _ => Coord::new((width as i32 - 3) / 2, height as i32 - 4),
    }
}

/// 平坦地图 + 初始状态，并且已经进入 tick 1（AP 已重置为 2）。
fn flat_state(teams: u8) -> (GameState, MapData, RulesConfig) {
    let rules = RulesConfig::default();
    let map = flat_map(20, 20, teams);
    let mut state = GameState::new(teams, &rules, &map);
    state.begin_tick(&rules);
    (state, map, rules)
}

/// 把单位搬到指定坐标（测试里直接摆位，不走移动规则）。
fn place(state: &mut GameState, unit: EntityId, pos: Coord) {
    state
        .unit_mut(unit)
        .unwrap_or_else(|| panic!("测试单位 {unit} 应存在"))
        .pos = pos;
}

fn hp(state: &GameState, unit: EntityId) -> u8 {
    state
        .unit(unit)
        .unwrap_or_else(|| panic!("测试单位 {unit} 应存在"))
        .hp
}

fn ap(state: &GameState, unit: EntityId) -> u8 {
    state
        .unit(unit)
        .unwrap_or_else(|| panic!("测试单位 {unit} 应存在"))
        .ap_left
}

fn is_alive(state: &GameState, unit: EntityId) -> bool {
    state
        .unit(unit)
        .unwrap_or_else(|| panic!("测试单位 {unit} 应存在"))
        .alive
}

fn cmd(unit: EntityId, action: Action) -> UnitCommand {
    UnitCommand { unit, action }
}

/// 收集本 tick 全部 `illegal_action` 的 reason。
fn illegal_reasons(state: &GameState) -> Vec<String> {
    state
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::IllegalAction { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

fn resolve_moves(state: &mut GameState, map: &MapData, commands: &[UnitCommand]) {
    crate::conflict::resolve_moves(state, map, commands);
}

fn resolve_attacks(
    state: &mut GameState,
    map: &MapData,
    rules: &RulesConfig,
    rng: &mut Rng,
    commands: &[UnitCommand],
) {
    crate::combat::resolve_attacks(state, map, rules, rng, commands);
}

// ---------------------------------------------------------------- §14-5 攻击射程与视线

/// §14-5：射程 3 可命中；射程 4 不可；中间隔墙不可。
#[test]
fn attack_range_three_hits_four_misses_and_wall_blocks_sight() {
    let rules = RulesConfig::default();
    let mut rng = Rng::new(1);

    // (a) 距离 3：命中，伤害 1。
    {
        let (mut state, map, _) = flat_state(2);
        place(&mut state, 1, Coord::new(5, 5));
        place(&mut state, 4, Coord::new(5, 8));
        resolve_attacks(
            &mut state,
            &map,
            &rules,
            &mut rng,
            &[cmd(1, Action::Attack(4))],
        );
        assert_eq!(hp(&state, 4), 2, "距离 3 应该命中");
        assert_eq!(ap(&state, 1), 1, "攻击消耗 1 AP");
        assert!(illegal_reasons(&state).is_empty());
    }

    // (b) 距离 4：超出射程，记非法动作，不扣 HP、不扣 AP。
    {
        let (mut state, map, _) = flat_state(2);
        place(&mut state, 1, Coord::new(5, 5));
        place(&mut state, 4, Coord::new(5, 9));
        resolve_attacks(
            &mut state,
            &map,
            &rules,
            &mut rng,
            &[cmd(1, Action::Attack(4))],
        );
        assert_eq!(hp(&state, 4), 3, "距离 4 不该命中");
        assert_eq!(ap(&state, 1), 2, "非法攻击不消耗 AP");
        assert!(
            illegal_reasons(&state).iter().any(|r| r.contains("超出射程")),
            "应记 illegal_action（超出射程）"
        );
    }

    // (c) 距离 3 但中间有墙：无视线。
    {
        let (mut state, mut map, _) = flat_state(2);
        place(&mut state, 1, Coord::new(5, 5));
        place(&mut state, 4, Coord::new(5, 8));
        let wall = map.index(5, 7).expect("(5,7) 在地图内");
        map.terrain[wall] = Terrain::Wall;
        resolve_attacks(
            &mut state,
            &map,
            &rules,
            &mut rng,
            &[cmd(1, Action::Attack(4))],
        );
        assert_eq!(hp(&state, 4), 3, "墙后面的目标打不到");
        assert!(
            illegal_reasons(&state).iter().any(|r| r.contains("视线")),
            "应记 illegal_action（没有视线）"
        );
    }
}

/// §14-6：同一单位同一回合攻击两次，第二次失败并记 `illegal_action`。
#[test]
fn second_attack_in_same_tick_is_rejected() {
    let (mut state, map, rules) = flat_state(2);
    let mut rng = Rng::new(2);
    place(&mut state, 1, Coord::new(5, 5));
    place(&mut state, 4, Coord::new(5, 8));

    resolve_attacks(
        &mut state,
        &map,
        &rules,
        &mut rng,
        &[cmd(1, Action::Attack(4)), cmd(1, Action::Attack(4))],
    );

    assert_eq!(hp(&state, 4), 2, "只应该命中一次");
    assert_eq!(ap(&state, 1), 1, "只应该消耗 1 AP");
    assert!(
        illegal_reasons(&state)
            .iter()
            .any(|r| r.contains("本回合已攻击过")),
        "第二次攻击必须记 illegal_action"
    );
}

// ---------------------------------------------------------------- §14-7/8/9 移动与冲突

/// §14-7：两个单位同 tick 抢同一格 → 都留原地、都消耗 1 AP、有 `move_conflict`。
#[test]
fn two_units_racing_for_one_cell_both_stay_and_spend_ap() {
    let (mut state, map, _) = flat_state(2);
    place(&mut state, 1, Coord::new(5, 5));
    place(&mut state, 4, Coord::new(6, 4));

    resolve_moves(
        &mut state,
        &map,
        &[cmd(1, Action::Move(Direction::Right)), cmd(4, Action::Move(Direction::Down))],
    );

    assert_eq!(state.unit(1).expect("1 号存在").pos, Coord::new(5, 5));
    assert_eq!(state.unit(4).expect("4 号存在").pos, Coord::new(6, 4));
    assert_eq!(ap(&state, 1), 1, "冲突消耗 1 AP");
    assert_eq!(ap(&state, 4), 1, "冲突消耗 1 AP");
    assert!(
        illegal_reasons(&state).is_empty(),
        "撞人是冲突不是非法动作"
    );
    let conflict = state
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::MoveConflict { x, y, units } => Some((*x, *y, units.clone())),
            _ => None,
        })
        .expect("应有 move_conflict 事件");
    assert_eq!((conflict.0, conflict.1), (6, 5));
    assert_eq!(conflict.2, vec![1, 4], "事件里应含全部请求者（按行动顺序）");
}

/// §14-8：A→B、B→A 双方成功（交换位置）。
#[test]
fn swapping_units_both_succeed() {
    let (mut state, map, _) = flat_state(2);
    place(&mut state, 1, Coord::new(5, 5));
    place(&mut state, 4, Coord::new(6, 5));

    resolve_moves(
        &mut state,
        &map,
        &[cmd(1, Action::Move(Direction::Right)), cmd(4, Action::Move(Direction::Left))],
    );

    assert_eq!(state.unit(1).expect("1 号存在").pos, Coord::new(6, 5));
    assert_eq!(state.unit(4).expect("4 号存在").pos, Coord::new(5, 5));
    assert_eq!(ap(&state, 1), 1, "成功移动消耗 1 AP");
    assert_eq!(ap(&state, 4), 1);
    assert_eq!(
        state
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::UnitMoved { .. }))
            .count(),
        2
    );
}

/// §14-9：撞墙不消耗 AP；走进「不会离开」的单位所占格也不消耗 AP（§7.4）。
///
/// 「撞人消耗 AP」专门指 §7.3 的多单位抢同一格，已在上一个测试里断言。
#[test]
fn bumping_wall_or_stationary_unit_costs_no_ap() {
    // (a) 撞墙
    {
        let (mut state, mut map, _) = flat_state(2);
        place(&mut state, 1, Coord::new(5, 5));
        let wall = map.index(5, 4).expect("(5,4) 在地图内");
        map.terrain[wall] = Terrain::Wall;
        resolve_moves(&mut state, &map, &[cmd(1, Action::Move(Direction::Up))]);
        assert_eq!(state.unit(1).expect("1 号存在").pos, Coord::new(5, 5));
        assert_eq!(ap(&state, 1), 2, "撞墙不消耗 AP");
        assert!(
            illegal_reasons(&state).iter().any(|r| r.contains("墙")),
            "应记 illegal_action（目标是墙）"
        );
    }

    // (b) 目标格被一个不动的单位占着
    {
        let (mut state, map, _) = flat_state(2);
        place(&mut state, 1, Coord::new(5, 5));
        place(&mut state, 4, Coord::new(6, 5));
        resolve_moves(&mut state, &map, &[cmd(1, Action::Move(Direction::Right))]);
        assert_eq!(state.unit(1).expect("1 号存在").pos, Coord::new(5, 5));
        assert_eq!(ap(&state, 1), 2, "撞不动的人不消耗 AP");
        assert!(
            illegal_reasons(&state).iter().any(|r| r.contains("占用")),
            "应记 illegal_action（目标格被占用）"
        );
    }
}

// ---------------------------------------------------------------- §14-10 掉旗

/// §14-10：携带者死亡 → 旗掉在死亡格周围 3×3 内的合法格（非墙、非虚空、非阵营）。
#[test]
fn carrier_death_drops_flag_in_legal_neighbour_cell() {
    let (mut state, map, rules) = flat_state(2);
    let mut rng = Rng::new(3);
    let death = Coord::new(10, 10);
    place(&mut state, 1, death);
    let flag_id = state.alloc_flag_id();
    state.flags.push(Flag {
        id: flag_id,
        pos: death,
        carrier: Some(1),
    });
    state
        .unit_mut(1)
        .expect("1 号存在")
        .carrying_flag = Some(flag_id);

    state.kill_unit(1, Some(4), &map, &rules, &mut rng);

    assert!(!is_alive(&state, 1));
    assert_eq!(state.flags.len(), 1, "周围有合法格时旗不应该消失");
    let flag = &state.flags[0];
    assert_eq!(flag.carrier, None, "掉落后没有携带者");
    // 第一级随机池是死亡格周围的 3×3（不含死亡格本身），所以落点必须是切比雪夫距离 1 的邻居：
    // 对角邻居的曼哈顿距离是 2，因此这里断言的是切比雪夫距离。
    let (dx, dy) = ((flag.pos.x - death.x).abs(), (flag.pos.y - death.y).abs());
    assert_eq!(
        (dx.max(dy), dx + dy),
        (1, dx + dy),
        "落点必须在死亡格周围 3×3 的邻居格上（非死亡格本身），实际 {:?}",
        flag.pos
    );
    assert_ne!(flag.pos, death, "死亡格本身属于最后一级兜底，不该出现在第一级");
    assert!(map.is_walkable(flag.pos.x, flag.pos.y), "落点必须可通行");
    assert!(
        map.in_any_base(flag.pos.x, flag.pos.y).is_none(),
        "旗不能掉进阵营区"
    );
    assert!(
        state
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::FlagDropped { flag, .. } if *flag == flag_id)),
        "应发 flag_dropped 事件"
    );
}

// ---------------------------------------------------------------- §14-11 复活

/// §14-11：死亡后第 10 个 tick 满血复活，且落在己方阵营 3×3 内。
#[test]
fn dead_unit_respawns_on_tenth_tick_in_own_base() {
    let (mut state, map, rules) = flat_state(2);
    let mut rng = Rng::new(4);
    state.kill_unit(1, None, &map, &rules, &mut rng);
    assert_eq!(state.unit(1).expect("1 号存在").respawn_timer, 10);

    let mut revived_on = None;
    for tick in 1..=10 {
        crate::engine::resolve_respawns(&mut state, &map, &rules);
        if is_alive(&state, 1) {
            revived_on = Some(tick);
            break;
        }
    }
    assert_eq!(revived_on, Some(10), "死亡后第 10 个 tick 复活");

    let unit = state.unit(1).expect("1 号存在");
    assert_eq!(unit.hp, rules.unit_max_hp, "复活必须满血");
    assert_eq!(unit.respawn_timer, 0);
    assert!(
        map.in_base(0, unit.pos.x, unit.pos.y),
        "复活必须在己方阵营区（实际 {:?}）",
        unit.pos
    );
    assert!(
        state
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::UnitRespawned { unit: 1, team: 0, .. })),
        "应发 unit_respawned 事件"
    );
}

/// §14-11（续）：己方阵营满员时复活延后，腾出格子后的下一个 tick 才复活。
#[test]
fn respawn_is_delayed_while_own_base_is_full() {
    // 让每队 9 个单位把 3×3 阵营区正好填满。
    let rules = RulesConfig {
        units_per_team: 9,
        ..RulesConfig::default()
    };
    let map = flat_map(20, 20, 2);
    let mut state = GameState::new(2, &rules, &map);
    state.begin_tick(&rules);
    let mut rng = Rng::new(5);

    state.kill_unit(1, None, &map, &rules, &mut rng);
    // 直接把倒计时压到 0：这里测的是「满员延后」，不是倒计时本身（上一个测试已覆盖）。
    state
        .unit_mut(1)
        .expect("1 号存在")
        .respawn_timer = 0;
    // 让一个敌方单位站进刚空出来的 (1,1)，阵营重新满员。
    place(&mut state, 10, Coord::new(1, 1));

    crate::engine::resolve_respawns(&mut state, &map, &rules);
    assert!(!is_alive(&state, 1), "阵营满员时必须延后");
    assert_eq!(
        state.unit(1).expect("1 号存在").respawn_timer,
        0,
        "延后时倒计时保持 0，不能重置"
    );

    // 腾出一格 → 下一个 tick 立即复活。
    place(&mut state, 10, Coord::new(10, 10));
    crate::engine::resolve_respawns(&mut state, &map, &rules);
    assert!(is_alive(&state, 1), "腾出格子后应该复活");
}

// ---------------------------------------------------------------- §14-12 得分

/// §14-12：携带旗进入己方阵营 → 队伍 +1、旗消失、`score` 事件带 `new_score`。
#[test]
fn carrying_flag_into_own_base_scores() {
    let (mut state, map, _) = flat_state(2);
    let base = map.base_of(0).expect("team0 有阵营区");
    place(&mut state, 1, base);
    let flag_id = state.alloc_flag_id();
    state.flags.push(Flag {
        id: flag_id,
        pos: base,
        carrier: Some(1),
    });
    state
        .unit_mut(1)
        .expect("1 号存在")
        .carrying_flag = Some(flag_id);

    crate::flag::check_scores(&mut state, &map);

    assert_eq!(state.scores[0], 1, "队伍 0 得 1 分");
    assert!(state.flags.is_empty(), "旗必须消失");
    assert_eq!(
        state.unit(1).expect("1 号存在").carrying_flag,
        None,
        "携带关系必须解除"
    );
    let score = state
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::Score {
                team,
                unit,
                flag,
                new_score,
            } => Some((*team, *unit, *flag, *new_score)),
            _ => None,
        })
        .expect("应有 score 事件");
    assert_eq!(score, (0, 1, flag_id, 1));
}

/// §14-12（反例）：站在阵营外不得分。
#[test]
fn carrying_flag_outside_base_does_not_score() {
    let (mut state, map, _) = flat_state(2);
    place(&mut state, 1, Coord::new(10, 10));
    let flag_id = state.alloc_flag_id();
    state.flags.push(Flag {
        id: flag_id,
        pos: Coord::new(10, 10),
        carrier: Some(1),
    });
    state
        .unit_mut(1)
        .expect("1 号存在")
        .carrying_flag = Some(flag_id);

    crate::flag::check_scores(&mut state, &map);

    assert_eq!(state.scores[0], 0);
    assert_eq!(state.flags.len(), 1, "旗仍在场上");
}

/// 回归测试：旗被携带后，`FlagView` 坐标必须始终等于携带者坐标。
///
/// 这个 bug 是真实回放跑 `tools/validate_replay.py` 抓出来的：
/// 内部 `Flag.pos` 记录的是「刷新/掉落」位置，拾取后不再跟着单位走，
/// 于是帧里出现「旗在 (7,6) 而携带者在 (6,8)」→ 校验器判 error（118 条）。
/// 修法是在 `GameState::flag_views()` 里统一归一化坐标，观察快照与回放帧共用。
#[test]
fn carried_flag_view_coordinates_follow_carrier() {
    let (mut state, map, rules) = flat_state(2);
    let flag_pos = Coord::new(8, 8);
    place(&mut state, 1, flag_pos);
    state.begin_tick(&rules); // 给单位 AP，才能拾旗/移动

    let flag_id = state.alloc_flag_id();
    state.flags.push(Flag {
        id: flag_id,
        pos: flag_pos,
        carrier: None,
    });
    crate::flag::resolve_pickups(&mut state, &[cmd(1, Action::PickFlag)]);
    assert_eq!(
        state.unit(1).expect("1 号存在").carrying_flag,
        Some(flag_id),
        "拾旗必须建立携带关系"
    );

    // 决策快照里旗已经跟着携带者（AI 需要靠这个判断旗在哪）。
    let config = MatchConfig::new(2, 7);
    let snapshot = state.observation(0, &config, &map);
    let seen = snapshot
        .flags
        .iter()
        .find(|flag| flag.id == flag_id)
        .expect("快照里应有这面旗");
    assert_eq!(seen.pos, flag_pos);
    assert_eq!(seen.carrier, Some(1));

    // 携带者移动后，回放帧里的旗坐标必须跟着走。
    let target = Coord::new(9, 8);
    crate::conflict::resolve_moves(&mut state, &map, &[cmd(1, Action::Move(Direction::Right))]);
    assert_eq!(state.unit(1).expect("1 号存在").pos, target, "移动应成功");
    let frame = state.take_frame();
    let carried = frame
        .flags
        .iter()
        .find(|flag| flag.id == flag_id)
        .expect("帧里应有这面旗");
    assert_eq!(carried.pos, target, "旗坐标必须等于携带者坐标");
    assert_eq!(carried.carrier, Some(1));
}

// ---------------------------------------------------------------- §14-13 炸弹

/// §14-13：放置后第 2 个 tick 爆炸、十字半径 2、有友伤、计时只出现 2/1。
#[test]
fn bomb_fuse_blast_radius_and_friendly_fire() {
    let (mut state, map, rules) = flat_state(2);
    let mut rng = Rng::new(6);
    place(&mut state, 1, Coord::new(10, 10)); // 放置者
    place(&mut state, 2, Coord::new(12, 10)); // 距离 2：友军也吃伤害
    place(&mut state, 4, Coord::new(13, 10)); // 距离 3：安全

    crate::bomb::resolve_placements(&mut state, &map, &rules, &[cmd(1, Action::PlaceBomb)]);
    assert_eq!(state.bombs.len(), 1);
    assert_eq!(state.bombs[0].timer, 2, "放置当回合 timer = 2");
    assert_eq!(ap(&state, 1), 1, "放炸弹消耗 1 AP");

    // 放置所在的 tick：倒计时阶段跳过这颗炸弹。
    crate::bomb::tick_and_explode(&mut state, &map, &rules, &mut rng);
    assert_eq!(state.bombs[0].view().timer, 2, "放置当回合不递减");

    // 下一个 tick：2 → 1，不爆炸。
    state.begin_tick(&rules);
    crate::bomb::tick_and_explode(&mut state, &map, &rules, &mut rng);
    assert_eq!(state.bombs[0].view().timer, 1);

    // 再下一个 tick：1 → 0，爆炸。
    state.begin_tick(&rules);
    crate::bomb::tick_and_explode(&mut state, &map, &rules, &mut rng);
    assert!(state.bombs.is_empty(), "引信到 0 必须爆炸并移除");
    assert_eq!(hp(&state, 2), 1, "半径内的友军也会被炸（友伤）");
    assert_eq!(hp(&state, 4), 3, "半径 2 之外不受影响");

    let blast = state
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::BombExploded {
                bomb,
                x,
                y,
                radius,
                hit_units,
            } => Some((*bomb, *x, *y, *radius, hit_units.clone())),
            _ => None,
        })
        .expect("应有 bomb_exploded 事件");
    assert_eq!((blast.1, blast.2, blast.3), (10, 10, 2));
    assert!(blast.4.contains(&2), "命中名单应含半径内的友军");
    assert!(!blast.4.contains(&4), "命中名单不应含半径外的单位");
}

/// §14-13（续）：墙阻挡爆炸传播。
#[test]
fn wall_stops_bomb_blast() {
    let (mut state, mut map, rules) = flat_state(2);
    let mut rng = Rng::new(7);
    place(&mut state, 1, Coord::new(10, 10));
    place(&mut state, 2, Coord::new(12, 10));
    let wall = map.index(11, 10).expect("(11,10) 在地图内");
    map.terrain[wall] = Terrain::Wall;

    crate::bomb::resolve_placements(&mut state, &map, &rules, &[cmd(1, Action::PlaceBomb)]);
    state.begin_tick(&rules);
    crate::bomb::tick_and_explode(&mut state, &map, &rules, &mut rng);
    state.begin_tick(&rules);
    crate::bomb::tick_and_explode(&mut state, &map, &rules, &mut rng);

    assert!(state.bombs.is_empty(), "炸弹应该已经爆炸");
    assert_eq!(hp(&state, 2), 3, "墙后面的单位不受伤害");
    let hit = state
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::BombExploded { hit_units, .. } => Some(hit_units.clone()),
            _ => None,
        })
        .expect("应有 bomb_exploded 事件");
    assert!(
        !hit.contains(&2),
        "墙阻挡后 (12,10) 不该出现在命中名单里（名单={hit:?}）"
    );
    // 放置者站在爆心，按规则同样吃 2 点伤害（炸弹不认自己人）。
    assert_eq!(hp(&state, 1), 1, "站在爆心的放置者也会被炸伤");
}

/// §14-13（续）：站在任意阵营格上的单位免疫爆炸伤害。
#[test]
fn bomb_does_not_hurt_units_standing_in_base() {
    let (mut state, map, rules) = flat_state(2);
    let mut rng = Rng::new(8);
    // team0 阵营区是 (1,1)-(3,3)；炸弹放在 (4,2)，向右传播恰好覆盖 (3,2)。
    place(&mut state, 1, Coord::new(4, 2));
    place(&mut state, 4, Coord::new(3, 2));
    assert!(
        map.in_any_base(3, 2).is_some(),
        "前提：(3,2) 必须是阵营格"
    );

    crate::bomb::resolve_placements(&mut state, &map, &rules, &[cmd(1, Action::PlaceBomb)]);
    state.begin_tick(&rules);
    crate::bomb::tick_and_explode(&mut state, &map, &rules, &mut rng);
    state.begin_tick(&rules);
    crate::bomb::tick_and_explode(&mut state, &map, &rules, &mut rng);

    assert_eq!(hp(&state, 4), 3, "阵营格上的单位免疫爆炸");
    let hit = state
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::BombExploded { hit_units, .. } => Some(hit_units.clone()),
            _ => None,
        })
        .expect("应有 bomb_exploded 事件");
    assert!(!hit.contains(&4), "免疫的单位不应出现在命中名单里");
}

// ---------------------------------------------------------------- §14-14 整局可复现

/// 测试用 AI：只依赖 `Observation`，行为完全确定（不用随机数）。
///
/// 覆盖了四类动作，让整局跑动能真正走到攻击/拾旗/移动/放炸弹各条结算路径。
struct ScriptedAi {
    name: &'static str,
}

impl TeamAi for ScriptedAi {
    fn name(&self) -> &str {
        self.name
    }

    fn decide(&mut self, obs: &Observation) -> TeamActions {
        let mut actions = TeamActions::new();
        for (slot, unit_id) in obs.my_units.iter().enumerate() {
            let Some(unit) = obs.me(*unit_id) else {
                continue;
            };
            if !unit.alive {
                continue;
            }

            // 1) 脚边有自己的旗可拾 → 拾旗。
            if unit.carrying_flag.is_none()
                && obs
                    .flags
                    .iter()
                    .any(|flag| flag.carrier.is_none() && flag.pos == unit.pos)
            {
                actions.pick_flag(*unit_id);
                continue;
            }

            // 2) 3 格内有敌人 → 攻击（携旗时不能攻击，跳过）。
            let target = if unit.carrying_flag.is_some() {
                None
            } else {
                obs.enemies()
                    .find(|enemy| enemy.alive && enemy.pos.manhattan(unit.pos) <= 3)
                    .map(|enemy| enemy.id)
            };
            if let Some(target) = target {
                actions.attack(*unit_id, target);
                continue;
            }

            // 3) 站在敌人堆里 → 放炸弹（阵营内不允许，跳过）。
            if unit.ap_left >= 2 && obs.enemies().any(|enemy| enemy.pos.manhattan(unit.pos) <= 2) {
                actions.place_bomb(*unit_id);
                continue;
            }

            // 4) 其余情况按固定方向循环走，保证轨迹可复现。
            let dirs = [
                Direction::Up,
                Direction::Right,
                Direction::Down,
                Direction::Left,
            ];
            let dir = dirs[(obs.tick as usize + slot) % dirs.len()];
            actions.move_unit(*unit_id, dir);
        }
        actions
    }
}

/// 用固定配置跑一整局，返回回放 JSONL 文本。
fn run_scripted_match(seed: u64) -> String {
    let mut config = MatchConfig::new(2, seed);
    config.width = 15;
    config.height = 15;
    config.max_ticks = 60;
    let ais: Vec<Box<dyn TeamAi>> = vec![
        Box::new(ScriptedAi { name: "scripted-a" }),
        Box::new(ScriptedAi { name: "scripted-b" }),
    ];
    let mut sim = Sim::new(config, ais).expect("测试配置必须合法");
    let bundle = sim.run_to_end_with_replay();
    assert_eq!(bundle.end.ticks, 60, "应跑到 max_ticks");
    assert_eq!(bundle.frame_count() as u32, bundle.end.ticks);
    bundle.to_jsonl().expect("回放序列化不应失败")
}

/// §14-14：同配置跑两次，回放逐字节相同；并且结构符合 JSONL 契约。
#[test]
fn full_match_replay_is_byte_identical() {
    let first = run_scripted_match(4242);
    let second = run_scripted_match(4242);
    assert_eq!(first, second, "同配置两次运行必须逐字节相同");

    assert!(first.ends_with('\n'), "最后一行也要有换行符");
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines.len(), 1 + 60 + 1, "init + 60 帧 + end");
    assert!(lines[0].contains("\"type\":\"init\""));
    assert!(lines[1].contains("\"type\":\"frame\""));
    assert!(lines[lines.len() - 1].contains("\"type\":\"end\""));
}

/// 不同种子应产生不同回放（否则「种子」形同虚设）。
#[test]
fn different_seeds_produce_different_replays() {
    assert_ne!(run_scripted_match(1), run_scripted_match(2));
}

/// `init` 行与 `end` 行的 AI 名必须同源（都来自 `MatchConfig.ai_names`）。
///
/// 这个一致性是回归测试：曾经 `init_line()` 用 `ai.name()`、`outcome()` 用 `config.ai_names`，
/// 于是示例程序里 init 报 "greedy-0" 而 end 报 "ai0"。统计/复盘都按下标对齐，
/// 两个端点不一致会静默错位；docs/internal-api.md 也明确该字段要「写进 init/结果」。
#[test]
fn init_and_end_report_ai_names_from_config() {
    let mut config = MatchConfig::new(2, 31);
    config.width = 15;
    config.height = 15;
    config.max_ticks = 5;
    config.ai_names = vec!["alpha".to_string(), "beta".to_string()];

    // 故意让实例自报名与配置不同，验证配置优先。
    let ais: Vec<Box<dyn TeamAi>> = vec![
        Box::new(ScriptedAi { name: "instance-a" }),
        Box::new(ScriptedAi { name: "instance-b" }),
    ];
    let mut sim = Sim::new(config.clone(), ais).expect("测试配置必须合法");
    let init = sim.init_line();
    assert_eq!(init.teams[0].ai_name, "alpha");
    assert_eq!(init.teams[1].ai_name, "beta");
    let result = sim.run_to_end();
    assert_eq!(result.ai_names, vec!["alpha".to_string(), "beta".to_string()]);

    // 配置留空时退回实例自报名，避免空名字。
    let mut blank = config;
    blank.ai_names = vec![String::new(), String::new()];
    let ais: Vec<Box<dyn TeamAi>> = vec![
        Box::new(ScriptedAi { name: "instance-a" }),
        Box::new(ScriptedAi { name: "instance-b" }),
    ];
    let sim = Sim::new(blank, ais).expect("测试配置必须合法");
    assert_eq!(sim.init_line().teams[0].ai_name, "instance-a");
}

/// `play` 与手写 `Sim` + `run_to_end` 的结果一致；`collect_replay` 只影响是否返回回放。
#[test]
fn play_matches_run_to_end_and_respects_collect_replay() {
    let mut config = MatchConfig::new(2, 99);
    config.width = 15;
    config.height = 15;
    config.max_ticks = 30;

    let make_ais = || -> Vec<Box<dyn TeamAi>> {
        vec![
            Box::new(ScriptedAi { name: "scripted-a" }),
            Box::new(ScriptedAi { name: "scripted-b" }),
        ]
    };

    let mut sim = Sim::new(config.clone(), make_ais()).expect("配置合法");
    let expected = sim.run_to_end();

    let with_replay = crate::play(config.clone(), make_ais(), true).expect("配置合法");
    assert_eq!(with_replay.result, expected, "结果必须与 run_to_end 一致");
    let replay = with_replay.replay.expect("collect_replay = true 时应返回回放");
    assert_eq!(replay.end, expected);
    assert_eq!(replay.init.max_ticks, 30);
    assert_eq!(replay.init.center_radius, config.rules.flag_capture_radius);

    let without_replay = crate::play(config, make_ais(), false).expect("配置合法");
    assert_eq!(without_replay.result, expected);
    assert!(without_replay.replay.is_none(), "未请求回放时不应返回");
}

/// 构造期校验：队伍数非法 / AI 数量不匹配 / 不支持的地图版本都要返回错误而不是 panic。
#[test]
fn sim_new_rejects_invalid_configuration() {
    let ais = || -> Vec<Box<dyn TeamAi>> {
        vec![
            Box::new(ScriptedAi { name: "a" }),
            Box::new(ScriptedAi { name: "b" }),
        ]
    };

    let bad_teams = MatchConfig::new(4, 1);
    assert!(matches!(
        Sim::new(bad_teams, ais()),
        Err(crate::SimError::BadTeamCount(4))
    ));

    let mismatch = MatchConfig::new(3, 1);
    assert!(matches!(
        Sim::new(mismatch, ais()),
        Err(crate::SimError::AiCountMismatch {
            got: 2,
            expected: 3
        })
    ));

    let mut bad_version = MatchConfig::new(2, 1);
    bad_version.map_gen_version = mapgen::MAP_GEN_VERSION + 1;
    assert!(matches!(
        Sim::new(bad_version, ais()),
        Err(crate::SimError::UnsupportedMapGenVersion(_))
    ));
}

// ------------------------------------------------- 补充：规则边界的定点回归测试

/// §7-5：不动点迭代的两种非平凡情形 —— 链式阻挡全失败、环状移动全成功。
///
/// 这两种情况是「先假设都成功，再反复剔除失败者」这个算法的试金石：
/// 单看「交换位置」会漏掉链式传播（A 的失败会连锁让上游失败）。
#[test]
fn conflict_chain_fails_while_cycle_succeeds() {
    // 链式阻挡：A(5,5)→(6,5) 被 B 占用，B(6,5)→(7,5) 被 C 占用，C 原地不动。
    // 不动点：B 不会离开 → B 失败 → A 的目标是「不会离开的 B」→ A 也失败。
    let (mut state, map, _) = flat_state(2);
    place(&mut state, 1, Coord::new(5, 5));
    place(&mut state, 2, Coord::new(6, 5));
    place(&mut state, 3, Coord::new(7, 5));
    resolve_moves(
        &mut state,
        &map,
        &[
            cmd(1, Action::Move(Direction::Right)),
            cmd(2, Action::Move(Direction::Right)),
        ],
    );
    assert_eq!(state.unit(1).expect("1 号存在").pos, Coord::new(5, 5), "A 失败留原地");
    assert_eq!(state.unit(2).expect("2 号存在").pos, Coord::new(6, 5), "B 失败留原地");
    assert_eq!(ap(&state, 1), 2, "失败不消耗 AP");
    assert_eq!(ap(&state, 2), 2, "失败不消耗 AP");
    assert!(
        !state
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::MoveConflict { .. })),
        "单请求者的失败不是冲突事件"
    );

    // 环状移动（4 格方环，四面各一个单位）：每个目标格上的占用者都会成功离开 → 全部成功。
    // 注意 4 邻域网格是二分图，不存在长度 3 的环，所以用 2×2 方块。
    let (mut state, map, _) = flat_state(2);
    place(&mut state, 1, Coord::new(5, 5));
    place(&mut state, 2, Coord::new(6, 5));
    place(&mut state, 4, Coord::new(6, 6));
    place(&mut state, 5, Coord::new(5, 6));
    resolve_moves(
        &mut state,
        &map,
        &[
            cmd(1, Action::Move(Direction::Right)),
            cmd(2, Action::Move(Direction::Down)),
            cmd(4, Action::Move(Direction::Left)),
            cmd(5, Action::Move(Direction::Up)),
        ],
    );
    assert_eq!(state.unit(1).expect("1 号存在").pos, Coord::new(6, 5));
    assert_eq!(state.unit(2).expect("2 号存在").pos, Coord::new(6, 6));
    assert_eq!(state.unit(4).expect("4 号存在").pos, Coord::new(5, 6));
    assert_eq!(state.unit(5).expect("5 号存在").pos, Coord::new(5, 5));
    for unit in [1, 2, 4, 5] {
        assert_eq!(ap(&state, unit), 1, "{unit} 号成功移动应消耗 1 AP");
    }
}

/// §3：同 tick 两个单位依次攻击同一目标，后手打到尸体 → 失败并记非法动作。
///
/// 规则明确「按行动顺序依次结算」，所以第二次攻击必须能看到前一次造成的死亡，
/// 而不是两个人都按 tick 开头的快照结算。
#[test]
fn second_attack_on_a_corpse_is_illegal() {
    let (mut state, map, rules) = flat_state(2);
    let mut rng = Rng::new(11);
    place(&mut state, 1, Coord::new(9, 10));
    place(&mut state, 2, Coord::new(8, 10));
    place(&mut state, 4, Coord::new(10, 10));
    state.unit_mut(4).expect("4 号存在").hp = 1;

    resolve_attacks(
        &mut state,
        &map,
        &rules,
        &mut rng,
        &[cmd(1, Action::Attack(4)), cmd(2, Action::Attack(4))],
    );

    assert!(!is_alive(&state, 4), "第一次攻击就该击杀");
    assert_eq!(state.kills[0], 1, "击杀数归攻击者队伍");
    assert_eq!(state.deaths[1], 1, "死亡数归受害者队伍");
    assert_eq!(ap(&state, 1), 1, "命中消耗 1 AP");
    assert_eq!(ap(&state, 2), 2, "打尸体失败不消耗 AP");
    assert!(
        illegal_reasons(&state).iter().any(|r| r.contains("目标已死亡")),
        "第二次攻击应记非法动作（原因={:?}）",
        illegal_reasons(&state)
    );
}

/// §5：已经携带一面旗的单位不能再拾第二面（第二面按非法动作，不消耗 AP）。
#[test]
fn carrying_unit_cannot_pick_a_second_flag() {
    let (mut state, map, _) = flat_state(2);
    let (unit, pos) = (1, Coord::new(8, 8));
    place(&mut state, unit, pos);

    // 手上已经有一面旗。
    let carried = state.alloc_flag_id();
    state.flags.push(Flag {
        id: carried,
        pos,
        carrier: Some(unit),
    });
    state.unit_mut(unit).expect("1 号存在").carrying_flag = Some(carried);
    // 脚下还有一面没人拿的旗。
    let ground = state.alloc_flag_id();
    state.flags.push(Flag {
        id: ground,
        pos,
        carrier: None,
    });

    crate::flag::resolve_pickups(&mut state, &[cmd(unit, Action::PickFlag)]);

    assert_eq!(
        state.unit(unit).expect("1 号存在").carrying_flag,
        Some(carried),
        "仍然只携带原来那面旗"
    );
    assert_eq!(state.flags[1].carrier, None, "地上的旗不该被拿起");
    assert_eq!(ap(&state, unit), 2, "非法拾旗不消耗 AP");
    assert!(
        illegal_reasons(&state)
            .iter()
            .any(|reason| reason.contains("已携带一面旗")),
        "应记非法动作（原因={:?}）",
        illegal_reasons(&state)
    );
    assert!(
        !state
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::FlagPicked { .. })),
        "不应产生 flag_picked 事件"
    );

    // `map` 只是为了让 `flat_state` 的类型推断与后续断言可用；此处不再移动。
    let _ = map;
}

/// §4：炸弹可以同格堆叠，各自独立倒计时，同一 tick 一起爆炸。
#[test]
fn bombs_stack_on_one_cell_and_explode_independently() {
    let (mut state, map, rules) = flat_state(2);
    let mut rng = Rng::new(12);
    place(&mut state, 1, Coord::new(10, 10));

    // 同一 tick 用 2 AP 连放两颗（规则只限制「放炸弹 1 AP」，没有每回合次数限制）。
    crate::bomb::resolve_placements(
        &mut state,
        &map,
        &rules,
        &[cmd(1, Action::PlaceBomb), cmd(1, Action::PlaceBomb)],
    );
    assert_eq!(state.bombs.len(), 2, "同格可以堆叠两颗炸弹");
    assert_ne!(state.bombs[0].id, state.bombs[1].id, "炸弹 id 必须不同");
    assert!(state.bombs.iter().all(|bomb| bomb.timer == 2), "初始倒计时都是 2");
    assert_eq!(ap(&state, 1), 0, "两颗炸弹消耗 2 AP");

    // 放置当 tick 不递减（引擎里倒计时阶段与放置阶段同属一个 tick，靠 `placed_tick` 跳过）；
    // 之后每过一个 tick 各减 1，第二个 tick 一起爆炸。
    state.begin_tick(&rules);
    crate::bomb::tick_and_explode(&mut state, &map, &rules, &mut rng);
    assert!(state.bombs.iter().all(|bomb| bomb.timer == 1), "回放里只应看到 1");
    state.begin_tick(&rules);
    crate::bomb::tick_and_explode(&mut state, &map, &rules, &mut rng);

    assert!(state.bombs.is_empty(), "两颗都已爆炸");
    let explosions: Vec<Vec<EntityId>> = state
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::BombExploded { hit_units, .. } => Some(hit_units.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(explosions.len(), 2, "应有两次 bomb_exploded");
    assert!(
        explosions.iter().all(|hit| hit.contains(&1)),
        "站在爆心的放置者被两颗都炸到（伤害 2+2 → 死亡）"
    );
    assert!(!is_alive(&state, 1), "累计 4 点伤害超过 3 点生命");
}

/// §8：胜负判定的各条边界（唯一最高分 / 并列平局 / 击杀再比较 / 仍并列）。
#[test]
fn winner_rules_cover_ties_and_kill_tiebreak() {
    use crate::WinCondition;

    let highest = WinCondition::HighestScore;
    let then_kills = WinCondition::HighestScoreThenKills;

    // 唯一最高分 → 胜者。
    assert_eq!(crate::engine::decide_winner(&[1, 0], &[0, 0], 2, highest), Some(0));
    // 并列最高 → 平局（默认规则不比击杀）。
    assert_eq!(crate::engine::decide_winner(&[1, 1], &[9, 0], 2, highest), None);
    // 全场 0 分 → 平局。
    assert_eq!(crate::engine::decide_winner(&[0, 0], &[0, 0], 2, highest), None);
    // 三队：唯一最高。(1 队 2 分)
    assert_eq!(crate::engine::decide_winner(&[0, 2, 1], &[0, 0, 0], 3, highest), Some(1));
    // 三队：0 队与 2 队并列最高 → 平局。
    assert_eq!(crate::engine::decide_winner(&[2, 0, 2], &[0, 0, 0], 3, highest), None);

    // HighestScoreThenKills：只在并列集团内部比击杀。
    assert_eq!(
        crate::engine::decide_winner(&[1, 1], &[2, 3], 2, then_kills),
        Some(1),
        "分数并列、击杀更多者胜"
    );
    assert_eq!(
        crate::engine::decide_winner(&[1, 1], &[2, 2], 2, then_kills),
        None,
        "分数与击杀都并列 → 平局"
    );
    assert_eq!(
        crate::engine::decide_winner(&[3, 0, 0], &[0, 5, 6], 3, then_kills),
        Some(0),
        "分数领先者不必比击杀"
    );
    // 分数领先集团外的击杀数再高也不影响判定。
    assert_eq!(
        crate::engine::decide_winner(&[2, 2, 0], &[0, 0, 99], 3, then_kills),
        None,
        "2 队虽然击杀最多，但它不在并列集团内"
    );
}

/// §6：掉落兜底阶梯 —— 3×3 堵死 → 5×5 → 中心区域 → 死亡格本身 → 退场。
///
/// 四级都要能被真实触发，这也是「随机池必须排除死亡格本身」的回归护栏。
#[test]
fn flag_drop_falls_back_to_wider_ring_then_center() {
    let (mut state, mut map, _) = flat_state(2);
    let mut rng = Rng::new(13);
    let origin = Coord::new(4, 4); // 远离中心区 (10,10) 与两个阵营区
    let flag_id = state.alloc_flag_id();
    state.flags.push(Flag {
        id: flag_id,
        pos: origin,
        carrier: Some(1),
    });
    state.unit_mut(1).expect("1 号存在").carrying_flag = Some(flag_id);

    // 第一圈（3×3）全堵成墙。
    for dy in -1..=1 {
        for dx in -1..=1 {
            let idx = map.index(origin.x + dx, origin.y + dy).expect("圈内坐标合法");
            map.terrain[idx] = Terrain::Wall;
        }
    }
    crate::flag::drop_carried_flag(&mut state, &map, &mut rng, origin, flag_id);

    let dropped = state.flags.iter().find(|flag| flag.id == flag_id).expect("旗应还在场");
    let (dx, dy) = ((dropped.pos.x - origin.x).abs(), (dropped.pos.y - origin.y).abs());
    assert_eq!(dx.max(dy), 2, "3×3 全堵死 → 落到 5×5 外圈（切比雪夫距离 2），实际 {:?}", dropped.pos);
    assert!(map.is_walkable(dropped.pos.x, dropped.pos.y), "落点必须可通行");
    assert!(map.in_any_base(dropped.pos.x, dropped.pos.y).is_none(), "落点不能在阵营区");
    assert!(
        state
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::FlagDropped { flag, .. } if *flag == flag_id)),
        "应产生 flag_dropped 事件"
    );

    // 再把 5×5 也堵死 → 只能落到中心区域（地图中心 (10,10)，半径 4）。
    for dy in -2..=2 {
        for dx in -2..=2 {
            let idx = map.index(origin.x + dx, origin.y + dy).expect("圈内坐标合法");
            map.terrain[idx] = Terrain::Wall;
        }
    }
    crate::flag::drop_carried_flag(&mut state, &map, &mut rng, origin, flag_id);
    let dropped = state.flags.iter().find(|flag| flag.id == flag_id).expect("旗应还在场");
    assert!(
        map.in_center_region(dropped.pos.x, dropped.pos.y),
        "两圈都堵死 → 落到中心区域，实际 {:?}",
        dropped.pos
    );

    // 第三级兜底：把 5×5 全堵成墙、只留下死亡格本身，并把中心区域缩到「只剩中心格且是墙」
    // → 前三级全空 → 只能落回死亡格本身（规则 §6「全失败 → 放死亡格本身」）。
    // 注意上一级把死亡格也算进 5×5 一起堵死了，这里必须先把它恢复成可通行。
    let origin_idx = map.index(origin.x, origin.y).expect("死亡格合法");
    map.terrain[origin_idx] = Terrain::Empty;
    for dy in -2..=2 {
        for dx in -2..=2 {
            if dx == 0 && dy == 0 {
                continue; // 死亡格本身留空，供第三级兜底命中
            }
            let idx = map.index(origin.x + dx, origin.y + dy).expect("圈内坐标合法");
            map.terrain[idx] = Terrain::Wall;
        }
    }
    let center = map.center;
    map.center_radius = 0; // 中心区域退化为「只有中心那一格」
    let center_idx = map.index(center.x, center.y).expect("中心格合法");
    map.terrain[center_idx] = Terrain::Wall; // 再把中心格堵死 → 中心候选为空
    state.events.clear();
    crate::flag::drop_carried_flag(&mut state, &map, &mut rng, origin, flag_id);
    let dropped = state.flags.iter().find(|flag| flag.id == flag_id).expect("旗应还在场");
    assert_eq!(dropped.pos, origin, "前三级的合法格全被堵死 → 落回死亡格本身");
    assert!(
        state
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::FlagDropped { flag, .. } if *flag == flag_id)),
        "落回死亡格同样是掉落，要发 flag_dropped"
    );

    // 第四级：连死亡格本身都不合法（变成墙）→ 该旗暂时退场（从 flags 移除且不发事件）。
    map.terrain[origin_idx] = Terrain::Wall;
    state.events.clear();
    crate::flag::drop_carried_flag(&mut state, &map, &mut rng, origin, flag_id);
    assert!(
        state.flags.iter().all(|flag| flag.id != flag_id),
        "全失败 → 该旗暂时退场，不写进任何帧"
    );
    assert!(
        !state
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::FlagDropped { flag, .. } if *flag == flag_id)),
        "退场的旗不发 flag_dropped 事件"
    );
}
