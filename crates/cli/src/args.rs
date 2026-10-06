//! 命令行参数定义（clap derive）与 `--replay-sample` 的解析。
//!
//! 这里只做「语法层」的校验（类型、取值范围、枚举名），语义校验（例如队伍数与
//! `--ai` 下标是否匹配）放在 `runner`，因为它需要访问 AI 注册表。
//!
//! 参数表与 `docs/rules.md` §11 一一对应，默认值也照抄文档——文档是用户可见契约，
//! 代码与文档不一致时以文档为准，改代码而不是改文档。

use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand};

use crate::error::CliError;
use crate::seed::SeedMode;

/// 顶层命令：`qfr <子命令>`。
#[derive(Debug, Parser)]
#[command(
    name = "qfr",
    version,
    about = "抢旗人（Flag Bomb Arena）：批量对局、评分与回放导出",
    long_about = "抢旗人（Flag Bomb Arena）：批量对局、评分与回放导出。\n\
                  典型用法见 README 的「快速开始」。",
    propagate_version = true
)]
pub struct Cli {
    /// 子命令（当前只有 `run`）。
    #[command(subcommand)]
    pub command: Command,
}

/// 子命令集合。
#[derive(Debug, Subcommand)]
pub enum Command {
    /// 批量运行对局，产出 manifest/summary/matches/replays 四件套。
    Run(Box<RunArgs>),
}

/// `qfr run` 的全部参数。
///
/// 用 `Box<RunArgs>` 承载是为了避免 clap 巨大枚举变体的栈体积警告，
/// 同时让 `Command` 保持小尺寸。
#[derive(Debug, Args)]
#[command(after_help = "示例：\n  qfr run --matches 100 --teams 3 --ai 0=greedy_flag --ai 1=defender -o out/3p\n  qfr run --matches 500 --seed-mode fixed-random --base-seed 20240501 -o out/fr500")]
pub struct RunArgs {
    /// 批量对局数（≥ 1）。
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub matches: u32,

    /// 每局最大全局回合数。
    #[arg(long = "max-ticks", default_value_t = 300, value_name = "N")]
    pub max_ticks: u32,

    /// 队伍数（只允许 2 或 3）。
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(2..=3), value_name = "N")]
    pub teams: u8,

    /// 第 N 队使用哪个 AI，可重复（例如 `--ai 0=greedy_flag --ai 2=defender`）。
    ///
    /// 未指定的队伍使用默认 AI（random）；也允许省略下标（按最小空闲队号分配）。
    #[arg(long = "ai", value_name = "N=name")]
    pub ai: Vec<String>,

    /// 输出目录（不存在则创建）。
    #[arg(short = 'o', long = "out", default_value = "out", value_name = "DIR")]
    pub out: PathBuf,

    /// 回放保存策略：`all`（全部）、`none`（不存）、或整数 N（只存前 N 局）。
    #[arg(long = "replay-sample", default_value = "all", value_name = "all|none|N")]
    pub replay_sample: ReplaySample,

    /// 地图生成版本（不支持时明确报错，而不是静默按当前版本生成）。
    #[arg(long = "map-gen-version", default_value_t = 1, value_name = "N")]
    pub map_gen_version: u32,

    /// 种子模式：per-match（每局随机图）/ fixed-random（随机一张图复用）/ fixed（指定种子）。
    #[arg(long = "seed-mode", value_enum, default_value_t = SeedMode::PerMatch, value_name = "per-match|fixed-random|fixed")]
    pub seed_mode: SeedMode,

    /// `fixed` 模式的地图种子（必填）；其他模式下忽略并提示。
    #[arg(long = "seed", value_name = "N")]
    pub seed: Option<u64>,

    /// 种子生成器的基准种子：给了它，「随机」本身也可复现。
    #[arg(long = "base-seed", value_name = "N")]
    pub base_seed: Option<u64>,

    /// 并行线程数：0 = 由 rayon 决定（通常 = 逻辑核数）。
    #[arg(long = "jobs", default_value_t = 0, value_name = "N")]
    pub jobs: usize,

    /// 地图宽度。
    #[arg(long = "map-width", default_value_t = 25, value_name = "N")]
    pub map_width: u16,

    /// 地图高度。
    #[arg(long = "map-height", default_value_t = 25, value_name = "N")]
    pub map_height: u16,

    /// 评分权重：胜率。
    #[arg(long = "weight-win-rate", default_value_t = 0.6, value_name = "W")]
    pub weight_win_rate: f64,

    /// 评分权重：得分占比。
    #[arg(long = "weight-score-share", default_value_t = 0.25, value_name = "W")]
    pub weight_score_share: f64,

    /// 评分权重：击杀比。
    #[arg(long = "weight-kill-ratio", default_value_t = 0.15, value_name = "W")]
    pub weight_kill_ratio: f64,

    /// 评分基准分（strength = 0.5 时的分数）。
    #[arg(long = "baseline-rating", default_value_t = 1000.0, value_name = "R")]
    pub baseline_rating: f64,

    /// 每 10 倍实力对应的分数（默认 500）。
    #[arg(long = "points-per-decade", default_value_t = 500.0, value_name = "P")]
    pub points_per_decade: f64,

    /// 每队单位数（预留参数；默认 3，非 3 时警告规则未针对该值调参）。
    #[arg(long = "units-per-team", default_value_t = 3, value_name = "N")]
    pub units_per_team: u8,

    /// 写入 manifest 的创建时间：`none` 表示完全省略（用于逐字节可复现的回归测试），
    /// 其他字符串按原样写入（例如 `2024-05-01T00:00:00Z`）。
    #[arg(long = "created-time", value_name = "none|TEXT")]
    pub created_time: Option<String>,

    /// 不打印人类可读的汇总表（只写文件）。
    #[arg(long = "quiet", default_value_t = false)]
    pub quiet: bool,
}

