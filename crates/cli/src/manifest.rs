//! 输出四件套中的「声明类」结构：`manifest.json`、`summary.json` 与 `matches.jsonl` 的一行。
//!
//! # 为什么 manifest 要带嵌套的 `versions`
//!
//! `tools/check_versions.py`（Lead 的验收脚本）读的是**顶层**三个字段；
//! 同时再嵌一份 `versions` 便于 Web UI 一次取出「三件套」而不用在顶层里挑。
//! 两份值由同一个构造器写死，不可能不一致（脚本也会再校验一次）。
//!
//! # 为什么 summary.json 不是裸的 `ScoringReport`
//!
//! 冻结的 `ScoringReport` 只有 `rules_version` 与 `map_gen_version`，
//! 而验收脚本要求 summary 与 manifest 的三件套完全相等（含 `engine_version`）。
//! 与其改冻结的评分结构，不如在 CLI 落盘时**加一层薄包装**：
//! `engine_version` 属于「本次运行的声明」，本来就该由 CLI 补上。
//! `#[serde(flatten)]` 保证 `ScoringReport` 的字段仍在顶层，
//! 现有读 summary.json 的脚本（按顶层键取 by_ai_name 等）不受影响。
//!
//! # 为什么 matches.jsonl 一行要带 `ai_seeds`
//!
//! `docs/rules.md` §10 要求「每局的完整种子（地图种子 + ai_seeds）必须写进 matches.jsonl」，
//! 但冻结的 `MatchResult` 只有地图种子 `seed`。做法是把 `ai_seeds` 追加到
//! `MatchResult` 序列化结果的后面（超集，不新增 `type` 字段）：
//! 用 `MatchResult` 反序列化的下游（serde 默认忽略未知字段）完全不受影响，
//! 而单局复现需要的 AI 种子就从「必须回看 manifest + 复算」变成「当场可读」。

use serde::Serialize;

use protocol::versions::{ENGINE_VERSION, RULES_VERSION};
use protocol::MatchResult;
use scoring::{ScoringConfig, ScoringReport};

use crate::seed::SeedPlanInfo;

/// 版本号三件套（嵌套写法）。
#[derive(Clone, Debug, Serialize)]
pub struct Versions {
    /// 模拟核心实现版本。
    pub engine_version: u32,
    /// 规则版本。
    pub rules_version: u32,
    /// 地图生成算法版本。
    pub map_gen_version: u32,
}

impl Versions {
    /// 用本次运行实际使用的版本构造（地图生成版本来自命令行，不是编译期常量，
    /// 这样才能在「程序比地图生成器新」时报出真实的不匹配）。
    pub fn new(map_gen_version: u32) -> Self {
        Self {
            engine_version: ENGINE_VERSION,
            rules_version: RULES_VERSION,
            map_gen_version,
        }
    }
}

/// `manifest.json`：本次批量运行的完整声明。
#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
    pub engine_version: u32,
    pub rules_version: u32,
    pub map_gen_version: u32,
    /// 三件套的嵌套副本（见模块文档）。
    pub versions: Versions,
    /// 创建时间（RFC3339 UTC）；`--created-time none` 时整个字段省略，
    /// 这样同一批产物可以逐字节比对。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_time: Option<String>,
    /// 创建时间的 Unix 秒（便于脚本排序/比对；给固定时间字符串时不写）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_time_unix: Option<i64>,
    /// 种子模式（kebab-case，与 CLI 取值一致）。
    pub seed_mode: String,
    /// 种子生成器基准种子。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_seed: Option<u64>,
    /// `fixed` 模式的固定种子。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed_seed: Option<u64>,
    /// 解析后的种子信息（模式、样本、总数）。
    pub seed_plan: SeedPlanInfo,
    /// 完整运行配置。
    pub config: ManifestConfig,
    /// 非致命提示（例如「--seed 在该模式下被忽略」）——写进 manifest 才不会丢失。
    pub warnings: Vec<String>,
}

/// `manifest.config`：复现一次运行所需的全部参数。
#[derive(Clone, Debug, Serialize)]
pub struct ManifestConfig {
    pub matches: u32,
    pub max_ticks: u32,
    pub teams: u8,
    /// 每队 AI 名字（下标即队伍 ID）。
    pub ai_names: Vec<String>,
    pub map_width: u16,
    pub map_height: u16,
    pub units_per_team: u8,
    /// `--jobs` 原值（0 = 自动）。
    pub jobs: usize,
    /// `--replay-sample` 的原始写法（all / none / N）。
    pub replay_sample: String,
    /// 评分权重与基准（含 `teams`，明确标注分数所属体系）。
    pub scoring: ScoringConfig,
}

