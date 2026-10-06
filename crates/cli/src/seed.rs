//! 种子计划：把 `--seed-mode` / `--seed` / `--base-seed` 解析成「每局地图种子 + 每队 AI 种子」。
//!
//! # 为什么要有独立的「种子计划」这一步
//!
//! 批量跑 1000 局时，种子有两条完全不同的来源：
//!
//! 1. **地图种子**：决定生成哪张地图；
//! 2. **AI 种子**：决定每个 AI 内部的随机数（走位抖动、开火选择等）。
//!
//! 如果边跑边摇骰子（在并行闭包里 `rand()`），那么「同一条命令跑两次结果不同」，
//! 任何回归对比都失去意义。所以本模块**在并行之前一次性算出所有种子**，
//! 存进 `Vec`，并行阶段只做只读查表：并行只影响耗时，不影响结果。
//!
//! 三种模式的语义（见 `docs/rules.md` §10）：
//!
//! | 模式 | 地图种子 | AI 随机盐 |
//! |---|---|---|
//! | `per-match` | 每局一个新种子（整批地图都不同） | 直接用该局地图种子 |
//! | `fixed-random` | 随机摇**一个**种子，全批复用（同图） | 按局索引推导，避免「图同 AI 也同 → 100 局结果一模一样」 |
//! | `fixed` | 用户指定的固定种子，全批复用 | 同 `fixed-random` |
//!
//! `fixed-random` 的陷阱值得再强调一次：如果 AI 也完全确定，那么 1000 局会得到
//! 1000 个**完全相同**的结果，胜率统计就成了 0 或 1 的噪声。所以 AI 盐按局索引
//! 确定性推导：图不变、AI 抖动变，既保留了「同一张图的公平对比」，又让统计有意义。
//!
//! # 确定性来源：SplitMix64 全 workspace 只有一处实现
//!
//! 全部随机性来自 `mapgen`（`crates/mapgen/src/rng.rs`），本模块**不再自带任何混淆算术**：
//!
//! * 顺序取种子（`per-match` 的整批地图种子）用 [`mapgen::Rng`]；
//! * 一次性种子混淆（§10 的 `ai_salt[i] = splitmix64(map_seed ^ i ^ 0x51ED_2701)`）用
//!   本模块的 [`mix64`]，它只是一层命名：`Rng::new(z).next_u64()`，即「以 `z` 播种的流的第 1 个输出」。
//!
//! **为什么只留一处实现**：§10 的种子公式与地图生成共用同一套 SplitMix64 混淆。若 CLI
//! 复制一份算术，两份「目前恰好相同」的实现迟早会在某次「顺手优化」后分叉；而分叉极难被
//! 测试发现（两边各自的测试都还是绿的），最终表现为「同 seed 复现不出同一张图 / 同一批 AI」。
//! 所以这里只 **调用** `mapgen` 的实现，不复制它的算术。
//!
//! **两个 API 的分工（极易用错：差一次 `+= GAMMA`）**：
//!
//! * [`mapgen::splitmix64`] 是 `Rng::next_u64` 内部的**输出混淆**，**不加**黄金比例常量：
//!   纯函数、不推进状态。注意 `mapgen::splitmix64(0) == 0`，它**不是** §10 说的那个「一次性哈希」。
//! * [`mapgen::Rng`] 是**状态推进**的流：`next_u64()` = 状态先 `+= GAMMA`，再做上面的输出混淆。
//! * [`mix64`] 才是 §10 公式里的「一次性种子混淆」口径 = `Rng::new(z).next_u64()` =
//!   `splitmix64(z + GAMMA)`（标准的一次性 splitmix64 哈希，先加常量再混淆）。
//!   直接写 `mapgen::splitmix64(z)` 会少加一次 `GAMMA`——所有种子都会变，历史回放全部失效。
//!   下面 `mix64_reference_values_are_stable` 与 `mapgen_splitmix64_is_not_the_one_shot_hash`
//!   两个测试把这条差别钉死。

use mapgen::Rng;
use serde::Serialize;

use crate::error::CliError;

/// 团队位次在 `ai_seed` 中的位移：把「同一局不同队伍」的随机流彻底分开。
const TEAM_SHIFT: u32 = 32;

/// `ai_seed = ai_salt ^ ((team) << 32) ^ AI_SEED_XOR`。
///
/// 这个常量来自 splitmix64 的黄金比例常数 `0x9E3779B97F4A7C15` 的低 32 位。
/// 作用是把「队伍 0 的种子」与「原始盐」拉开，避免出现 `ai_seed == map_seed`
/// 这种一眼就能猜到的关系（否则不同局的 AI 行为可能因为地图种子相邻而相关）。
pub const AI_SEED_XOR: u64 = 0x9E37_79B9;

