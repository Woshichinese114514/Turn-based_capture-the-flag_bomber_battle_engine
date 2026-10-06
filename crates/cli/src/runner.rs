//! 批量对局的编排：校验 → 种子计划 → 并行跑局 → 单线程统一写盘。
//!
//! # 为什么并行与写盘要分开
//!
//! 并行写文件有两个坑：
//!
//! 1. **顺序不确定**：`matches.jsonl` 要求按局索引升序、回放按编号命名；
//!    如果每个线程自己 `fs::write`，文件内容虽然对，但目录顺序与日志顺序会变，
//!    更糟的是同一路径被两个线程同时写（不同局重名）会得到交错内容。
//! 2. **错误处理变复杂**：并行阶段一旦有线程 IO 失败，其余线程已经写了半份产物，
//!    留下「一半新一半旧」的目录——这种状态最难排查。
//!
//! 所以这里的分工是：**并行阶段只做纯计算，把结果与回放文本收集到内存；
//! 回到主线程后再按索引升序一次性写盘**。批量对局的开销主要在模拟本身，
//! 串行写盘（几百 KB 到几 MB）不是瓶颈。
//!
//! # 回放的 JSON 序列化放在并行阶段
//!
//! 回放转 JSON 文本是 CPU 工作（大回放尤其明显），而且不碰文件系统，
//! 所以放在 worker 里做；写盘时只剩 `fs::write`，天然满足「单线程统一写盘」。

use std::fs;
use std::path::{Path, PathBuf};

use protocol::versions::RULES_VERSION;
use protocol::MatchResult;
use rayon::prelude::*;
use scoring::{aggregate, ScoringReport, ScoringConfig};

use crate::args::{ReplaySample, RunArgs};
use crate::error::CliError;
use crate::manifest::{Manifest, ManifestConfig, MatchLine, SummaryFile, Versions};
use crate::seed::SeedPlan;
use crate::timefmt::{entropy_seed, format_rfc3339, now_unix_secs};

/// 回放样本大小：manifest 里只记录前几局的种子，避免 1000 局的 manifest 膨胀。
const SEED_SAMPLE: usize = 5;

/// 单局的计算产物（回放已在 worker 内序列化成文本，写盘阶段只做 IO）。
struct MatchOutcome {
    result: MatchResult,
    replay_jsonl: Option<String>,
}

/// 一次批量运行的完整产物（已写盘，返回供日志/测试断言）。
#[derive(Debug)]
pub struct BatchOutcome {
    /// 输出目录。
    pub out_dir: PathBuf,
    /// 评分汇总（与 summary.json 内容一致）。
    pub report: ScoringReport,
    /// 每局结果（按局索引升序）。
    pub results: Vec<MatchResult>,
    /// manifest.json 的结构化副本。
    pub manifest: Manifest,
    /// 实际写出的回放份数。
    pub replays_written: usize,
    /// 非致命提示。
    pub warnings: Vec<String>,
}

