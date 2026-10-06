//! docs/rules.md §14 测试 21：批量可复现。
//!
//! 「同 `base-seed` + 同 `seed-mode` + 同局数跑两次，`matches.jsonl` 逐行相同」
//! 是整个工具链的地基：只要它成立，任何一次评测都能被完整复算。

mod common;

use common::{read, run_with, tempdir};

/// `matches.jsonl` 与 `manifest.json` 逐字节可复现（`--created-time none` 抹掉时间）。
#[test]
fn batch_is_byte_reproducible() {
    let extra = [
        "--matches", "5",
        "--teams", "3",
        "--max-ticks", "80",
        "--seed-mode", "per-match",
        "--base-seed", "20240501",
        "--replay-sample", "none",
        "--created-time", "none",
    ];
    let first = tempdir();
    let second = tempdir();
    let a = run_with(first.path(), &extra);
    // 第二次跑只为「落盘对比」：断言全部走文件，所以这里刻意丢弃返回值（不参与断言）。
    run_with(second.path(), &extra);

    assert_eq!(a.results.len(), 5);
    assert_eq!(a.replays_written, 0, "replay-sample=none 不应写回放");

    let matches_a = read(&first.path().join("matches.jsonl"));
    let matches_b = read(&second.path().join("matches.jsonl"));
    assert_eq!(matches_a, matches_b, "matches.jsonl 必须逐行相同");

    let manifest_a = read(&first.path().join("manifest.json"));
    let manifest_b = read(&second.path().join("manifest.json"));
    assert_eq!(manifest_a, manifest_b, "manifest.json 必须逐字节相同");

    // summary.json 里的评分汇总同样应当只由对局结果决定。
    assert_eq!(
        read(&first.path().join("summary.json")),
        read(&second.path().join("summary.json")),
        "summary.json 必须逐字节相同"
    );

    // 行数 = 局数；每行的 map_gen_version 与 manifest 一致（评测脚本会校验）。
    let rows: Vec<&str> = matches_a.lines().collect();
    assert_eq!(rows.len(), 5);
    for row in &rows {
        let value: serde_json::Value = serde_json::from_str(row).expect("每行必须是 JSON");
        assert_eq!(value["map_gen_version"].as_u64(), Some(u64::from(a.manifest.map_gen_version)));
        assert!(value["seed"].as_u64().is_some(), "每行必须带地图种子：{row}");
        assert!(value["ai_seeds"].as_array().is_some(), "每行必须带 ai_seeds：{row}");
    }
    assert_eq!(a.manifest.seed_plan.map_seed_sample.len(), 5);
}

/// 局索引必须升序、连续，且 `matches.jsonl` 与内存结果一一对应（§12 写入要求）。
#[test]
fn matches_jsonl_is_sorted_by_match_index() {
    let extra = [
        "--matches", "6",
        "--teams", "2",
        "--max-ticks", "60",
        "--base-seed", "99",
        "--replay-sample", "none",
    ];
    let dir = tempdir();
    let out = run_with(dir.path(), &extra);

    for (index, result) in out.results.iter().enumerate() {
        assert_eq!(result.match_index, index as u32, "results 必须按局索引升序");
    }
    let text = read(&dir.path().join("matches.jsonl"));
    let indices: Vec<u64> = text
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("每行必须是 JSON");
            value["match_index"].as_u64().expect("每行有 match_index")
        })
        .collect();
    assert_eq!(indices, (0..6).collect::<Vec<u64>>());
}
