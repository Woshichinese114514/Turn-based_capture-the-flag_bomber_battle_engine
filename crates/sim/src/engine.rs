//! 引擎：`Sim` 的 9 阶段 tick 流水线、胜负判定与 `play` 便捷入口（`docs/rules.md` §8、§9）。
//!
//! ## tick 的 9 个阶段（严格按规则 §9 的顺序）
//!
//! ```text
//! 1. 清理与重置：清 attacked_this_turn、存活单位 AP = ap_per_unit、死亡单位 AP = 0
//! 2. AI 决策：按 (tick + slot) % teams 的轮换顺序，每队拿「决策前快照」决定动作
//! 3. 统一结算：移动（§7 冲突） → 攻击 → 放炸弹 → 拾旗
//! 4. 炸弹：所有既有炸弹倒计时减 1，减到 0 立即爆炸
//! 5. 复活：死亡倒计时减 1，减到 0 在己方阵营找空格满血复活（满员则延后重试）
//! 6. 旗刷新检查：每 flag_spawn_interval 个 tick 检查一次
//! 7. 得分检查：携带者站在己方阵营 → 队伍 +1、旗消失
//! 8. 结束检查：达到 max_ticks，或（可选）只剩一队存活
//! 9. 产出 RenderFrame
//! ```
//!
//! 之所以把「顺序」写得这么死：它决定了同一个 tick 里「移动后能否拾旗」「攻击后能否继续动」
//! 「炸弹先炸还是先复活」这些边界行为。任何调整都等于改规则，必须同步改 `docs/rules.md`。
//!
//! ## 决策顺序与行动顺序
//!
//! 第 2 步的轮换顺序同时定义了第 3 步的**行动顺序**：指令按队伍轮换顺序拼接成一个
//! `Vec<UnitCommand>`，攻击结算就按这个顺序逐条判定（规则 §3.3「后手打到尸体 → 非法动作」）。
//! 所有队伍看到的是**同一个** tick 开始时的快照，因此不存在「先决策者占便宜」的视野差。
//!
//! ## 结束时的事件不能丢
//!
//! 第 7 步的得分、第 4 步的爆炸都可能发生在最后一个 tick。因此 `step` 先跑完整套阶段、
//! 再判定结束并返回该 tick 的帧；`finished` 标记只在**帧构造之后**置位，
//! 保证最后 `max_ticks` 的帧里含全部事件（回放校验要求 events 与帧一致）。

use mapgen::{generate_versioned, is_supported_version, MapData, MapSpec, Rng};
use protocol::versions::{ENGINE_VERSION, RULES_VERSION};
use protocol::{Coord, EntityId, GameEvent, MatchResult, RenderFrame, ReplayInit, TeamId, TeamInfo};

use crate::ai::{Observation, TeamAi};
use crate::config::{MatchConfig, RulesConfig, SimError, WinCondition};
use crate::state::GameState;

/// 一场对局的模拟器。
///
/// 生命周期：`new` 生成地图与初始状态 → 反复 `step`（或直接 `run_to_end`）→ `outcome`。
/// 所有状态都在本结构内部，没有全局变量，因此两个 `Sim` 互不影响（并行跑分的前提）。
pub struct Sim {
    /// 输入配置（含规则参数与地图尺寸）。
    config: MatchConfig,
    /// 生成好的地图（本 tick 内只读）。
    map: MapData,
    /// 内部实体状态。
    state: GameState,
    /// 每队的 AI 实例，下标 = 队伍号。
    ais: Vec<Box<dyn TeamAi>>,
    /// 整局唯一的随机源（规则 §9 随机性纪律）。
    rng: Rng,
    /// 对局是否已结束（达到 tick 上限或满足可选结束条件）。
    finished: bool,
    /// 累计产出的帧，供 `run_to_end_with_replay` 使用。
    ///
    /// 上限只有 `max_ticks`（标准 300）帧，占用可忽略；一直收集可以让「先 `step` 几步
    /// 再 `run_to_end_with_replay`」也拿到完整回放，比事后拼装更不容易出错。
    frames: Vec<RenderFrame>,
    /// 回放 `init` 行（构造后不再变化，存一份避免重复拼装）。
    init: ReplayInit,
}

