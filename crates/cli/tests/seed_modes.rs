//! docs/rules.md §14 测试 20：三种种子模式行为符合第 10 节。
//!
//! 这些测试走完整的 `run_batch` 流程，断言的是**产物里的种子**（manifest.json 的
//! `seed_plan` 与每局 `MatchResult.seed`），而不是内部函数——因为「可复现」的验收
//! 依据是落盘的产物。

mod common;

use common::{read, run_with, tempdir, try_run_with};

/// `per-match` + `--base-seed`：两次运行逐局种子完全一致（§10 要点 2）。
#[test]
fn per_match_with_base_seed_is_reproducible() {
    let extra = [
        "--matches", "4",
        "--teams", "2",
        "--seed-mode", "per-match",
        "--base-seed", "12345",
        "--replay-sample", "none",
        "--created-time", "none",
    ];
    let first = tempdir();
    let second = tempdir();
    let a = run_with(first.path(), &extra);
    let b = run_with(second.path(), &extra);

    assert_eq!(a.manifest.seed_mode, "per-match");
    assert_eq!(a.manifest.seed_plan.base_seed, Some(12345));
    assert_eq!(a.manifest.seed_plan.total_matches, 4);
    assert_eq!(
        a.manifest.seed_plan.map_seed_sample,
        b.manifest.seed_plan.map_seed_sample,
        "同 base-seed 的地图种子序列必须一致"
    );
    assert_eq!(
        a.manifest.seed_plan.ai_seed_sample,
        b.manifest.seed_plan.ai_seed_sample,
        "同 base-seed 的 AI 种子序列必须一致"
    );
    assert_eq!(
        read(&first.path().join("matches.jsonl")),
        read(&second.path().join("matches.jsonl")),
        "per-match + base-seed 的 matches.jsonl 必须逐行一致"
    );
    // per-match 的 AI 盐就是地图种子（§10 要点 3）。
    let sample = &a.manifest.seed_plan;
    assert_eq!(sample.ai_seed_sample[0].len(), 2);
    for (map_seed, ai_seeds) in sample.map_seed_sample.iter().zip(&sample.ai_seed_sample) {
        assert_ne!(ai_seeds[0], *map_seed, "AI 种子必须与地图种子区分开");
        assert_ne!(ai_seeds[0], ai_seeds[1], "同局不同队必须拿到不同 AI 种子");
    }
}

/// `per-match` 不给 `--base-seed`：用熵播种，两次运行（几乎必然）不同。
#[test]
fn per_match_without_base_seed_uses_entropy() {
    let extra = [
        "--matches", "4",
        "--teams", "2",
        "--seed-mode", "per-match",
        "--replay-sample", "none",
    ];
    let first = tempdir();
    let second = tempdir();
    let a = run_with(first.path(), &extra);
    let b = run_with(second.path(), &extra);

    assert_eq!(a.manifest.seed_plan.base_seed, None);
    assert_ne!(
        a.manifest.seed_plan.map_seed_sample, b.manifest.seed_plan.map_seed_sample,
        "无 base-seed 时两次运行不应该撞出同一串 64 位种子（概率 2^-256）"
    );
}

/// `fixed-random`：地图固定（所有局同图），但 AI 盐按局索引推导（§10 的已知陷阱）。
#[test]
fn fixed_random_shares_one_map_but_varies_ai_seed() {
    let extra = [
        "--matches", "4",
        "--teams", "2",
        "--seed-mode", "fixed-random",
        "--base-seed", "20240501",
        "--replay-sample", "none",
        "--created-time", "none",
    ];
    let first = tempdir();
    let second = tempdir();
    let a = run_with(first.path(), &extra);
    let b = run_with(second.path(), &extra);
    let plan = &a.manifest.seed_plan;

    assert!(!plan.map_seed_sample.is_empty());
    let map_seed = plan.map_seed_sample[0];
    assert!(
        plan.map_seed_sample.iter().all(|m| *m == map_seed),
        "fixed-random 必须所有局共用同一个地图种子：{:?}",
        plan.map_seed_sample
    );
    assert!(
        a.results.iter().all(|r| r.seed == map_seed),
        "每局 MatchResult.seed 都应是那个固定随机种子"
    );
    assert_ne!(
        plan.ai_seed_sample[0], plan.ai_seed_sample[1],
        "地图固定时 AI 盐必须按局索引变化，否则所有局结果一模一样"
    );
    assert_eq!(plan.map_seed_sample, b.manifest.seed_plan.map_seed_sample);
    assert_eq!(plan.ai_seed_sample, b.manifest.seed_plan.ai_seed_sample);
}

/// `fixed`：用户给的种子原样用于每一局。
#[test]
fn fixed_seed_is_used_verbatim() {
    let extra = [
        "--matches", "3",
        "--teams", "2",
        "--seed-mode", "fixed",
        "--seed", "777",
        "--replay-sample", "none",
        "--created-time", "none",
    ];
    let dir = tempdir();
    let out = run_with(dir.path(), &extra);

    assert_eq!(out.manifest.seed_mode, "fixed");
    assert_eq!(out.manifest.fixed_seed, Some(777));
    assert_eq!(out.manifest.seed_plan.fixed_seed, Some(777));
    assert!(out.manifest.seed_plan.map_seed_sample.iter().all(|m| *m == 777));
    assert!(out.results.iter().all(|r| r.seed == 777));
    // fixed 模式下 AI 盐仍然按局索引推导，避免「同图同 AI 全平局」。
    assert_ne!(
        out.manifest.seed_plan.ai_seed_sample[0],
        out.manifest.seed_plan.ai_seed_sample[1]
    );
}

/// `fixed` 不给 `--seed`：必须明确报错，而不是悄悄用熵。
#[test]
fn fixed_without_seed_is_rejected() {
    let dir = tempdir();
    let err = try_run_with(
        dir.path(),
        &["--matches", "2", "--seed-mode", "fixed", "--replay-sample", "none"],
    )
    .expect_err("fixed 模式缺 --seed 必须失败");
    assert!(matches!(err, cli::CliError::MissingFixedSeed), "{err}");
}

/// 非 fixed 模式给了 `--seed` 只是提示（§11：其他模式忽略并给出提示）。
#[test]
fn seed_is_ignored_outside_fixed_mode_with_a_warning() {
    let dir = tempdir();
    let out = run_with(
        dir.path(),
        &[
            "--matches", "2",
            "--seed-mode", "per-match",
            "--seed", "999",
            "--base-seed", "5",
            "--replay-sample", "none",
        ],
    );
    assert!(
        out.warnings.iter().any(|w| w.contains("--seed")),
        "应提示 --seed 被忽略：{:?}",
        out.warnings
    );
    assert!(out.results.iter().all(|r| r.seed != 999));
}
