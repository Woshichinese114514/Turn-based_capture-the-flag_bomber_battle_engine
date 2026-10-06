//! 全 workspace 唯一的确定性随机源：SplitMix64。
//!
//! ## 为什么自己写而不用 `rand`
//!
//! 本项目的两条硬契约都要求「跨进程、跨时间、跨版本稳定」：
//!
//! 1. `同种子 + 同 map_gen_version` ⇒ 地图**逐格完全相同**；
//! 2. `同配置重跑一局` ⇒ 回放 `to_jsonl()` **逐字节相同**（测试 14）。
//!
//! `rand` 的 `StdRng` 只承诺「同一大版本内算法稳定」，上游换一次算法（甚至换一次
//! `rand_chacha` 版本）就会让历史回放、历史统计全部失效。所以把算法连同魔数一起
//! 钉死在本文件里：**本文件一旦发布就不允许再改**（要改就升 `MAP_GEN_VERSION`，
//! 并同步升规则/引擎版本号，见 `docs/replay-format.md` 第 7 节）。
//!
//! SplitMix64 的选择理由：状态只有一个 `u64`（易于序列化/打印复现），输出混淆质量
//! 足够好（通过 BigCrush 等级的常规检验），实现只有十几行、没有任何平台差异
//! （纯 `u64` 环绕算术），非常适合「种子 → 序列」这种契约场景。
//!
//! ## 使用纪律（来自 `docs/internal-api.md` §2）
//!
//! * 一局游戏里 `sim` 只允许持有**一个** `Rng` 实例：炸弹爆炸顺序、旗刷新、掉旗位置、
//!   随机候选格全部从它取数；多开 RNG 会让「哪次抽取属于哪个事件」变得无法复现。
//! * `ai` 用**自己**的、由 `ai_seed` 播种的 `Rng`，绝不允许触碰 `sim` 的实例。

/// SplitMix64 的状态推进常量（黄金比例的无理数近似）。
///
/// 用环绕加法把状态变成 Weyl 序列：即使两个种子只差 1，输出序列也会迅速失去相关性。
const GOLDEN_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
/// 输出混淆第一轮乘子（splitmix64 的推荐常数）。
const MIX_MUL_1: u64 = 0xBF58_476D_1CE4_E5B9;
/// 输出混淆第二轮乘子。
const MIX_MUL_2: u64 = 0x94D0_49BB_1331_11EB;

/// 无状态混淆函数：把任意 `u64` 打散成一个「看起来随机」的 `u64`。
///
/// 它是 [`Rng::next_u64`] 的**纯函数形式**：只做输出混淆，**不推进状态**
/// （`next_u64` = 先把状态加 `GAMMA`，再调用本函数）。两者的区别很重要：
/// 种子推导要的是「同一个输入永远得到同一个输出」，而不是「序列的下一个」。
///
/// 单独暴露它是因为 CLI 需要用它做**种子推导**
/// （`docs/rules.md` 第 10 节：`ai_salt[i] = splitmix64(map_seed ^ i ^ 0x51ED2701)`）。
/// 让 CLI 复用同一个实现，可以避免「种子推导」和「地图生成」用了两套混淆算法
/// 导致复现口径不一致。
pub fn splitmix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(MIX_MUL_1);
    x = (x ^ (x >> 27)).wrapping_mul(MIX_MUL_2);
    x ^ (x >> 31)
}

/// 确定性伪随机数发生器（SplitMix64）。
///
/// 结构体刻意不实现 `Clone`，因为「克隆一个 RNG」在本项目里几乎总是 bug：
/// 两个克隆各自推进会让同一局出现两条互不知晓的随机流。需要多路随机就显式多开
/// 播种（AI 侧），而不是克隆。
pub struct Rng {
    /// 内部状态。私有：外部只能通过 `next_*` 系列推进，保证抽取顺序可审计。
    state: u64,
}

