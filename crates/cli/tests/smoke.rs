//! docs/rules.md §14 测试 22：集成（100 局 random vs random 不 panic + 回放可校验）。

mod common;

use std::path::Path;
use std::process::Command;

use common::{read, repo_root, run_with, tempdir};

/// 100 局 random vs random：不 panic、结果结构完整、汇总覆盖全部局数。
#[test]
fn hundred_matches_random_vs_random_do_not_panic() {
    let dir = tempdir();
    let out = run_with(
        dir.path(),
        &[
            "--matches", "100",
            "--teams", "2",
            "--max-ticks", "60",
            "--replay-sample", "3",
            "--base-seed", "7",
            "--created-time", "none",
        ],
    );

    assert_eq!(out.results.len(), 100);
    assert_eq!(out.report.total_matches, 100);
    assert_eq!(out.report.teams, 2);
    assert_eq!(out.replays_written, 3, "--replay-sample 3 只写前 3 局");
    assert_eq!(out.warnings.len(), 0, "默认参数下不应有警告：{:?}", out.warnings);

    for (index, result) in out.results.iter().enumerate() {
        assert_eq!(result.match_index, index as u32);
        assert_eq!(result.scores.len(), 2);
        assert_eq!(result.kills.len(), 2);
        assert_eq!(result.deaths.len(), 2);
        assert_eq!(result.ai_names.len(), 2);
        assert_eq!(result.map_gen_version, out.manifest.map_gen_version);
        assert!(result.ticks > 0 && result.ticks <= 60, "ticks={} 超出 0..=60", result.ticks);
    }
    // 三个回放文件都在 replays/ 下，按局索引编号。
    for index in 0..3 {
        let path = dir.path().join(format!("replays/match_{index:05}.jsonl"));
        assert!(path.is_file(), "缺少回放 {}", path.display());
    }
}

/// 回放文件结构：init / frame* / end，且 init 的三件套与 manifest 完全一致。
#[test]
fn replay_files_are_well_formed() {
    let dir = tempdir();
    let out = run_with(
        dir.path(),
        &[
            "--matches", "2",
            "--teams", "2",
            "--max-ticks", "20",
            "--replay-sample", "all",
            "--base-seed", "7",
            "--created-time", "none",
        ],
    );
    assert_eq!(out.replays_written, 2);

    for index in 0..2 {
        let path = dir.path().join(format!("replays/match_{index:05}.jsonl"));
        let text = read(&path);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() >= 2, "回放至少要有 init 与 end：{}", path.display());

        let init: serde_json::Value = serde_json::from_str(lines[0]).expect("init 行是 JSON");
        assert_eq!(init["type"], "init");
        assert_eq!(init["engine_version"].as_u64(), Some(u64::from(out.manifest.engine_version)));
        assert_eq!(init["rules_version"].as_u64(), Some(u64::from(out.manifest.rules_version)));
        assert_eq!(init["map_gen_version"].as_u64(), Some(u64::from(out.manifest.map_gen_version)));

        let end: serde_json::Value =
            serde_json::from_str(lines[lines.len() - 1]).expect("end 行是 JSON");
        assert_eq!(end["type"], "end");
        assert_eq!(end["match_index"].as_u64(), Some(index as u64));
        assert_eq!(end["seed"].as_u64(), Some(out.results[index as usize].seed));

        for line in &lines[1..lines.len() - 1] {
            let frame: serde_json::Value = serde_json::from_str(line).expect("frame 行是 JSON");
            assert_eq!(frame["type"], "frame");
        }
    }
}

/// 回放必须能通过 `tools/validate_replay.py`（§14 测试 22 的后半句）。
///
/// stub 检查环境（`.stubcheck.sh` 设 `QFR_STUBCHECK=1`）里 sim 是假实现，地图/帧都是空的，
/// 校验器必然报错，所以此时跳过；真实 sim 落地后这个测试是真验收。
#[test]
fn replays_pass_python_validator() {
    if std::env::var_os("QFR_STUBCHECK").is_some() {
        eprintln!("跳过：stub sim 环境（QFR_STUBCHECK=1），等待真实 sim");
        return;
    }
    let out_dir = repo_root().join("target/qfr-validator/run");
    let _ = std::fs::remove_dir_all(&out_dir);
    let out = run_with(
        &out_dir,
        &[
            "--matches", "3",
            "--teams", "2",
            "--max-ticks", "40",
            "--replay-sample", "all",
            "--base-seed", "7",
            "--created-time", "none",
        ],
    );
    let script = repo_root().join("tools/validate_replay.py");
    assert!(script.is_file(), "缺少校验器 {}", script.display());
    for index in 0..out.replays_written {
        let replay = out_dir.join(format!("replays/match_{index:05}.jsonl"));
        assert!(replay.is_file(), "缺少回放 {}", replay.display());
        match Command::new("python3").arg(&script).arg(&replay).status() {
            Ok(status) => assert!(status.success(), "validate_replay.py 报错：{}", replay.display()),
            Err(e) => {
                eprintln!("跳过：无法启动 python3（{e}）");
                return;
            }
        }
    }
}

/// 版本不支持时必须明确报错（§11 `--map-gen-version`）。
#[test]
fn unsupported_map_gen_version_is_rejected() {
    let dir = tempdir();
    let err = common::try_run_with(
        dir.path(),
        &["--matches", "1", "--map-gen-version", "999", "--replay-sample", "none"],
    )
    .expect_err("不支持的 map-gen-version 必须失败");
    assert!(matches!(err, cli::CliError::UnsupportedMapGenVersion { .. }), "{err}");
}

/// 输出目录不存在时自动创建（§11 `-o/--out`）。
#[test]
fn output_dir_is_created() {
    let dir = tempdir();
    let nested = dir.path().join("deep/nested/out");
    assert!(!nested.exists());
    run_with(
        &nested,
        &["--matches", "1", "--replay-sample", "none", "--created-time", "none"],
    );
    for name in ["manifest.json", "summary.json", "matches.jsonl"] {
        assert!(Path::new(&nested).join(name).is_file(), "缺少 {name}");
    }
}