/// `fixed-random` / `fixed` 模式里「按局索引推导 AI 盐」用的异或常量。
///
/// 取一个与内容无关的固定常数，是为了让 `ai_salt[i] = splitmix64(map_seed ^ i ^ SALT)`
/// 在 `i` 相差 1 时也完全不同（否则相邻局的 AI 随机流会高度相关）。
pub const AI_SALT_XOR: u64 = 0x51ED_2701;

// 本模块原本带有一份 `mix64` + `SplitMix64` 的**完整算术**（乘子、移位、黄金比例常量都在这里）。
// 为消除「同一契约公式两处实现」的复现风险，现已删除那份算术，只留下下面这层「零算术」的命名
// 包装：全 workspace 的 SplitMix64 混淆只在 `crates/mapgen/src/rng.rs` 一处。

/// 一次性种子混淆（`docs/rules.md` §10 公式里的 `splitmix64(x)`）。
///
/// 语义 = 「以 `x` 播种的 SplitMix64 流的**第 1 个输出**」= 标准的一次性 splitmix64 哈希
/// （**先加黄金比例常量**，再做输出混淆）。它没有任何本地算术：直接复用 [`mapgen::Rng`]
/// 的第一步，因此将来 mapgen 的混淆若真被改动，CLI 的种子会**同时**改变（而不是悄悄分叉）。
///
/// 为什么不用同名参数直接调 `mapgen::splitmix64`：那个函数是 `Rng` 内部的**输出混淆**，不含
/// `+= GAMMA`，`mapgen::splitmix64(0) == 0`，与 §10 的一次性哈希口径差一次加法（见模块文档）。
/// 本函数 = `splitmix64(x + GAMMA)`，这才是历史行为与 `docs/rules.md` §10 想要的那个。
pub fn mix64(z: u64) -> u64 {
    Rng::new(z).next_u64()
}

/// 由「AI 盐」推导某队的 AI 种子（见 `docs/rules.md` §10）。
///
/// 公式固定为 `ai_salt ^ ((team as u64) << 32) ^ 0x9E3779B9`：
/// 高位放队伍号是为了让「同盐不同队」的随机流相距极远，
/// 低 32 位异或常量则避免低比特位的规律性（splitmix64 的种子对低位很敏感）。
pub fn ai_seed_from_salt(ai_salt: u64, team: u8) -> u64 {
    ai_salt ^ ((team as u64) << TEAM_SHIFT) ^ AI_SEED_XOR
}

/// 种子模式（CLI 的 `--seed-mode`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeedMode {
    /// 每局一个新随机种子（每局地图不同）。
    PerMatch,
    /// 随机一个种子，批量全部复用（所有局同图，AI 盐按局索引变化）。
    FixedRandom,
    /// 用户指定固定种子，批量全部复用。
    Fixed,
}

impl SeedMode {
    /// 写进 `manifest.json` 的稳定字符串（kebab-case，与 CLI 取值一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            SeedMode::PerMatch => "per-match",
            SeedMode::FixedRandom => "fixed-random",
            SeedMode::Fixed => "fixed",
        }
    }

    /// 该模式是否使用 `--seed`（只有 `fixed` 用）。
    pub fn uses_fixed_seed(self) -> bool {
        matches!(self, SeedMode::Fixed)
    }

    /// 该模式是否使用 `--base-seed`（`fixed` 模式用不上：固定种子本身就是确定的）。
    pub fn uses_base_seed(self) -> bool {
        !matches!(self, SeedMode::Fixed)
    }
}

impl std::fmt::Display for SeedMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一整批对局的种子计划：`map_seeds[i]` 是第 i 局的地图种子，
/// `ai_seeds[i][team]` 是第 i 局第 team 队的 AI 种子。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedPlan {
    /// 使用的模式（回写进 manifest，便于复盘「这批统计是哪种抽样」）。
    pub mode: SeedMode,
    /// 种子生成器的基准种子（`None` = 用运行时的熵播种，因此不可复现）。
    pub base_seed: Option<u64>,
    /// `fixed` 模式的固定种子。
    pub fixed_seed: Option<u64>,
    /// 每局地图种子，长度 = 局数。
    pub map_seeds: Vec<u64>,
    /// 每局每队的 AI 种子，`[局][队]`。
    pub ai_seeds: Vec<Vec<u64>>,
}