impl Sim {
    /// 构造一局：校验配置 → 生成地图 → 建初始状态。
    ///
    /// 错误全部是配置级问题（队伍数、AI 数量、地图版本、地图自校验），
    /// 因此都在这里一次性暴露，结算期不再返回 `Result`。
    pub fn new(config: MatchConfig, ais: Vec<Box<dyn TeamAi>>) -> Result<Self, SimError> {
        // 队伍数：规则目前只定义 2 或 3（协议预留到 4，但规则缺位时不做假设）。
        if config.teams < 2 || config.teams > 3 {
            return Err(SimError::BadTeamCount(config.teams));
        }
        if ais.len() != config.teams as usize {
            return Err(SimError::AiCountMismatch {
                got: ais.len(),
                expected: config.teams as usize,
            });
        }
        if !is_supported_version(config.map_gen_version) {
            return Err(SimError::UnsupportedMapGenVersion(config.map_gen_version));
        }
        let spec = MapSpec {
            seed: config.seed,
            width: config.width,
            height: config.height,
            teams: config.teams,
            center_radius: config.rules.flag_capture_radius,
            map_gen_version: config.map_gen_version,
            ..MapSpec::default()
        };
        let map = generate_versioned(config.map_gen_version, &spec)?;
        // 用协议自带的校验器再验一遍：地图要写进回放 init 行，宁可在这里失败。
        map.to_map_init().validate().map_err(SimError::BadMap)?;

        let state = GameState::new(config.teams, &config.rules, &map);
        let rng = Rng::new(config.seed);
        let init = build_init(&config, &map, &ais);

        Ok(Self {
            config,
            map,
            state,
            ais,
            rng,
            finished: false,
            frames: Vec::new(),
            init,
        })
    }

    /// 只读配置。
    pub fn config(&self) -> &MatchConfig {
        &self.config
    }

    /// 回放 `init` 行（含地图、队伍与规则参数）。
    pub fn init_line(&self) -> ReplayInit {
        self.init.clone()
    }

    /// 已结算的 tick 数（初始 0）。
    pub fn current_tick(&self) -> u32 {
        self.state.tick
    }

    /// 对局是否结束。
    pub fn is_over(&self) -> bool {
        self.finished
    }

    /// 推进一个 tick；对局已结束时返回 `None`。
    ///
    /// 返回的帧即规则 §9 第 9 步的产物：tick 号、比分、单位 / 旗 / 炸弹快照与本 tick 事件。
    pub fn step(&mut self) -> Option<RenderFrame> {
        if self.finished {
            return None;
        }
        let frame = self.run_tick();
        self.frames.push(frame.clone());
        Some(frame)
    }

    /// 跑完整局并返回结果（不取回放）。
    pub fn run_to_end(&mut self) -> MatchResult {
        while self.step().is_some() {}
        self.outcome()
    }

    /// 跑完整局并返回回放包（`init` + 全部帧 + `end`）。
    pub fn run_to_end_with_replay(&mut self) -> crate::ReplayBundle {
        while self.step().is_some() {}
        crate::ReplayBundle {
            init: self.init.clone(),
            frames: self.frames.clone(),
            end: self.outcome(),
        }
    }

    /// 当前结果快照。
    ///
    /// 与是否结束无关：未跑完时它就是「目前为止的比分与击杀」，方便调试。
    pub fn outcome(&self) -> MatchResult {
        MatchResult {
            match_index: self.config.match_index,
            seed: self.config.seed,
            map_gen_version: self.config.map_gen_version,
            ticks: self.state.tick,
            scores: self.state.scores.clone(),
            kills: self.state.kills.clone(),
            deaths: self.state.deaths.clone(),
            winner: self.decide_winner(),
            ai_names: self.config.ai_names.clone(),
        }
    }

