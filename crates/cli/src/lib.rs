//! `cli` crate：批量对局命令行入口的**可测试库层**。
//!
//! # 为什么把逻辑放在 lib 而不是全塞进 `main.rs`
//!
//! `main` 只能在「进程级集成测试」里被验证：要起子进程、看退出码、读文件，
//! 一个断言要几百毫秒。把参数解析（[`args`]）、种子计划（[`seed`]）、
//! 编排与写盘（[`runner`]）拆成库函数后，绝大多数契约（种子模式、可复现性、
//! 四件套结构）都能用普通单测直接断言，`main.rs` 只剩「clap 装配 + 打印表格」。
//!
//! # 模块分工
//!
//! * [`args`]：clap 参数定义与 `--replay-sample` 解析（只做语法校验）；
//! * [`seed`]：把 `--seed-mode`/`--seed`/`--base-seed` 变成整批种子（见 `docs/rules.md` §10）；
//! * [`manifest`]：`manifest.json` / `summary.json` / `matches.jsonl` 的结构定义；
//! * [`runner`]：校验 → 种子计划 → rayon 并行跑局 → 单线程统一写盘（四件套）；
//! * [`timefmt`]：不依赖 chrono 的 UTC 时间格式化与运行期熵源；
//! * [`textwidth`]：终端表格按「显示宽度」（中文占 2 列）对齐，而不是按字符数；
//! * [`error`]：库层结构化错误（`thiserror`），`main` 再转成 `anyhow`。

pub mod args;
pub mod error;
pub mod manifest;
pub mod runner;
pub mod seed;
pub mod textwidth;
pub mod timefmt;

pub use args::{Cli, Command, ReplaySample, RunArgs};
pub use error::CliError;
pub use manifest::{Manifest, ManifestConfig, MatchLine, SummaryFile, Versions};
pub use runner::{run_batch, BatchOutcome};
pub use seed::{SeedMode, SeedPlan, SeedPlanInfo};
pub use textwidth::{display_width, pad_left, pad_right, render_table, Align};