/// `--replay-sample` 的取值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplaySample {
    /// 保存全部回放。
    All,
    /// 一份都不存（只出统计）。
    None,
    /// 只保存前 N 局（0 等同于 none）。
    First(u32),
}

impl ReplaySample {
    /// 第 `index` 局是否要保存回放。
    pub fn saves(self, index: usize) -> bool {
        match self {
            ReplaySample::All => true,
            ReplaySample::None => false,
            ReplaySample::First(n) => (index as u64) < u64::from(n),
        }
    }

    /// 写进 manifest 的标准字符串（与用户输入形式一致）。
    pub fn as_str(self) -> String {
        match self {
            ReplaySample::All => "all".to_string(),
            ReplaySample::None => "none".to_string(),
            ReplaySample::First(n) => n.to_string(),
        }
    }
}

impl std::fmt::Display for ReplaySample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl FromStr for ReplaySample {
    type Err = CliError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        match trimmed {
            "all" => Ok(ReplaySample::All),
            "none" => Ok(ReplaySample::None),
            _ => trimmed
                .parse::<u32>()
                .map(ReplaySample::First)
                .map_err(|_| CliError::BadReplaySample(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_shape_is_consistent() {
        // `debug_assert` 会校验 clap 配置本身（重名参数、默认值类型等），
        // 这类错误只有运行 `--help` 才暴露，放在单测里最划算。
        Cli::command().debug_assert();
    }

    #[test]
    fn replay_sample_parses_all_forms() {
        assert_eq!("all".parse::<ReplaySample>().expect("all 可解析"), ReplaySample::All);
        assert_eq!("none".parse::<ReplaySample>().expect("none 可解析"), ReplaySample::None);
        assert_eq!("7".parse::<ReplaySample>().expect("N 可解析"), ReplaySample::First(7));
        assert!(!ReplaySample::First(0).saves(0));
        assert!(ReplaySample::All.saves(999));
        assert!(!ReplaySample::None.saves(0));
        assert!(ReplaySample::First(3).saves(2) && !ReplaySample::First(3).saves(3));
        let err = "half".parse::<ReplaySample>().unwrap_err();
        assert!(matches!(err, CliError::BadReplaySample(_)), "{err}");
        assert_eq!(ReplaySample::First(5).as_str(), "5");
    }

    #[test]
    fn defaults_follow_rules_doc() {
        let cli = Cli::try_parse_from(["qfr", "run"]).expect("默认参数必须可用");
        let Command::Run(args) = cli.command;
        assert_eq!(args.matches, 1);
        assert_eq!(args.max_ticks, 300);
        assert_eq!(args.teams, 2);
        assert_eq!(args.out, PathBuf::from("out"));
        assert_eq!(args.replay_sample, ReplaySample::All);
        assert_eq!(args.map_gen_version, 1);
        assert_eq!(args.seed_mode, SeedMode::PerMatch);
        assert_eq!(args.jobs, 0);
        assert_eq!((args.map_width, args.map_height), (25, 25));
        assert_eq!(args.weight_win_rate, 0.6);
        assert_eq!(args.weight_score_share, 0.25);
        assert_eq!(args.weight_kill_ratio, 0.15);
        assert_eq!(args.baseline_rating, 1000.0);
        assert_eq!(args.points_per_decade, 500.0);
        assert_eq!(args.units_per_team, 3);
        assert!(args.seed.is_none() && args.base_seed.is_none());
        assert!(args.ai.is_empty() && !args.quiet);
    }

    #[test]
    fn team_count_range_is_enforced_at_parse_time() {
        let err = Cli::try_parse_from(["qfr", "run", "--teams", "4"]).unwrap_err();
        assert!(err.to_string().contains("--teams") || err.to_string().contains("4"));
        assert!(Cli::try_parse_from(["qfr", "run", "--teams", "3"]).is_ok());
    }
}
