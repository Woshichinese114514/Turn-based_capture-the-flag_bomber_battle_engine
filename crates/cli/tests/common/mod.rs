//! cli 集成测试共享工具。
//!
//! 集成测试直接调用 `cli::run_batch`（而不是起进程），这样能拿到 `BatchOutcome`
//! 做结构化断言；参数仍然走真实的 clap 解析，保证「命令行怎么写」和「测试怎么跑」一致。

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use clap::Parser;

/// 仓库根目录（`crates/cli` 往上两级）。
pub fn repo_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().unwrap_or(root)
}

/// 组装 `qfr run` 的完整 argv：`-o <out>` + 额外参数。
pub fn argv_for(out: &Path, extra: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "qfr".to_string(),
        "run".to_string(),
        "-o".to_string(),
        out.display().to_string(),
    ];
    argv.extend(extra.iter().map(|s| (*s).to_string()));
    argv
}

/// 解析参数为 `RunArgs`（解析失败即测试失败）。
pub fn parse_args(out: &Path, extra: &[&str]) -> cli::args::RunArgs {
    try_parse_args(out, extra).expect("命令行参数必须可解析")
}

/// 解析参数为 `RunArgs`，返回 clap 的错误。
pub fn try_parse_args(out: &Path, extra: &[&str]) -> Result<cli::args::RunArgs, clap::Error> {
    let cli = cli::Cli::try_parse_from(argv_for(out, extra))?;
    match cli.command {
        cli::args::Command::Run(args) => Ok(*args),
    }
}

/// 跑一批对局，失败即测试失败。
pub fn run_with(out: &Path, extra: &[&str]) -> cli::BatchOutcome {
    try_run_with(out, extra).expect("批量运行必须成功")
}

/// 跑一批对局，把错误交还给调用者。
pub fn try_run_with(out: &Path, extra: &[&str]) -> Result<cli::BatchOutcome, cli::CliError> {
    cli::run_batch(&parse_args(out, extra))
}

/// 读文本文件（不存在即测试失败）。
pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("读取 {} 失败：{e}", path.display()))
}

/// 临时输出目录。
pub fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("qfr-cli-test-")
        .tempdir()
        .expect("创建临时目录")
}