    /// 执行一个完整 tick 的 9 个阶段，返回该 tick 的帧。
    fn run_tick(&mut self) -> RenderFrame {
        // ---- 1. 清理与重置 ----
        self.state.begin_tick(&self.config.rules);
        let tick = self.state.tick;

        // ---- 2. AI 决策（轮换顺序，(tick + slot) % teams）----
        let mut commands = Vec::new();
        let teams = self.config.teams;
        for slot in 0..teams {
            let team = ((tick as usize + slot as usize) % teams as usize) as TeamId;
            // 决策前快照：此时本 tick 还没有任何动作被结算。
            let obs: Observation = self.state.observation(team, &self.config, &self.map);
            let actions = self.ais[team as usize].decide(&obs);
            commands.extend(actions.commands);
        }

        // ---- 3. 统一结算：移动 → 攻击 → 放炸弹 → 拾旗 ----
        // 每个子阶段内部都会重新检查 AP 与前置条件，因此顺序不可交换。
        crate::conflict::resolve_moves(&mut self.state, &self.map, &commands);
        crate::combat::resolve_attacks(
            &mut self.state,
            &self.map,
            &self.config.rules,
            &mut self.rng,
            &commands,
        );
        crate::bomb::resolve_placements(&mut self.state, &self.map, &self.config.rules, &commands);
        crate::flag::resolve_pickups(&mut self.state, &commands);

        // ---- 4. 炸弹倒计时与爆炸 ----
        crate::bomb::tick_and_explode(&mut self.state, &self.map, &self.config.rules, &mut self.rng);

        // ---- 5. 复活 ----
        resolve_respawns(&mut self.state, &self.map, &self.config.rules);

        // ---- 6. 旗刷新 ----
        crate::flag::maybe_spawn_flag(
            &mut self.state,
            &self.map,
            &self.config.rules,
            self.config.teams,
            &mut self.rng,
        );

        // ---- 7. 得分 ----
        crate::flag::check_scores(&mut self.state, &self.map);

        // ---- 8. 结束检查（必须在取帧之前判定，但帧仍要返回）----
        if self.should_finish() {
            self.finished = true;
        }

        // ---- 9. 帧 ----
        self.state.take_frame()
    }

    /// 结束条件：达到 tick 上限，或启用了 `all_dead_loses` 且只剩一队有存活单位。
    fn should_finish(&self) -> bool {
        if self.state.tick >= self.config.max_ticks {
            return true;
        }
        if !self.config.all_dead_loses {
            return false;
        }
        let alive_teams = (0..self.config.teams)
            .filter(|team| self.state.alive_count(*team) > 0)
            .count();
        alive_teams <= 1
    }

    /// 按 `win_condition` 判定胜者（委托给纯函数 [`decide_winner`]，便于单测覆盖各种并列组合）。
    fn decide_winner(&self) -> Option<TeamId> {
        decide_winner(
            &self.state.scores,
            &self.state.kills,
            self.config.teams,
            self.config.win_condition,
        )
    }
}

/// 按 `win_condition` 从分数/击杀数组里判定胜者。
///
/// * `HighestScore`：最高分**唯一**才产生胜者；并列最高 → 平局（`None`）。
/// * `HighestScoreThenKills`：先比分数，并列时再比击杀数；击杀仍并列 → 平局。
///
/// 注意「分数为 0 的全场平局」与「有队伍得分为负」都不特殊处理：
/// 只要最高分是唯一的就有胜者，否则平局。
///
/// 抽成纯函数是因为胜负判定全是边界情况（并列、部分并列、击杀再并列），
/// 直接对数组做单测比「构造一整局打到恰好并列」可靠得多。
pub(crate) fn decide_winner(
    scores: &[i32],
    kills: &[u32],
    teams: u8,
    condition: WinCondition,
) -> Option<TeamId> {
    let best_score = *scores.iter().max()?;
    let score_leaders: Vec<usize> = (0..teams as usize)
        .filter(|team| scores.get(*team) == Some(&best_score))
        .collect();
    if score_leaders.len() == 1 {
        return Some(score_leaders[0] as TeamId);
    }
    if condition == WinCondition::HighestScore {
        return None;
    }
    // HighestScoreThenKills：只在分数领先集团内部比击杀。
    let best_kills = score_leaders
        .iter()
        .filter_map(|team| kills.get(*team))
        .copied()
        .max()?;
    let kill_leaders: Vec<usize> = score_leaders
        .into_iter()
        .filter(|team| kills.get(*team) == Some(&best_kills))
        .collect();
    if kill_leaders.len() == 1 {
        Some(kill_leaders[0] as TeamId)
    } else {
        None
    }
}

/// 便捷入口：构造 → 跑完 → 可选带回放。
///
/// `collect_replay == false` 时只是不返回回放包；为了让两条路径的行为完全一致，
/// 帧仍然照常产出（回放收集不影响任何随机数推进 —— 这一点是测试 14 的前提）。
pub fn play(
    config: MatchConfig,
    ais: Vec<Box<dyn TeamAi>>,
    collect_replay: bool,
) -> Result<PlayOutcome, SimError> {
    let mut sim = Sim::new(config, ais)?;
    if collect_replay {
        let replay = sim.run_to_end_with_replay();
        let result = sim.outcome();
        Ok(PlayOutcome {
            result,
            replay: Some(replay),
        })
    } else {
        let result = sim.run_to_end();
        Ok(PlayOutcome {
            result,
            replay: None,
        })
    }
}