impl SeedPlan {
    /// 生成整批种子。**必须在启动 rayon 之前调用**（见模块文档）。
    ///
    /// * `entropy` 只在「用户没给 `--base-seed`（或 `fixed` 模式外的随机源）」时使用，
    ///   由调用方从系统时间/进程号取，保证不写死一个假随机。
    pub fn generate(
        mode: SeedMode,
        matches: u32,
        teams: u8,
        base_seed: Option<u64>,
        fixed_seed: Option<u64>,
        entropy: u64,
    ) -> Result<Self, CliError> {
        let count = matches as usize;
        let team_count = teams as usize;
        let mut map_seeds = Vec::with_capacity(count);
        let mut ai_seeds = Vec::with_capacity(count);

        match mode {
            SeedMode::PerMatch => {
                // 整批种子由一个生成器顺序吐出：这样 seeds[1] 依赖 seeds[0]，
                // 任何并行都不会改变「谁拿到哪个种子」。
                let mut rng = Rng::new(base_seed.unwrap_or(entropy));
                for _ in 0..count {
                    let map_seed = rng.next_u64();
                    map_seeds.push(map_seed);
                    // per-match：AI 盐就是地图种子（每局地图都新，AI 随机流自然不同）。
                    let seeds = (0..team_count)
                        .map(|team| ai_seed_from_salt(map_seed, team as u8))
                        .collect();
                    ai_seeds.push(seeds);
                }
            }
            SeedMode::FixedRandom => {
                // 只摇一次：整批复用同一张地图。
                let map_seed = match base_seed {
                    // `Some(seed)`：取该生成器的**第一个**输出（等价于 `splitmix64(seed)`）。
                    Some(seed) => Rng::new(seed).next_u64(),
                    // 没有 base-seed：直接用一次性混淆把运行期熵打散，不引入「第几个输出」的语义。
                    None => mix64(entropy),
                };
                for index in 0..count {
                    map_seeds.push(map_seed);
                    ai_seeds.push(ai_seeds_for_fixed_map(map_seed, index, team_count));
                }
            }
            SeedMode::Fixed => {
                let map_seed = fixed_seed.ok_or(CliError::MissingFixedSeed)?;
                for index in 0..count {
                    map_seeds.push(map_seed);
                    ai_seeds.push(ai_seeds_for_fixed_map(map_seed, index, team_count));
                }
            }
        }

        Ok(Self {
            mode,
            base_seed,
            fixed_seed,
            map_seeds,
            ai_seeds,
        })
    }

    /// 第 `index` 局的地图种子（越界时返回 0，绝不 panic：评分/统计代码不应因为一次越界就崩掉）。
    pub fn map_seed(&self, index: usize) -> u64 {
        self.map_seeds.get(index).copied().unwrap_or(0)
    }

    /// 第 `index` 局第 `team` 队的 AI 种子。
    pub fn ai_seed(&self, index: usize, team: u8) -> u64 {
        self.ai_seeds
            .get(index)
            .and_then(|row| row.get(team as usize))
            .copied()
            .unwrap_or(0)
    }

    /// 局数。
    pub fn len(&self) -> usize {
        self.map_seeds.len()
    }

    /// 是否为空计划（0 局；正常不会出现，`--matches` 已校验 ≥ 1）。
    pub fn is_empty(&self) -> bool {
        self.map_seeds.is_empty()
    }

    /// 供 `manifest.json` 记录的「解析后种子信息」。
    pub fn info(&self, sample_size: usize) -> SeedPlanInfo {
        let n = sample_size.min(self.len());
        SeedPlanInfo {
            mode: self.mode.as_str().to_string(),
            base_seed: self.base_seed,
            fixed_seed: self.fixed_seed,
            total_matches: self.len(),
            map_seed_sample: self.map_seeds[..n].to_vec(),
            ai_seed_sample: self.ai_seeds[..n].to_vec(),
        }
    }
}

/// `fixed-random` / `fixed`：地图固定时，AI 盐按局索引确定性推导。
///
/// `ai_salt[i] = splitmix64(map_seed ^ i ^ 0x51ED_2701)`，与 §10 的公式逐字一致；
/// 这里的 `splitmix64` 就是本模块的 [`mix64`]（一次性哈希口径，见模块文档）。
fn ai_seeds_for_fixed_map(map_seed: u64, index: usize, team_count: usize) -> Vec<u64> {
    let ai_salt = mix64(map_seed ^ (index as u64) ^ AI_SALT_XOR);
    (0..team_count)
        .map(|team| ai_seed_from_salt(ai_salt, team as u8))
        .collect()
}