/// 跑完一整批对局并写出四件套。
pub fn run_batch(args: &RunArgs) -> Result<BatchOutcome, CliError> {
    let started = now_unix_secs();
    let mut warnings: Vec<String> = Vec::new();

    // ---- 1. 语义校验（语法校验已由 clap 完成）----
    validate_args(args, &mut warnings)?;

    let registry = ai::register_all();
    let ai_names = resolve_ai_names(&args.ai, args.teams, &registry)?;

    // ---- 2. 种子计划：必须在并行之前一次算完 ----
    let plan = SeedPlan::generate(
        args.seed_mode,
        args.matches,
        args.teams,
        args.base_seed,
        args.seed,
        entropy_seed(),
    )?;

    // ---- 3. 评分配置（默认权重来自 ScoringConfig，命令行只覆盖用户显式给的部分）----
    let scoring_config = build_scoring_config(args)?;
    if scoring_config.teams != args.teams {
        // ScoringConfig::for_teams 已经设过，这里只是防御性检查：
        // 一旦不一致，summary.json 标注的体系就会与实际对局不符。
        return Err(CliError::BadTeamCount(scoring_config.teams));
    }

    // ---- 4. 并行跑局（纯计算，不碰文件系统）----
    let pool = rayon::ThreadPoolBuilder::new();
    let pool = if args.jobs == 0 {
        pool
    } else {
        pool.num_threads(args.jobs)
    };
    let pool = pool.build().map_err(|e| CliError::ThreadPool {
        jobs: args.jobs,
        message: e.to_string(),
    })?;

    let indices: Vec<u32> = (0..args.matches).collect();
    let outcomes: Vec<MatchOutcome> = pool.install(|| {
        indices
            .par_iter()
            .map(|&index| run_one(index, args, &registry, &ai_names, &plan))
            .collect::<Result<Vec<_>, CliError>>()
    })?;

    // `par_iter().collect()` 对索引迭代器本来就保序，这里再显式按索引排序，
    // 让「matches.jsonl 升序」这条契约不依赖 rayon 的实现细节。
    let mut ordered: Vec<(u32, MatchOutcome)> = indices.into_iter().zip(outcomes).collect();
    ordered.sort_by_key(|(index, _)| *index);

    let mut results: Vec<MatchResult> = Vec::with_capacity(ordered.len());
    let mut replays: Vec<(u32, String)> = Vec::new();
    for (index, outcome) in ordered {
        results.push(outcome.result);
        if let Some(text) = outcome.replay_jsonl {
            replays.push((index, text));
        }
    }

    // ---- 5. 评分汇总 ----
    let report = aggregate(
        &results,
        args.teams,
        RULES_VERSION,
        args.map_gen_version,
        &scoring_config,
    );

    // ---- 6. 单线程统一写盘 ----
    let (created_time, created_time_unix) = resolve_created_time(args, started);
    let versions = Versions::new(args.map_gen_version);
    let manifest = Manifest {
        engine_version: versions.engine_version,
        rules_version: versions.rules_version,
        map_gen_version: versions.map_gen_version,
        versions: versions.clone(),
        created_time,
        created_time_unix,
        seed_mode: plan.mode.as_str().to_string(),
        base_seed: plan.base_seed,
        fixed_seed: plan.fixed_seed,
        seed_plan: plan.info(SEED_SAMPLE),
        config: ManifestConfig {
            matches: args.matches,
            max_ticks: args.max_ticks,
            teams: args.teams,
            ai_names: ai_names.clone(),
            map_width: args.map_width,
            map_height: args.map_height,
            units_per_team: args.units_per_team,
            jobs: args.jobs,
            replay_sample: args.replay_sample.as_str(),
            scoring: scoring_config.clone(),
        },
        warnings: warnings.clone(),
    };

    write_outputs(args, &plan, &manifest, &report, &results, &replays)?;

    Ok(BatchOutcome {
        out_dir: args.out.clone(),
        report,
        results,
        manifest,
        replays_written: replays.len(),
        warnings,
    })
}

/// 参数语义校验 + 收集非致命提示。
fn validate_args(args: &RunArgs, warnings: &mut Vec<String>) -> Result<(), CliError> {
    if args.matches == 0 {
        return Err(CliError::BadMatchCount(0));
    }
    if args.teams != 2 && args.teams != 3 {
        return Err(CliError::BadTeamCount(args.teams));
    }
    if args.map_width == 0 || args.map_height == 0 {
        return Err(CliError::BadMapSize {
            width: args.map_width,
            height: args.map_height,
        });
    }
    if args.map_gen_version != mapgen::MAP_GEN_VERSION {
        return Err(CliError::UnsupportedMapGenVersion {
            got: args.map_gen_version,
            supported: mapgen::MAP_GEN_VERSION,
        });
    }
    if args.units_per_team == 0 {
        return Err(CliError::BadUnitsPerTeam(args.units_per_team));
    }
    if args.units_per_team != 3 {
        warnings.push(format!(
            "--units-per-team {} 不是默认值 3：规则数值（HP/复活/炸弹）未针对该值调参，结果仅供探索",
            args.units_per_team
        ));
    }
    if !args.seed_mode.uses_fixed_seed() && args.seed.is_some() {
        warnings.push(format!(
            "已忽略 --seed：只有 fixed 模式使用它（本批使用 --seed-mode {}）",
            args.seed_mode
        ));
    }
    if !args.seed_mode.uses_base_seed() && args.base_seed.is_some() {
        warnings.push("已忽略 --base-seed：fixed 模式的地图种子本身就确定".to_string());
    }
    if args.max_ticks == 0 {
        return Err(CliError::BadMaxTicks(0));
    }
    if matches!(args.replay_sample, ReplaySample::None) {
        warnings.push("--replay-sample none：不会写出 replays/ 目录，单局复盘不可用".to_string());
    }
    Ok(())
}

