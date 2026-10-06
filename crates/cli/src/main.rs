//! `qfr` 命令入口：clap 装配 + 人类可读的汇总输出。
//!
//! 这里刻意保持「薄」：
//!
//! * 参数解析交给 [`cli::args`]；
//! * 全部业务逻辑在 [`cli::runner::run_batch`]（可单测）；
//! * `main` 只负责把库层错误用 `anyhow` 往上抛，并把 `ScoringReport` 打成一张表。
//!
//! 编码规范要求**生产代码不允许 `unwrap()`**：`main` 的错误路径全部用 `?` 传播。
//! 唯一会「退出进程」的地方是 clap 自己的参数错误处理（它打印用法并返回非零退出码，
//! 这正是我们想要的 CLI 行为）。

use anyhow::Result;
use clap::Parser;

use cli::args::{Cli, Command};
use cli::runner::{run_batch, BatchOutcome};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => {
            let outcome = run_batch(&args)?;
            if !args.quiet {
                print_summary(&outcome);
            }
        }
    }
    Ok(())
}

/// 打印批次汇总：产出位置、局数、种子模式，以及按 AI / 按槽位的评分表。
fn print_summary(outcome: &BatchOutcome) {
    let report = &outcome.report;
    println!("输出目录：{}", outcome.out_dir.display());
    println!(
        "总局数：{}（回放 {} 份）  队伍数：{}  种子模式：{}",
        report.total_matches, outcome.replays_written, report.teams, outcome.manifest.seed_mode
    );
    println!(
        "版本：engine {} / rules {} / map_gen {}",
        outcome.manifest.engine_version,
        outcome.manifest.rules_version,
        outcome.manifest.map_gen_version
    );
    println!(
        "注意：2 队与 3 队的分数属于不同体系，不可直接比较（本批 teams={}）",
        report.teams
    );

    println!();
    println!(
        "{:<16} {:>7} {:>4} {:>4} {:>4} {:>9} {:>8}",
        "AI", "局数", "胜", "平", "负", "胜率", "评分"
    );
    for row in &report.by_ai_name {
        println!(
            "{:<16} {:>7} {:>4} {:>4} {:>4} {:>9.3} {:>8.1}",
            row.ai_name, row.matches, row.wins, row.draws, row.losses, row.win_rate, row.rating
        );
    }

    if report.by_slot.len() > 1 {
        println!();
        println!(
            "{:<6} {:<16} {:>7} {:>4} {:>9} {:>8}",
            "槽位", "AI", "局数", "胜", "胜率", "评分"
        );
        for row in &report.by_slot {
            println!(
                "{:<6} {:<16} {:>7} {:>4} {:>9.3} {:>8.1}",
                row.slot, row.ai_name, row.matches, row.wins, row.win_rate, row.rating
            );
        }
    }

    for warning in &outcome.warnings {
        eprintln!("提示：{warning}");
    }
}