/// 写进 manifest 的种子信息（模式、base_seed、样本、总数）。
#[derive(Clone, Debug, Serialize)]
pub struct SeedPlanInfo {
    /// 模式字符串（kebab-case）。
    pub mode: String,
    /// 种子生成器基准种子。
    pub base_seed: Option<u64>,
    /// 固定种子（仅 `fixed` 模式）。
    pub fixed_seed: Option<u64>,
    /// 本批总局数（= 种子数组长度）。
    pub total_matches: usize,
    /// 前几局的地图种子样本（默认 5 个，便于人工核对复现性）。
    pub map_seed_sample: Vec<u64>,
    /// 前几局的 AI 种子样本，`[局][队]`。
    pub ai_seed_sample: Vec<Vec<u64>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    // 只在测试里引用：用来证明 `mapgen::splitmix64`（输出混淆）与本模块 `mix64`（一次性哈希）
    // **不是**同一个口径（见 `mapgen_splitmix64_is_not_the_one_shot_hash`）。
    use mapgen::splitmix64;

    /// SplitMix64 流的参考序列（期望值由独立实现算得，写死以防将来「顺手改一下混合函数」）。
    ///
    /// 用的是 `mapgen::Rng`（本模块已不再自带流实现），所以这组值同时是**跨 crate 契约**：
    /// `mapgen` 的混淆算法一旦被改动（且没升 `MAP_GEN_VERSION`），这里会立刻失败。
    ///
    /// 注意两个 API 的关系：`Rng::new(z).next_u64()` 的**第一个**输出就等于本模块的
    /// [`mix64(z)`]（两者都做 `fmix64(z + GAMMA)`）。序列从这里开始，不要把它和第 2 个输出
    /// 对齐——这是最容易写错的一位偏移。
    #[test]
    fn splitmix64_stream_first_values_are_stable() {
        let mut rng = Rng::new(7);
        let got: Vec<u64> = (0..5).map(|_| rng.next_u64()).collect();
        assert_eq!(
            got,
            vec![
                7191089600892374487,
                309689372594955804,
                16616101746815609346,
                10753165928301472203,
                8346079845500723674,
            ]
        );
        assert_eq!(mix64(7), got[0], "一次性混淆 == 流的第一步（同一次 fmix64(z+GAMMA)）");
    }

    /// 一次性种子混淆的参考值：这是「种子推导」的**契约值**，任何改动都等于让历史回放失效。
    #[test]
    fn mix64_reference_values_are_stable() {
        assert_eq!(mix64(0), 16294208416658607535);
        assert_eq!(mix64(1), 10451216379200822465);
        assert_eq!(mix64(AI_SALT_XOR), 7346102462620707947);
    }

    /// 把「`mapgen::splitmix64` ≠ 本模块 `mix64`」这条易错点钉死。
    ///
    /// `mapgen::splitmix64` 是 `Rng::next_u64` 内部的**输出混淆**（不加 `GAMMA`，所以
    /// `splitmix64(0) == 0`）；本模块的 `mix64` 才是 §10 要的一次性哈希（先加 `GAMMA`）。
    /// 如果将来有人「简化」成直接调 `mapgen::splitmix64`，种子会全体改变，这个测试会立刻报错。
    #[test]
    fn mapgen_splitmix64_is_not_the_one_shot_hash() {
        assert_eq!(splitmix64(0), 0, "输出混淆不含 += GAMMA");
        assert_ne!(splitmix64(0), mix64(0));
        assert_ne!(splitmix64(7), mix64(7));
    }

    #[test]
    fn ai_seed_formula_matches_rules_doc() {
        // 地图种子 12345、队伍 0：12345 ^ 0x9E3779B9 = 0x9E374980
        assert_eq!(ai_seed_from_salt(12345, 0), 0x9E37_4980);
        assert_eq!(ai_seed_from_salt(12345, 1), 0x1_9E37_4980);
        assert_eq!(ai_seed_from_salt(12345, 2), 0x2_9E37_4980);
    }

