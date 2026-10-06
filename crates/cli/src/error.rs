//! CLI 层错误：库层用 `thiserror`（本文件），`main` 只负责用 `anyhow` 把错误报告给用户。
//!
//! 为什么库层不直接用 `anyhow`：`run_batch` 是可以在测试里被调用的函数，
//! 测试需要**按变体**断言失败原因（例如「3 队的 D 局必须是 BadTeamCount」），
//! 而 `anyhow::Error` 只能比较字符串。所以库层保留结构化错误，
//! 只在 `main` 的最后一层转成 `anyhow`。
//!
//! 所有用户可见的错误信息都是中文，并且**尽量把「怎么改」写进去**：
//! CLI 的报错是用户唯一的线索，只说「参数非法」等于没说。

use std::path::PathBuf;

use thiserror::Error;

/// CLI 可能出现的全部错误。
#[derive(Debug, Error)]
pub enum CliError {
    #[error("队伍数必须是 2 或 3，得到 {0}")]
    BadTeamCount(u8),

    #[error("局数必须至少为 1，得到 {0}")]
    BadMatchCount(u32),

    #[error("地图尺寸必须大于 0，得到 {width}x{height}")]
    BadMapSize { width: u16, height: u16 },

    #[error("不支持的地图生成版本 {got}（当前实现只支持 {supported}）；请升级程序或改用 --map-gen-version {supported}")]
    UnsupportedMapGenVersion { got: u32, supported: u32 },

    #[error("--ai 的队伍下标 {index} 超出范围（本局 {teams} 队，合法下标 0..{teams}）")]
    AiIndexOutOfRange { index: u8, teams: u8 },

    #[error("--ai 重复指定了第 {0} 队的 AI；同一队只能指定一次（后者不会静默覆盖前者）")]
    DuplicateAiSlot(u8),

    #[error("无法解析 --ai 参数 {0:?}，期望格式为 `N=name`（例如 `--ai 0=greedy_flag`）或直接写 AI 名字 `name`")]
    BadAiSpec(String),

    #[error("未知的 AI 名字 {name:?}；已注册的 AI：{available}")]
    AiNotRegistered { name: String, available: String },

    #[error("无法解析 --replay-sample 参数 {0:?}，期望 `all`、`none` 或非负整数 N（保存前 N 局）")]
    BadReplaySample(String),

    #[error("--seed-mode fixed 必须显式给出 --seed N；其他模式请去掉 --seed（它会被忽略）")]
    MissingFixedSeed,

    #[error("--max-ticks 必须至少为 1，得到 {0}")]
    BadMaxTicks(u32),

    #[error("--units-per-team 必须至少为 1，得到 {0}")]
    BadUnitsPerTeam(u8),

    #[error("评分参数非法：{0}")]
    BadWeights(String),

    #[error("第 {index} 局要求保存回放，但模拟核心没有返回回放数据（内部不一致）")]
    ReplayMissing { index: u32 },

    #[error("创建线程池失败（--jobs {jobs}）：{message}")]
    ThreadPool { jobs: usize, message: String },

    #[error("创建目录 {path} 失败：{source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("写入文件 {path} 失败：{source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("序列化输出失败：{0}")]
    Json(#[from] serde_json::Error),

    #[error("模拟核心报错：{0}")]
    Sim(#[from] sim::SimError),

    #[error("评分模块报错：{0}")]
    Scoring(#[from] scoring::ScoringError),
}

impl CliError {
    /// 写文件失败时带上路径——裸 `io::Error` 的 "No such file or directory"
    /// 不说是哪个文件，而一次批量运行会写几百个文件。
    pub(crate) fn write_file(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        CliError::WriteFile {
            path: path.into(),
            source,
        }
    }

    /// 创建目录失败时同理。
    pub(crate) fn create_dir(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        CliError::CreateDir {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_are_actionable_chinese() {
        let e = CliError::AiNotRegistered {
            name: "nope".into(),
            available: "defender, greedy_flag, random".into(),
        };
        let text = e.to_string();
        assert!(text.contains("nope"), "{text}");
        assert!(text.contains("defender"), "报错要列出可用 AI：{text}");

        let e = CliError::UnsupportedMapGenVersion {
            got: 2,
            supported: 1,
        };
        assert!(e.to_string().contains("--map-gen-version 1"));
    }
}