/// `play` 的返回值：结果必有，回放视 `collect_replay` 而定。
pub struct PlayOutcome {
    /// 对局结果。
    pub result: MatchResult,
    /// 回放包（未请求时为 `None`）。
    pub replay: Option<crate::ReplayBundle>,
}

/// 复活阶段（规则 §9 第 5 步、§2）。
///
/// 语义细节：
/// * 死亡当 tick 就会进入这里的递减（`§9` 把第 5 步排在第 3 步之后），
///   因此 `respawn_ticks = 10` 时单位在死亡后第 10 个 tick 复活。
/// * 减到 0 时尝试在**己方阵营 3×3**内找空格：行优先取第一个没有被存活单位占据的格。
///   没有随机性 —— 规则没有要求随机复活点，确定性的选择让回放更稳定。
/// * 阵营满员：`respawn_timer` 保持 0，下一个 tick 继续尝试（不重置成 10）。
pub(crate) fn resolve_respawns(state: &mut GameState, map: &MapData, rules: &RulesConfig) {
    // 先递减所有死亡单位的倒计时。
    for unit in state.units_mut() {
        if unit.alive {
            continue;
        }
        if unit.respawn_timer > 0 {
            unit.respawn_timer -= 1;
        }
    }
    // 再处理到 0 的（可能与上一步在同一 tick 内，故分两轮，避免「刚减到 0 就复活」
    // 与「本 tick 才死亡」互相干扰；两轮都按单位 ID 升序）。
    let ready: Vec<EntityId> = state
        .units()
        .iter()
        .filter(|unit| !unit.alive && unit.respawn_timer == 0)
        .map(|unit| unit.id)
        .collect();
    for unit_id in ready {
        let Some(team) = state.unit(unit_id).map(|unit| unit.team) else {
            continue;
        };
        let Some(base) = map.base_of(team) else {
            continue;
        };
        let Some(pos) = find_free_base_cell(state, base, rules) else {
            // 阵营满员（或全部被占用）：保持在 0，下个 tick 再试。
            continue;
        };
        state.respawn_unit(unit_id, pos, rules.unit_max_hp);
        state.push_event(GameEvent::UnitRespawned {
            unit: unit_id,
            team,
            x: pos.x,
            y: pos.y,
        });
    }
}

/// 在阵营区（左上角 `base`，边长 `base_size`）内找第一个没有存活单位占据的格。
fn find_free_base_cell(state: &GameState, base: Coord, rules: &RulesConfig) -> Option<Coord> {
    let size = rules.base_size_i32();
    for dy in 0..size {
        for dx in 0..size {
            let pos = Coord::new(base.x + dx, base.y + dy);
            if state.alive_at(pos).is_none() {
                return Some(pos);
            }
        }
    }
    None
}

/// 拼装回放 `init` 行。
fn build_init(config: &MatchConfig, map: &MapData, ais: &[Box<dyn TeamAi>]) -> ReplayInit {
    let teams = (0..config.teams)
        .map(|team| {
            let base = map.base_of(team).unwrap_or(Coord::new(0, 0));
            TeamInfo {
                team_id: team,
                // AI 名以 `MatchConfig.ai_names` 为准：docs/internal-api.md 明确该字段
                // 「写进 init/结果」，两端必须同一来源，否则同一场对局 init 与 end
                // 会报出不同名字（统计/复盘按下标对齐时会错位）。
                // 只有当配置里该队名字为空串时，才退回实例自报的 `ai.name()`，
                // 这样嵌入方即使漏填也不会得到空名字。
                ai_name: config
                    .ai_names
                    .get(team as usize)
                    .filter(|name| !name.is_empty())
                    .cloned()
                    .or_else(|| ais.get(team as usize).map(|ai| ai.name().to_string()))
                    .unwrap_or_default(),
                base_x: base.x,
                base_y: base.y,
            }
        })
        .collect();
    ReplayInit {
        engine_version: ENGINE_VERSION,
        rules_version: RULES_VERSION,
        map_gen_version: config.map_gen_version,
        map: map.to_map_init(),
        teams,
        max_ticks: config.max_ticks,
        flag_spawn_interval: config.rules.flag_spawn_interval,
        center_radius: config.rules.flag_capture_radius,
        seed: config.seed,
    }
}
