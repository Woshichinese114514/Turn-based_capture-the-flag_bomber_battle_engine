//! 两个纯函数：`原始指标 → strength` 与 `strength → rating`。
//!
//! 刻意拆成两级，是为了让将来的算法替换最小化：
//! * 换成 ELO 时，用「期望得分」替代 [`compute_strength`] 的输出即可；
//! * 换成 TrueSkill 时，只替换 [`compute_rating`] 的映射；
//! * 两者都是纯函数（无状态、无 IO），因此可以用写死期望值的单元测试锁住。

use crate::config::ScoringConfig;

/// 把 `strength` 收进 `[min_strength, max_strength]`，并且**兜住 NaN / ±inf**。
///
/// 为什么需要兜底而不是直接 `clamp`：`avg_score_share` 等在极端输入下可能产生 NaN
/// （例如将来引入 `0/0` 的新指标），而 NaN 的 `clamp` 会原样返回 NaN，
/// 一路污染到 `log10`，最后在 JSON 里序列化成 `null`，排查成本极高。
/// 这里的约定：
/// * `NaN` → 0.5（平均水准，没有任何有效信息）；
/// * `+inf` → `max_strength`，`-inf` → `min_strength`（保留「极强/极弱」的方向感）。
pub(crate) fn clamp_strength(strength: f64, config: &ScoringConfig) -> f64 {
    // 防御性处理：如果配置里 min > max（人为传参错误），以两者构成的最小区间为准，
    // 保证区间非空、避免 clamp panic。
    let lo = config.min_strength.min(config.max_strength);
    let hi = config.min_strength.max(config.max_strength);
    if strength.is_nan() {
        return 0.5;
    }
    if strength == f64::INFINITY {
        return hi;
    }
    if strength == f64::NEG_INFINITY {
        return lo;
    }
    strength.clamp(lo, hi)
}

/// 小样本收缩：把胜率朝 0.5 拉，等价于先验 `prior` 局五五开。
///
/// `shrunk = (wins + 0.5·prior) / (matches + prior)`
///
/// 为什么不直接用 `wins/matches`：批量跑 20 局时，一个 AI 完全可能靠运气 20 胜 0 负，
/// 原始胜率 1.0 会被上限截断成 0.98，看起来像「强 49 倍」。加上先验后，
/// 20 胜 0 负的收缩胜率约为 0.9（约 1450 分），而 200 胜 0 负才逼近 0.975，
/// 分数随样本量增长——这正是统计上应有的谨慎。
pub fn shrink_win_rate(wins: f64, matches: f64, prior: f64) -> f64 {
    if !wins.is_finite() || !matches.is_finite() || !prior.is_finite() {
        return 0.5;
    }
    if matches <= 0.0 {
        // 没打过比赛：没有任何证据，给平均水准，而不是 0 或 NaN。
        return 0.5;
    }
    let prior = prior.max(0.0);
    let shrunk = (wins + 0.5 * prior) / (matches + prior);
    shrunk.clamp(0.0, 1.0)
}

/// 第一步：三个归一化指标 → 综合实力 `strength ∈ [min_strength, max_strength]`。
///
/// ```text
/// strength_raw = (w_win·win_rate + w_score·score_share + w_kill·kill_ratio) / (w_win + w_score + w_kill)
/// strength     = clamp(strength_raw, min_strength, max_strength)
/// ```
///
/// 权重做**归一化**（除以权重和），保证任意权重组合下结果是 [0,1] 的凸组合，
/// 而不是「权重越大分越高」这种没有意义的依赖。
/// 若三个权重都为 0（配置错误），退化为 0.5 而不是 NaN 或 panic。
pub fn compute_strength(
    win_rate: f64,
    avg_score_share: f64,
    avg_kill_ratio: f64,
    config: &ScoringConfig,
) -> f64 {
    let weight_sum =
        config.weight_win_rate + config.weight_score_share + config.weight_kill_ratio;
    let raw = if weight_sum > 0.0 {
        (config.weight_win_rate * win_rate
            + config.weight_score_share * avg_score_share
            + config.weight_kill_ratio * avg_kill_ratio)
            / weight_sum
    } else {
        // 没有权重 = 没有可用的区分信息 → 平均水准。
        0.5
    };
    clamp_strength(raw, config)
}