/// 把 `--ai N=name` 解析成「每队一个 AI 名字」。
///
/// 规则：
/// * `N=name` 显式指定第 N 队；`N` 必须在 `0..teams` 内；
/// * 只写 `name` 时分配到当前最小的空闲队号（`--ai greedy_flag --ai defender` 这种写法）；
/// * 未指定的队伍用默认 AI（`ai::DEFAULT_AI`，即 random）；
/// * 同一队指定两次直接报错——静默覆盖会让用户以为参数生效了但其实没有。
fn resolve_ai_names(
    specs: &[String],
    teams: u8,
    registry: &ai::AiRegistry,
) -> Result<Vec<String>, CliError> {
    let mut slots: Vec<Option<String>> = vec![None; teams as usize];
    for spec in specs {
        let trimmed = spec.trim();
        let (index, name) = match trimmed.split_once('=') {
            Some((index_text, name)) => {
                let index: u8 = index_text
                    .trim()
                    .parse()
                    .map_err(|_| CliError::BadAiSpec(spec.clone()))?;
                if index >= teams {
                    return Err(CliError::AiIndexOutOfRange { index, teams });
                }
                (index, name.trim().to_string())
            }
            None => {
                let name = trimmed.to_string();
                let index = slots
                    .iter()
                    .position(Option::is_none)
                    .ok_or_else(|| CliError::BadAiSpec(spec.clone()))? as u8;
                (index, name)
            }
        };
        if name.is_empty() {
            return Err(CliError::BadAiSpec(spec.clone()));
        }
        if !registry.contains(&name) {
            return Err(CliError::AiNotRegistered {
                name,
                available: registry.names().join(", "),
            });
        }
        if slots[index as usize].is_some() {
            return Err(CliError::DuplicateAiSlot(index));
        }
        slots[index as usize] = Some(name);
    }
    Ok(slots
        .into_iter()
        .map(|slot| slot.unwrap_or_else(|| ai::DEFAULT_AI.to_string()))
        .collect())
}

/// 用命令行参数覆盖 `ScoringConfig` 的默认值（`teams` 顺带固定为本次对局队伍数）。
fn build_scoring_config(args: &RunArgs) -> Result<ScoringConfig, CliError> {
    let mut config = ScoringConfig::for_teams(args.teams)?;
    config.weight_win_rate = args.weight_win_rate;
    config.weight_score_share = args.weight_score_share;
    config.weight_kill_ratio = args.weight_kill_ratio;
    config.baseline_rating = args.baseline_rating;
    config.points_per_decade = args.points_per_decade;

    let weights = [
        config.weight_win_rate,
        config.weight_score_share,
        config.weight_kill_ratio,
    ];
    if weights.iter().any(|w| !w.is_finite() || *w < 0.0) {
        return Err(CliError::BadWeights(format!(
            "权重必须是有限非负数，得到 {weights:?}"
        )));
    }
    let sum: f64 = weights.iter().sum();
    if !(sum > 0.0) {
        return Err(CliError::BadWeights(
            "三个权重之和必须大于 0，否则 strength 恒等于 0.5，评分没有意义".to_string(),
        ));
    }
    if !config.baseline_rating.is_finite() || !config.points_per_decade.is_finite() {
        return Err(CliError::BadWeights(
            "--baseline-rating / --points-per-decade 必须是有限数".to_string(),
        ));
    }
    Ok(config)
}

/// 跑一局：构造本局配置与 AI 实例，调用 `sim::play`，并在 worker 内把回放序列化成文本。
fn run_one(
    index: u32,
    args: &RunArgs,
    registry: &ai::AiRegistry,
    ai_names: &[String],
    plan: &SeedPlan,
) -> Result<MatchOutcome, CliError> {
    let position = index as usize;
    let map_seed = plan.map_seed(position);
    let ai_seeds: Vec<u64> = (0..args.teams)
        .map(|team| plan.ai_seed(position, team))
        .collect();

    let mut ais: Vec<Box<dyn sim::TeamAi>> = Vec::with_capacity(ai_names.len());
    for (team, name) in ai_names.iter().enumerate() {
        let seed = ai_seeds.get(team).copied().unwrap_or(0);
        // 每个 AI 用「本局 + 本队」的种子新建实例：AI 不允许跨局共享状态，
        // 否则并行跑局时结果会依赖线程调度的顺序（不可复现）。
        let ai = registry
            .create(name, seed)
            .ok_or_else(|| CliError::AiNotRegistered {
                name: name.clone(),
                available: registry.names().join(", "),
            })?;
        ais.push(ai);
    }

    let mut config = sim::MatchConfig::new(args.teams, map_seed);
    config.match_index = index;
    config.ai_seeds = ai_seeds;
    config.ai_names = ai_names.to_vec();
    config.width = args.map_width;
    config.height = args.map_height;
    config.max_ticks = args.max_ticks;
    config.map_gen_version = args.map_gen_version;
    config.rules.units_per_team = args.units_per_team;

    let collect_replay = args.replay_sample.saves(position);
    let outcome = sim::play(config, ais, collect_replay)?;
    let replay_jsonl = match (collect_replay, outcome.replay) {
        (true, Some(bundle)) => Some(bundle.to_jsonl()?),
        (true, None) => return Err(CliError::ReplayMissing { index }),
        (false, _) => None,
    };
    Ok(MatchOutcome {
        result: outcome.result,
        replay_jsonl,
    })
}