    #[test]
    fn per_match_with_base_seed_is_reproducible_and_maps_differ() {
        let a = SeedPlan::generate(SeedMode::PerMatch, 8, 2, Some(20240501), None, 999).unwrap();
        let b = SeedPlan::generate(SeedMode::PerMatch, 8, 2, Some(20240501), None, 111).unwrap();
        // entropy 不同但 base_seed 相同 → 结果必须完全一致。
        assert_eq!(a.map_seeds, b.map_seeds);
        assert_eq!(a.ai_seeds, b.ai_seeds);
        // per-match：每局地图不同（8 局随机撞种子的概率可忽略，此处是契约检查）。
        let mut sorted = a.map_seeds.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 8, "per-match 每局都应是新地图种子");
        // AI 种子 = 地图种子 ^ (team<<32) ^ 0x9E3779B9
        assert_eq!(a.ai_seed(3, 1), ai_seed_from_salt(a.map_seed(3), 1));
    }

    #[test]
    fn per_match_without_base_seed_uses_entropy() {
        let a = SeedPlan::generate(SeedMode::PerMatch, 4, 2, None, None, 1).unwrap();
        let b = SeedPlan::generate(SeedMode::PerMatch, 4, 2, None, None, 2).unwrap();
        assert_ne!(a.map_seeds, b.map_seeds, "无 base-seed 时熵必须真的参与");
    }

    #[test]
    fn fixed_random_reuses_one_map_but_varies_ai_salt() {
        let plan =
            SeedPlan::generate(SeedMode::FixedRandom, 6, 2, Some(20240501), None, 0).unwrap();
        assert!(plan.map_seeds.iter().all(|s| *s == plan.map_seeds[0]));
        // 每局 AI 盐不同 → 每局第 0 队的 AI 种子不同（否则 6 局结果会完全一样）。
        let team0: Vec<u64> = (0..6).map(|i| plan.ai_seed(i, 0)).collect();
        let mut unique = team0.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 6, "AI 盐必须按局索引变化：{team0:?}");
        // 同 base-seed 重跑一致，且与 per-match 的推导方式不同。
        let again =
            SeedPlan::generate(SeedMode::FixedRandom, 6, 2, Some(20240501), None, 12345).unwrap();
        assert_eq!(plan.map_seeds, again.map_seeds);
        assert_eq!(plan.ai_seeds, again.ai_seeds);

        // 逐字复算 §10 的公式，防止实现漂移。
        let map_seed = plan.map_seeds[0];
        for i in 0..6usize {
            let salt = mix64(map_seed ^ (i as u64) ^ AI_SALT_XOR);
            assert_eq!(plan.ai_seed(i, 0), ai_seed_from_salt(salt, 0));
            assert_eq!(plan.ai_seed(i, 1), ai_seed_from_salt(salt, 1));
        }
    }

    #[test]
    fn fixed_mode_requires_seed_and_ignores_base_seed() {
        let plan = SeedPlan::generate(SeedMode::Fixed, 3, 3, Some(777), Some(12345), 0).unwrap();
        assert!(plan.map_seeds.iter().all(|s| *s == 12345));
        assert_eq!(plan.map_seeds.len(), 3);
        assert_eq!(plan.ai_seeds.len(), 3);
        assert!(plan.ai_seeds.iter().all(|row| row.len() == 3));
        // 用另一个（本该无用的）base_seed 重跑，结果必须一样：fixed 不看 base-seed。
        let again = SeedPlan::generate(SeedMode::Fixed, 3, 3, None, Some(12345), 0).unwrap();
        assert_eq!(plan.map_seeds, again.map_seeds);
        assert_eq!(plan.ai_seeds, again.ai_seeds);

        // 缺少 --seed 必须报错，而不是悄悄用 0。
        let err = SeedPlan::generate(SeedMode::Fixed, 3, 3, None, None, 0).unwrap_err();
        assert!(matches!(err, CliError::MissingFixedSeed), "{err}");
    }

    #[test]
    fn plan_info_samples_at_most_available() {
        let plan = SeedPlan::generate(SeedMode::PerMatch, 3, 2, Some(1), None, 0).unwrap();
        let info = plan.info(5);
        assert_eq!(info.total_matches, 3);
        assert_eq!(info.map_seed_sample.len(), 3);
        assert_eq!(info.ai_seed_sample.len(), 3);
        assert_eq!(info.ai_seed_sample[0].len(), 2);
        assert_eq!(info.mode, "per-match");
        assert!(plan.map_seed(99) == 0 && plan.ai_seed(99, 9) == 0);
        assert!(!plan.is_empty() && plan.len() == 3);
    }

    #[test]
    fn mode_strings_and_flags_agree_with_cli_values() {
        assert_eq!(SeedMode::PerMatch.as_str(), "per-match");
        assert_eq!(SeedMode::FixedRandom.as_str(), "fixed-random");
        assert_eq!(SeedMode::Fixed.as_str(), "fixed");
        assert!(SeedMode::Fixed.uses_fixed_seed());
        assert!(!SeedMode::PerMatch.uses_fixed_seed());
        assert!(SeedMode::PerMatch.uses_base_seed());
        assert!(!SeedMode::Fixed.uses_base_seed());
    }
}
