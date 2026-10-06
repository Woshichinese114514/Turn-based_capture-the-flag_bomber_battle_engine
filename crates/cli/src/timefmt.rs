//! 时间与熵：只依赖标准库的 UTC 时间格式化 + 运行期熵源。
//!
//! # 为什么手写 RFC3339 而不引 chrono
//!
//! 本项目对时间的唯一需求是「在 manifest.json 里写一个人类可读的创建时间」。
//! 为这一个字段引入 chrono（及其时区数据库）会让交付物的依赖树变重，
//! 而 `civil_from_days` 是十几行、可单测的成熟算法（Howard Hinnant 的日期算法）。
//! 精度到秒、固定 UTC（文件名与跨时区比对都少一层歧义）。
//!
//! # 为什么需要「熵」
//!
//! 用户没给 `--base-seed` 时，`per-match` / `fixed-random` 必须真的随机，
//! 否则每次运行都得到同一批地图（用户会以为程序坏了）。这里用「系统时间纳秒 + 进程号」
//! 混出一个 64 位熵值：无需额外依赖，且两次调用撞车的概率可忽略。
//! 注意：熵只在**没给 base-seed** 时参与，所以「同 base-seed → 同结果」的可复现性不受影响。

use std::time::{SystemTime, UNIX_EPOCH};

// 复用 `cli::seed::mix64`（它本身是 `mapgen::Rng` 的第一步，零本地算术）：熵只用于
// 「没有 --base-seed」的不可复现场景，但仍要和种子推导同口径，避免两套算法分叉。
use crate::seed::mix64;

/// 当前 Unix 时间戳（秒）。时钟异常（早于 1970）时返回 0，绝不 panic。
pub fn now_unix_secs() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    }
}

/// 把 Unix 时间戳（秒）格式化为 `YYYY-MM-DDTHH:MM:SSZ`（UTC）。
///
/// 用 `div_euclid` / `rem_euclid` 而不是 `/`、`%`：后者对负数取整方向不对，
/// 会让 1970 年之前的时刻算错一天。
pub fn format_rfc3339(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// 把「1970-01-01 起的天数」转成公历 (年, 月, 日)。
///
/// 这是 Howard Hinnant 的 `civil_from_days`：先在「400 年一轮（146097 天）」的
/// 尺度上定位纪元，再在年内用 153 天的月份模式反推月日。实现里所有除法都是整数除法
/// 且被除数非负，所以不需要再处理负数舍入。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // 把纪元从 1970-01-01 移到 0000-03-01，让闰日落在「年末」，月份模式才整齐。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // 纪元内第几天 [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]，3 月为 0
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 运行期熵：时间（纳秒）+ 进程号，再走一次 `mix64`（`seed::mix64`，即 mapgen 的 SplitMix64
/// 一次性混淆，零本地算术）混合，避免低位规律。它只在没有 `--base-seed` 时被用到，
/// 所以「同 base-seed → 同结果」的可复现性不受影响。
pub fn entropy_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = u64::from(std::process::id());
    mix64(nanos ^ (pid << 32) ^ 0xA5A5_5A5A_1234_5678)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_714_521_600), "2024-05-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format_rfc3339(1_704_067_199), "2023-12-31T23:59:59Z");
        // 世纪闰年：2000-02-29 是闰日，1900 不是（1900-02-28 的后一天是 03-01）。
        assert_eq!(format_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_rfc3339(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn round_trips_day_boundaries() {
        // 每天 00:00:00 都能被整除到正确日期（防止 div_euclid 写错）。
        for day in [0i64, 1, 31, 365, 3_652, 19_782] {
            let text = format_rfc3339(day * 86_400);
            assert!(text.ends_with("T00:00:00Z"), "{day} → {text}");
        }
    }

    #[test]
    fn now_and_entropy_are_sane() {
        let secs = now_unix_secs();
        // 2020-01-01 之后（脚本环境的时间不会更早），且小于 2100 年。
        assert!(secs > 1_577_836_800 && secs < 4_102_444_800, "secs={secs}");
        assert_ne!(entropy_seed(), 0);
    }
}