/// `--created-time` 的三种语义：
/// * 未给 → 当前时间（RFC3339 + Unix 秒）；
/// * `none` → 两个字段都省略（逐字节可复现的回归测试用）；
/// * 其他文本 → 原样写入，不写 Unix 秒（用户给的是「事实」，不该被我们解释）。
fn resolve_created_time(args: &RunArgs, started: i64) -> (Option<String>, Option<i64>) {
    match args.created_time.as_deref() {
        Some("none") => (None, None),
        Some(text) => (Some(text.to_string()), None),
        None => (Some(format_rfc3339(started)), Some(started)),
    }
}

/// 单线程写出四件套：manifest / summary / matches / replays。
fn write_outputs(
    args: &RunArgs,
    plan: &SeedPlan,
    manifest: &Manifest,
    report: &ScoringReport,
    results: &[MatchResult],
    replays: &[(u32, String)],
) -> Result<(), CliError> {
    let out_dir = args.out.as_path();
    fs::create_dir_all(out_dir).map_err(|e| CliError::create_dir(out_dir, e))?;

    // manifest 与 summary：对象小，直接 pretty 便于人读。
    let manifest_json = serde_json::to_string_pretty(manifest)? + "\n";
    write_text(&out_dir.join("manifest.json"), &manifest_json)?;

    let summary = SummaryFile {
        report,
        engine_version: manifest.engine_version,
        versions: manifest.versions.clone(),
    };
    let summary_json = serde_json::to_string_pretty(&summary)? + "\n";
    write_text(&out_dir.join("summary.json"), &summary_json)?;

    // matches.jsonl：一行一局，按局索引升序；`ai_seeds` 从种子计划按同一索引取出。
    let mut matches_text = String::new();
    for (index, result) in results.iter().enumerate() {
        let ai_seeds: &[u64] = plan
            .ai_seeds
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let line = MatchLine { result, ai_seeds };
        matches_text.push_str(&serde_json::to_string(&line)?);
        matches_text.push('\n');
    }
    write_text(&out_dir.join("matches.jsonl"), &matches_text)?;

    // 回放：文件名按局索引补零编号（match_00000.jsonl），与 matches.jsonl 行序对应。
    // `--replay-sample` 已决定哪些局带回放文本；这里为空只可能是用户选了 none。
    if replays.is_empty() {
        return Ok(());
    }
    let replays_dir = out_dir.join("replays");
    fs::create_dir_all(&replays_dir).map_err(|e| CliError::create_dir(&replays_dir, e))?;
    for (index, text) in replays {
        let path = replays_dir.join(format!("match_{index:05}.jsonl"));
        write_text(&path, text)?;
    }
    Ok(())
}