/// 第二步：`strength → rating`，log scale 的关键一行。
///
/// ```text
/// odds   = strength / (1 - strength)
/// rating = baseline_rating + points_per_decade · log10(odds)
/// ```
///
/// 因为 `log10` 以 10 为底、系数是 500，所以 **rating 每 +500 ⇔ odds ×10 ⇔ 强 10 倍**；
/// +1000 分 ⇔ 强 100 倍。`strength` 先被 clamp 到 `[min, max]`，
/// 保证 `odds` 恒为正、有限，`log10` 不会出现 `-inf`/`+inf`。
pub fn compute_rating(strength: f64, config: &ScoringConfig) -> f64 {
    let s = clamp_strength(strength, config);
    let odds = s / (1.0 - s);
    config.baseline_rating + config.points_per_decade * odds.log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试 17：公式稳定性——固定输入必须给出固定输出（写死期望值，锁住公式）。
    #[test]
    fn rating_formula_is_stable_for_fixed_inputs() {
        let c = ScoringConfig::default();
        // 三个指标全是 0.5 → strength 恰好 0.5 → 基准分 1000。
        let s = compute_strength(0.5, 0.5, 0.5, &c);
        assert!((s - 0.5).abs() < 1e-12, "加权平均应为 0.5，得到 {s}");
        assert!((compute_rating(s, &c) - 1000.0).abs() < 1e-9);

        // 偏重胜率的例子：win_rate=0.75, share=0.6, kill=0.5
        // raw = (0.6*0.75 + 0.25*0.6 + 0.15*0.5) / 1.0 = 0.45+0.15+0.075 = 0.675
        let s = compute_strength(0.75, 0.6, 0.5, &c);
        assert!((s - 0.675).abs() < 1e-12, "得到 {s}");
        // rating = 1000 + 500*log10(0.675/0.325)
        let expected = 1000.0 + 500.0 * (0.675f64 / 0.325).log10();
        assert!((compute_rating(s, &c) - expected).abs() < 1e-9);

        // 纯 0 → 被压到 min_strength = 0.02：
        // rating = 1000 + 500·log10(0.02/0.98) ≈ +154.9，**有限**（不会 -inf）。
        let s = compute_strength(0.0, 0.0, 0.0, &c);
        assert!((s - 0.02).abs() < 1e-12);
        let r = compute_rating(s, &c);
        assert!(r.is_finite() && r < 1000.0, "0.02 → {r}");

        // 纯 1 → 被压到 max_strength = 0.98：
        // rating = 1000 + 500·log10(0.98/0.02) ≈ +1845.1，**有限**（不会 +inf）。
        let s = compute_strength(1.0, 1.0, 1.0, &c);
        assert!((s - 0.98).abs() < 1e-12);
        let r = compute_rating(s, &c);
        assert!(r.is_finite() && r > 1800.0, "0.98 → {r}");
    }

    /// 测试 18：语义测试——odds 提高 10 倍时 rating 恰好 +500（容差 1e-6）。
    #[test]
    fn ten_times_odds_is_exactly_500_points() {
        let c = ScoringConfig::default();

        // 0.5 → odds 1 → 1000 分
        let r_avg = compute_rating(0.5, &c);
        assert!((r_avg - 1000.0).abs() < 1e-6, "基准分 {r_avg}");

        // strength = 10/11 ≈ 0.90909 → odds = 10 → 1500 分
        let s10 = 10.0f64 / 11.0;
        let r10 = compute_rating(s10, &c);
        assert!(
            (r10 - 1500.0).abs() < 1e-6,
            "odds×10 应为 1500，得到 {r10}"
        );
        assert!(
            (r10 - r_avg - 500.0).abs() < 1e-6,
            "每 10 倍实力恰好 +500 分：{r10} - {r_avg}"
        );

        // strength = 100/101 ≈ 0.990099 → odds = 100 → 2000 分（+500 的叠加性）。
        // 注意：默认 max_strength = 0.98 < 100/101，会被 clamp 掉，
        // 所以这里必须临时放宽上限，才能观察到第二个十倍台阶。
        let wide = ScoringConfig {
            max_strength: 0.999,
            min_strength: 0.001,
            ..ScoringConfig::default()
        };
        let s100 = 100.0f64 / 101.0;
        let r100 = compute_rating(s100, &wide);
        assert!((r100 - 2000.0).abs() < 1e-6, "odds×100 应为 2000，得到 {r100}");
        let r10_wide = compute_rating(s10, &wide);
        assert!((r100 - r10_wide - 500.0).abs() < 1e-6);
        // 默认配置下 100/101 被压到上限：1000 + 500·log10(0.98/0.02) ≈ 1845.1
        let clamped = compute_rating(s100, &c);
        assert!((clamped - compute_rating(0.98, &c)).abs() < 1e-12);
        assert!((clamped - 1845.0980400142566).abs() < 1e-9, "得到 {clamped}");

        // 反向：strength = 1/11 ≈ 0.0909 → odds = 1/10 → 500 分（弱 10 倍）
        let s_low = 1.0f64 / 11.0;
        let r_low = compute_rating(s_low, &c);
        assert!((r_low - 500.0).abs() < 1e-6, "弱 10 倍应为 500，得到 {r_low}");
        assert!(
            (r_avg - r_low - 500.0).abs() < 1e-6,
            "反向也是每 10 倍 500 分"
        );
    }

    /// 小样本收缩：证据越多，胜率越接近原始值；0 局给平均。
    #[test]
    fn win_rate_shrinks_toward_half_until_enough_matches() {
        let prior = 5.0;
        // 3 胜 0 负： (3 + 2.5) / (3 + 5) = 0.6875，而不是 1.0
        let three = shrink_win_rate(3.0, 3.0, prior);
        assert!((three - 0.6875).abs() < 1e-12, "得到 {three}");
        // 100 胜 0 负： (100+2.5)/105 ≈ 0.97619，仍然被拉低
        let hundred = shrink_win_rate(100.0, 100.0, prior);
        assert!(hundred < 1.0 && hundred > 0.97, "得到 {hundred}");
        // 收缩是单调的：样本越多越接近 1
        assert!(three < hundred);
        // 0 局 → 0.5，不 panic、不 NaN
        assert_eq!(shrink_win_rate(0.0, 0.0, prior), 0.5);
        // 先验为 0 时退化为原始胜率
        assert!((shrink_win_rate(2.0, 4.0, 0.0) - 0.5).abs() < 1e-12);
    }

    /// 非有限输入不能让报告变成 NaN/null。
    #[test]
    fn non_finite_inputs_degrade_to_average() {
        let c = ScoringConfig::default();
        assert!((compute_strength(f64::NAN, 0.5, 0.5, &c) - 0.5).abs() < 1e-12);
        assert!((compute_strength(0.5, f64::INFINITY, 0.5, &c) - 0.98).abs() < 1e-12);
        assert!((compute_rating(f64::NAN, &c) - 1000.0).abs() < 1e-12);
        // 权重全 0：没有信息 → 平均水准
        let zero_weights = ScoringConfig {
            weight_win_rate: 0.0,
            weight_score_share: 0.0,
            weight_kill_ratio: 0.0,
            ..ScoringConfig::default()
        };
        assert!((compute_strength(1.0, 1.0, 1.0, &zero_weights) - 0.5).abs() < 1e-12);
    }

    /// 分数体系与换算无关：同样的 strength 在 2 队/3 队配置下映射相同，
    /// 差别只在于 `teams` 标注与「能不能互相比较」。
    #[test]
    fn rating_mapping_is_shared_but_teams_tag_differs() {
        let two = ScoringConfig::for_teams(2).expect("2 队");
        let three = ScoringConfig::for_teams(3).expect("3 队");
        assert_eq!(two.teams, 2);
        assert_eq!(three.teams, 3);
        assert_eq!(compute_rating(0.7, &two), compute_rating(0.7, &three));
    }
}