impl Rng {
    /// 用种子构造。同一个种子必然给出同一条序列。
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// 取下一个 `u64`。这是唯一的「真随机步进」入口，其余方法都在它之上实现。
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GOLDEN_GAMMA);
        splitmix64(self.state)
    }

    /// 取一个 `u32`。
    ///
    /// 取**高 32 位**而不是低 32 位：SplitMix64 的高位混淆质量更好，
    /// 低位虽然也通过了检验，但「取高位」是这类算法的惯例做法。
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// 取 `[0, 1)` 的 `f64`。
    ///
    /// 只用高 53 位（`f64` 的尾数宽度）以保证：每个可表示的 `f64` 被等概率取到，
    /// 并且结果严格小于 1（不会因为四舍五入产生 1.0，导致 `floor` 越界）。
    pub fn next_f64(&mut self) -> f64 {
        // 2^-53 = 1 / (1 << 53)
        const SCALE: f64 = 1.0 / (1u64 << 53) as f64;
        (self.next_u64() >> 11) as f64 * SCALE
    }

    /// 均匀取 `[0, len)`。`len == 0` 时返回 0（调用方应自行避免，这里不 panic）。
    ///
    /// 实现是 Lemire 的乘法取高位法（**不做 rejection**）：把 64 位随机数放大成
    /// `u128` 后乘以 `len`，取高 64 位。它的偏置上界是 `len / 2^64`——对现实中所有
    /// 长度（本游戏地图格数 ≤ 2^22）都远小于 2^-40，可以认为无偏；相比 `% len`
    /// 它省掉一次 64 位除法、更快，且偏置更小。若将来需要严格无偏（例如生成密码学
    /// 材料），应改用带 rejection 的版本，而不是在这里悄悄加 `%`。
    pub fn gen_index(&mut self, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        let r = self.next_u64() as u128;
        ((r * len as u128) >> 64) as usize
    }

    /// 均匀取 `[lo, hi_exclusive)` 的 `i32`。区间为空（`hi <= lo`）时返回 `lo`。
    ///
    /// 这里刻意用 `%`：区间宽度通常很小（例如 `[0,100)`），与 `gen_index` 不同，
    /// 偏置在这个尺度上可以忽略；保持实现简单、便于人工验算。
    pub fn gen_range_i32(&mut self, lo: i32, hi_exclusive: i32) -> i32 {
        if hi_exclusive <= lo {
            return lo;
        }
        let span = (hi_exclusive as i64 - lo as i64) as u64;
        lo + (self.next_u64() % span) as i32
    }

    /// 以 `numerator / denominator` 的概率返回 `true`。
    ///
    /// `denominator == 0` 视为概率 0（不 panic）：地图密度校验在更上层完成，
    /// 这里只需要保证「不会除以零」。
    pub fn gen_bool(&mut self, numerator: u32, denominator: u32) -> bool {
        if denominator == 0 {
            return false;
        }
        let n = numerator.min(denominator);
        (self.next_u64() % denominator as u64) < n as u64
    }

    /// 从切片里均匀取一个下标；空切片返回 `None`。
    ///
    /// 替代写法 `slice.get(self.gen_index(slice.len()))` 会在空切片时静默取到下标 0，
    /// 因此这里显式返回 `Option`，逼调用方处理「没有候选」的情形（例如刷旗时
    /// 中心区域一个合法格都没有）。
    pub fn pick_index<T>(&mut self, slice: &[T]) -> Option<usize> {
        if slice.is_empty() {
            return None;
        }
        Some(self.gen_index(slice.len()))
    }

    /// 原地 Fisher–Yates 洗牌。
    ///
    /// 用途：同一 tick 有多枚炸弹同时到点爆炸时，用一次洗牌决定结算先后。
    /// 为什么不按炸弹 ID 顺序结算？因为 ID 顺序等价于「放置顺序」，会让先放炸弹的
    /// 玩家在连锁爆炸里恒定占优（例如两枚炸弹同时炸同一个残血单位，谁先结算谁拿击杀）。
    /// 洗牌后顺序仍然完全确定（来自同一个种子驱动的 RNG），只是不再与位置/放置次序相关。
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        let n = slice.len();
        if n < 2 {
            return;
        }
        // 从尾部往前，每次在 [0, i] 里选一个换过来；能保证每种排列等概率。
        for i in (1..n).rev() {
            let j = self.gen_index(i + 1);
            slice.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::new(20240501);
        let mut b = Rng::new(20240501);
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64(), "同种子必须给出同一条序列");
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        let differs = (0..8).any(|_| a.next_u64() != b.next_u64());
        assert!(differs, "相邻种子的早期输出不应完全相同");
    }

    #[test]
    fn gen_index_and_ranges_are_in_bounds() {
        let mut rng = Rng::new(7);
        for _ in 0..1000 {
            assert!(rng.gen_index(7) < 7);
            let v = rng.gen_range_i32(-3, 4);
            assert!((-3..4).contains(&v), "越界：{v}");
        }
        assert_eq!(rng.gen_index(0), 0, "空长度不 panic");
        assert_eq!(rng.gen_range_i32(5, 5), 5, "空区间退化为下界");
        assert!(rng.pick_index::<u8>(&[]).is_none());
    }

    #[test]
    fn next_f64_stays_in_unit_interval() {
        let mut rng = Rng::new(99);
        for _ in 0..1000 {
            let v = rng.next_f64();
            assert!((0.0..1.0).contains(&v), "next_f64 越界：{v}");
        }
    }

    #[test]
    fn shuffle_is_a_permutation_and_deterministic() {
        let mut rng = Rng::new(5);
        let mut data: Vec<u32> = (0..50).collect();
        rng.shuffle(&mut data);
        let mut sorted = data.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>(), "洗牌必须是排列");

        let mut rng2 = Rng::new(5);
        let mut data2: Vec<u32> = (0..50).collect();
        rng2.shuffle(&mut data2);
        assert_eq!(data, data2, "同种子的洗牌结果必须一致");
    }

    #[test]
    fn gen_bool_boundaries() {
        let mut rng = Rng::new(11);
        for _ in 0..100 {
            assert!(!rng.gen_bool(0, 100), "概率 0 永远 false");
            assert!(rng.gen_bool(100, 100), "概率 1 永远 true");
            assert!(!rng.gen_bool(1, 0), "分母 0 视为 false，不 panic");
        }
    }
}