/// 写一个文本文件（路径进错误信息，便于定位是哪个文件写失败）。
fn write_text(path: &Path, contents: &str) -> Result<(), CliError> {
    fs::write(path, contents).map_err(|e| CliError::write_file(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::ReplaySample;
    use clap::Parser;

    fn args_with(ai: &[&str], teams: u8) -> RunArgs {
        let mut argv: Vec<String> = vec![
            "qfr".to_string(),
            "run".to_string(),
            "--teams".to_string(),
            teams.to_string(),
        ];
        for spec in ai {
            argv.push("--ai".to_string());
            argv.push((*spec).to_string());
        }
        let cli = crate::args::Cli::try_parse_from(argv).expect("解析参数");
        match cli.command {
            crate::args::Command::Run(args) => *args,
        }
    }

    #[test]
    fn ai_specs_resolve_to_per_team_names() {
        let registry = ai::register_all();
        let names = resolve_ai_names(&[], 2, &registry).expect("默认");
        assert_eq!(names, vec![ai::DEFAULT_AI, ai::DEFAULT_AI]);

        let names =
            resolve_ai_names(&["1=defender".into(), "0=greedy_flag".into()], 2, &registry)
                .expect("显式下标");
        assert_eq!(names, vec![ai::AI_GREEDY_FLAG, ai::AI_DEFENDER]);

        // 省略下标：按最小空闲队号分配。
        let names = resolve_ai_names(&["defender".into()], 3, &registry).expect("省略下标");
        assert_eq!(names, vec![ai::AI_DEFENDER, ai::DEFAULT_AI, ai::DEFAULT_AI]);

        // 未知 AI / 越界 / 重复 / 格式错误都要明确报错，而不是静默忽略。
        let err = resolve_ai_names(&["0=no_such_ai".into()], 2, &registry).unwrap_err();
        assert!(matches!(err, CliError::AiNotRegistered { .. }), "{err}");
        let err = resolve_ai_names(&["2=random".into()], 2, &registry).unwrap_err();
        assert!(matches!(err, CliError::AiIndexOutOfRange { index: 2, .. }), "{err}");
        let err = resolve_ai_names(&["0=random".into(), "0=random".into()], 2, &registry).unwrap_err();
        assert!(matches!(err, CliError::DuplicateAiSlot(0)), "{err}");
        let err = resolve_ai_names(&["x=random".into()], 2, &registry).unwrap_err();
        assert!(matches!(err, CliError::BadAiSpec(_)), "{err}");
        let err = resolve_ai_names(&["random".into(), "random".into(), "random".into()], 2, &registry)
            .unwrap_err();
        assert!(matches!(err, CliError::BadAiSpec(_)), "{err}");
    }

    #[test]
    fn args_validation_rejects_bad_combinations() {
        let mut warnings = Vec::new();
        let mut args = args_with(&[], 2);
        args.matches = 0;
        assert!(matches!(
            validate_args(&args, &mut warnings).unwrap_err(),
            CliError::BadMatchCount(0)
        ));

        let mut args = args_with(&[], 2);
        args.map_gen_version = 9;
        assert!(matches!(
            validate_args(&args, &mut warnings).unwrap_err(),
            CliError::UnsupportedMapGenVersion { got: 9, .. }
        ));

        // --seed 在非 fixed 模式下被忽略，但必须留下提示。
        warnings.clear();
        let mut args = args_with(&[], 2);
        args.seed = Some(5);
        validate_args(&args, &mut warnings).expect("忽略 --seed 不是错误");
        assert!(warnings.iter().any(|w| w.contains("--seed")), "{warnings:?}");

        // units_per_team != 3 时警告。
        warnings.clear();
        let mut args = args_with(&[], 2);
        args.units_per_team = 5;
        validate_args(&args, &mut warnings).expect("非默认单位数只是警告");
        assert!(warnings.iter().any(|w| w.contains("units-per-team")), "{warnings:?}");
    }

    #[test]
    fn scoring_config_overrides_and_rejects_bad_weights() {
        let mut args = args_with(&[], 3);
        args.weight_win_rate = 1.0;
        args.weight_score_share = 0.0;
        args.weight_kill_ratio = 0.0;
        args.baseline_rating = 800.0;
        let config = build_scoring_config(&args).expect("合法权重");
        assert_eq!(config.teams, 3);
        assert_eq!(config.baseline_rating, 800.0);
        assert_eq!(config.weight_win_rate, 1.0);

        args.weight_win_rate = 0.0;
        assert!(matches!(
            build_scoring_config(&args).unwrap_err(),
            CliError::BadWeights(_)
        ));
        args.weight_win_rate = -1.0;
        assert!(matches!(
            build_scoring_config(&args).unwrap_err(),
            CliError::BadWeights(_)
        ));
    }

    #[test]
    fn created_time_variants() {
        let mut args = args_with(&[], 2);
        args.created_time = Some("none".into());
        assert_eq!(resolve_created_time(&args, 0), (None, None));
        args.created_time = Some("2024-05-01T00:00:00Z".into());
        assert_eq!(
            resolve_created_time(&args, 0),
            (Some("2024-05-01T00:00:00Z".into()), None)
        );
        args.created_time = None;
        let (text, unix) = resolve_created_time(&args, 1_714_521_600);
        assert_eq!(text.as_deref(), Some("2024-05-01T00:00:00Z"));
        assert_eq!(unix, Some(1_714_521_600));
    }

    #[test]
    fn replay_sample_gating_matches_flag() {
        assert!(ReplaySample::All.saves(10));
        assert!(!ReplaySample::None.saves(0));
        assert!(ReplaySample::First(3).saves(1));
    }
}