/// `summary.json`：`ScoringReport` + 本次运行的版本声明。
#[derive(Debug, Serialize)]
pub struct SummaryFile<'a> {
    /// 评分汇总（字段被 flatten 到顶层，保持 summary.json 的既有可读结构）。
    #[serde(flatten)]
    pub report: &'a ScoringReport,
    /// 引擎版本（`ScoringReport` 里没有，由 CLI 声明）。
    pub engine_version: u32,
    /// 三件套的嵌套副本。
    pub versions: Versions,
}

/// `matches.jsonl` 的一行：`MatchResult` + 本局各队 `ai_seeds`。
#[derive(Debug, Serialize)]
pub struct MatchLine<'a> {
    #[serde(flatten)]
    pub result: &'a MatchResult,
    /// 本局各队 AI 种子（`[队]`，下标即队伍 ID）。
    pub ai_seeds: &'a [u64],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> ScoringReport {
        scoring::aggregate(&[], 2, RULES_VERSION, 1, &ScoringConfig::default())
    }

    #[test]
    fn summary_file_carries_all_three_versions_at_top_level() {
        let report = sample_report();
        let summary = SummaryFile {
            report: &report,
            engine_version: ENGINE_VERSION,
            versions: Versions::new(1),
        };
        let value = serde_json::to_value(&summary).expect("序列化");
        // 验收脚本要求这三个键在 summary.json 顶层存在且为正整数。
        assert_eq!(value["engine_version"], ENGINE_VERSION);
        assert_eq!(value["rules_version"], RULES_VERSION);
        assert_eq!(value["map_gen_version"], 1);
        // ScoringReport 的字段仍在顶层（flatten 生效）。
        assert_eq!(value["total_matches"], 0);
        assert!(value["by_ai_name"].is_array());
        assert_eq!(value["versions"]["engine_version"], ENGINE_VERSION);
    }

    #[test]
    fn match_line_is_a_superset_of_match_result() {
        let result = MatchResult {
            match_index: 0,
            seed: 42,
            map_gen_version: 1,
            ticks: 300,
            scores: vec![1, 0],
            kills: vec![1, 0],
            deaths: vec![0, 1],
            winner: Some(0),
            ai_names: vec!["random".into(), "random".into()],
        };
        let line = MatchLine {
            result: &result,
            ai_seeds: &[7, 8],
        };
        let value = serde_json::to_value(&line).expect("序列化");
        assert_eq!(value["seed"], 42);
        assert_eq!(value["ai_seeds"], serde_json::json!([7, 8]));
        assert!(value.get("type").is_none(), "matches 行不允许有 type 字段");
        // 下游仍能用 MatchResult 反序列化（忽略未知字段）。
        let back: MatchResult = serde_json::from_value(value).expect("反序列化");
        assert_eq!(back, result);
    }

    #[test]
    fn versions_block_matches_top_level_fields() {
        let v = Versions::new(3);
        let manifest = Manifest {
            engine_version: ENGINE_VERSION,
            rules_version: RULES_VERSION,
            map_gen_version: 3,
            versions: v,
            created_time: None,
            created_time_unix: None,
            seed_mode: "fixed".into(),
            base_seed: None,
            fixed_seed: Some(1),
            seed_plan: crate::seed::SeedPlan::generate(
                crate::seed::SeedMode::Fixed,
                1,
                2,
                None,
                Some(1),
                0,
            )
            .expect("种子计划")
            .info(5),
            config: ManifestConfig {
                matches: 1,
                max_ticks: 300,
                teams: 2,
                ai_names: vec!["random".into(), "random".into()],
                map_width: 25,
                map_height: 25,
                units_per_team: 3,
                jobs: 0,
                replay_sample: "all".into(),
                scoring: ScoringConfig::default(),
            },
            warnings: vec![],
        };
        let text = serde_json::to_string(&manifest).expect("序列化");
        assert!(!text.contains("created_time"), "省略时必须完全不出现：{text}");
        let value: serde_json::Value = serde_json::from_str(&text).expect("合法 JSON");
        for key in ["engine_version", "rules_version", "map_gen_version"] {
            assert_eq!(value[key], value["versions"][key], "{key}");
        }
    }
}
