//! # mapgen —— 带种子、带版本号的确定性地图生成
//!
//! 本 crate 只做两件事：**给定种子产出地图**，以及**校验这颗种子产出的地图是可玩的**。
//! 它不包含任何游戏规则判定（那是 `sim` 的事），也不读文件（那是 `cli` 的事）。
//!
//! ## 为什么地图生成必须版本化
//!
//! 「种子 → 地图」是本项目的复现基石：回放里只存 `seed + map_gen_version`，Web UI 与
//! 统计脚本都靠这两个值重建地图。如果生成算法被原地修改，**同一颗种子会产出另一张图**，
//! 历史回放就会「看起来没坏、实际全错」。因此：
//!
//! * 算法定型后不原地修改，要改就加 `generate_v2` 并升 [`MAP_GEN_VERSION`]；
//! * 每次生成都在 `MapData::map_gen_version` 与回放 `init` 行里带上版本号；
//! * 不支持的版本必须返回 `Err`（[`MapGenError::UnsupportedVersion`]），不能猜。
//!
//! ## 生成算法 v1 的意图（为什么这样生成）
//!
//! 1. **随机撒墙/虚空**：先把整张图当空地，再按密度撒 `Wall` / `Void`。
//!    墙提供战术掩体（挡视线、挡爆炸），虚空是**可以走进去、但进去就死**的深渊
//!    （`rules_version >= 2`，见 `docs/rules.md` §1/§9）：它不挡视线也不挡爆炸，
//!    只惩罚「图省事直线穿场」的单位。两者叠加让同一片区域在不同种子里有不同价值。
//!
//!    ⚠️ 生成器**不保证**虚空两侧可绕行：连通性校验用的是「安全可站立」的格子
//!    （`is_walkable`，不含虚空），因此虚空不会被当成通路；同时也不排除「某块空地
//!    只能穿过虚空到达」的布局——这种布局在规则上是「有代价的捷径」，由 AI 自己判断。
//! 2. **刻出阵营区**：阵营区必须是完整的 3×3 `TeamBase`，不能被随机墙破坏，
//!    否则出生/复活/得分区就废了。所以撒完再覆盖。
//! 3. **保证中心区可站人**：旗只在中心区域刷新，若中心区域全是墙/虚空，
//!    整局就没有旗可抢。
//! 4. **连通性校验**：每个阵营区到中心区域必须有一条可走通路，否则那支队伍
//!    可能整局走不到中心。不通过就用**同一个 RNG 继续抽**
//!    （换种子会破坏「种子 → 地图」契约；重新播种会丢掉已消耗的随机流）。
//! 5. **兜底修路**：连续若干次都抽不到连通图时，用确定性的 L 形走廊把阵营中心和
//!    地图中心连起来。这一步只把墙/虚空改成空地，不覆盖阵营格，因此一定能连通。
//!    有了兜底，`generate` 对合法输入几乎不会失败，`Err` 只留给真正非法的规格。

mod rng;

pub use rng::{splitmix64, Rng};

use protocol::{Coord, MapInit, TeamId, Terrain, MAX_TEAMS};
use serde::{Deserialize, Serialize};

/// 阵营区边长（3×3）。
///
/// 与 `protocol::MapInit::base_contains` 里写死的 3 必须一致（协议注释已说明为什么
/// 不把它做成可配置项）。这里导出成常量，让 `sim` 不用再抄一个魔法数字。
pub const BASE_SIZE: i32 = 3;

/// 当前地图生成版本。改动生成算法必须 +1，并同步 `docs/replay-format.md`。
pub const MAP_GEN_VERSION: u32 = 1;

/// 连通性重试的上限次数。
///
/// 取 32：单次尝试的成本是 O(w·h)，32 次对 25×25 也只是几万次操作；
/// 而「随机撒 22% 墙 + 7% 虚空后仍不连通」的概率极低（通常在 1e-3 量级以下），
/// 32 次之后仍失败说明规格本身很极端（例如中心半径 0 且中心恰好被墙封死），
/// 这时交给确定性兜底修路比无限重试更合适。
const MAX_CONNECT_ATTEMPTS: usize = 32;

/// 单张地图允许的最大格数（`width * height`）。
///
/// **为什么需要一个上界**：`MapSpec` 的宽高是 `u16`，调用方完全可以传
/// `65535 × 65535`。若不拦，`scatter_terrain` 里的 `(w * h) as usize` 会先以 `i32`
/// 相乘溢出（debug 构建直接 panic，release 环绕成无意义的容量并尝试分配几 GB），
/// 这不是「配置错误」应表现出的行为。取 `1 << 22`（约 419 万格，等价 2048×2048）：
/// 远超本游戏需要的 25×25 / 21×21，同时保证格数运算与内存占用都在合理范围。
const MAX_CELLS: usize = 1 << 22;

/// 该地图生成版本是否被当前引擎支持。
///
/// CLI 在 `--map-gen-version` 传入别的值时会据此明确报错，
/// 而不是拿当前算法硬算出一张与回放不符的图。
pub fn is_supported_version(version: u32) -> bool {
    version == MAP_GEN_VERSION
}

/// 地图生成参数。
///
/// 默认值即项目默认对局规格（25×25、2 队、墙 22%、虚空 7%、中心半径 4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapSpec {
    /// 地图种子；**唯一**的随机性来源。
    pub seed: u64,
    /// 地图宽度（列数）。
    pub width: u16,
    /// 地图高度（行数）。
    pub height: u16,
    /// 队伍数：只支持 2 或 3（4 队留给协议编码空间，规则未定义）。
    pub teams: u8,
    /// 中心区域半径（到中心曼哈顿距离 ≤ 它的格子才可能刷旗）。
    pub center_radius: u8,
    /// 墙密度百分比。
    pub wall_density_percent: u8,
    /// 虚空密度百分比。
    pub void_density_percent: u8,
    /// 生成算法版本（写成字段是为了让序化的规格自带版本信息）。
    pub map_gen_version: u32,
}

impl Default for MapSpec {
    fn default() -> Self {
        Self {
            seed: 0,
            width: 25,
            height: 25,
            teams: 2,
            center_radius: 4,
            wall_density_percent: 22,
            void_density_percent: 7,
            map_gen_version: MAP_GEN_VERSION,
        }
    }
}

/// 生成结果：静态地图数据。
///
/// `bases` **存左上角**而不是中心：UI 直接画 `x..x+3, y..y+3` 的矩形，
/// 回放 `init.teams[].base_x/base_y` 也是左上角，三处口径统一，避免 ±1 换算错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapData {
    /// 地图宽度（列数），与 `terrain` 的行主序换算 `idx = y * width + x` 一致。
    pub width: u16,
    /// 地图高度（行数）。
    pub height: u16,
    /// 生成本图的算法版本；升级生成算法时会 +1，回放解析方据此决定是否兼容。
    pub map_gen_version: u32,
    /// 行优先地形数组：`idx = y * width + x`。
    pub terrain: Vec<Terrain>,
    /// 每队 3×3 阵营区**左上角**，下标即队伍 ID。
    pub bases: Vec<Coord>,
    /// 地图中心格（`width/2, height/2` 向下取整）。
    pub center: Coord,
    /// 中心区域半径（回放 `init.center_radius` 的值，UI 据此画虚线圈）。
    pub center_radius: u8,
}

/// 地图生成错误。
///
/// 所有消息都必须人类可读：CLI 会把它直接打到 stderr，玩家/AI 开发者靠这句话定位配置错误。
#[derive(Debug, thiserror::Error)]
pub enum MapGenError {
    /// 请求了不支持的生成版本。
    #[error("不支持的地图生成版本 {0}（当前引擎只支持 {expected}）", expected = MAP_GEN_VERSION)]
    UnsupportedVersion(u32),
    /// 队伍数不是 2/3。
    #[error("队伍数必须是 2 或 3，得到 {0}")]
    BadTeamCount(u8),
    /// 密度之和超过 100%（后者会把前者覆盖成虚空，无法表达设计意图）。
    #[error("密度非法：墙 {wall}% + 虚空 {void}% 超过 100%")]
    BadDensity { wall: u8, void: u8 },
    /// 地图太小，放不下全部 3×3 阵营区（或阵营区彼此重叠）。
    #[error("地图尺寸过小：{width}×{height}，{teams} 队至少需要 {min}×{min}")]
    MapTooSmall {
        width: u16,
        height: u16,
        teams: u8,
        min: u16,
    },
    /// 地图格数超过 [`MAX_CELLS`]（防止 `i32` 相乘溢出与无意义的巨额分配）。
    #[error("地图尺寸过大：{width}×{height} = {cells} 格，上限 {max_cells} 格")]
    MapTooLarge {
        width: u16,
        height: u16,
        cells: usize,
        max_cells: usize,
    },
    /// 阵营区布局非法（越界/重叠），通常在尺寸校验之后仍出现时才是 bug。
    #[error("阵营区布局非法：{0}")]
    BaseLayout(String),
    /// 兜底修路之后仍不连通（理论上不可达，保留以便早期暴露实现 bug）。
    #[error("连通性修复失败：{0}")]
    ConnectivityFailed(String),
}

/// 按显式版本号生成地图。
///
/// `version` 是**权威**参数：`spec.map_gen_version` 只作为规格自带的元信息，
/// 不参与分发（避免「规格里写了 v2、调用方传 v1」时产生歧义）。
pub fn generate_versioned(version: u32, spec: &MapSpec) -> Result<MapData, MapGenError> {
    if !is_supported_version(version) {
        return Err(MapGenError::UnsupportedVersion(version));
    }
    generate_v1(spec)
}

/// 按当前版本生成地图（= `generate_versioned(MAP_GEN_VERSION, spec)`）。
pub fn generate(spec: &MapSpec) -> Result<MapData, MapGenError> {
    generate_versioned(MAP_GEN_VERSION, spec)
}

/// v1 生成算法。**定型后不再原地修改**；要改请加 `generate_v2`。
pub fn generate_v1(spec: &MapSpec) -> Result<MapData, MapGenError> {
    validate_spec(spec)?;
    let bases = base_layout(spec.teams, spec.width, spec.height)?;
    let center = Coord::new((spec.width / 2) as i32, (spec.height / 2) as i32);

    // 整次生成只用一个 RNG：重试也用同一条流继续抽，这样「同种子同版本」才成立。
    let mut rng = Rng::new(spec.seed);
    let mut accepted: Option<Vec<Terrain>> = None;
    for _ in 0..MAX_CONNECT_ATTEMPTS {
        let mut terrain = scatter_terrain(&mut rng, spec);
        carve_bases(&mut terrain, spec.width, &bases);
        ensure_center_walkable(
            &mut terrain,
            spec.width,
            spec.height,
            center,
            spec.center_radius,
            &bases,
        );
        if is_connected(
            &terrain,
            spec.width,
            spec.height,
            &bases,
            center,
            spec.center_radius,
        ) {
            accepted = Some(terrain);
            break;
        }
    }

    let terrain = match accepted {
        Some(t) => t,
        None => {
            // 兜底：在最后一次尝试的基础上确定性修路。
            // 只把 Wall/Void 改成 Empty，绝不覆盖 TeamBase，所以不会破坏阵营区。
            let mut t = scatter_terrain(&mut rng, spec);
            carve_bases(&mut t, spec.width, &bases);
            ensure_center_walkable(
                &mut t,
                spec.width,
                spec.height,
                center,
                spec.center_radius,
                &bases,
            );
            for base in &bases {
                carve_path(&mut t, spec.width, spec.height, base_center(*base), center);
            }
            if !is_connected(
                &t,
                spec.width,
                spec.height,
                &bases,
                center,
                spec.center_radius,
            ) {
                return Err(MapGenError::ConnectivityFailed(format!(
                    "{MAX_CONNECT_ATTEMPTS} 次随机生成 + 确定性修路后，仍有阵营区无法到达中心"
                )));
            }
            t
        }
    };

    Ok(MapData {
        width: spec.width,
        height: spec.height,
        map_gen_version: MAP_GEN_VERSION,
        terrain,
        bases,
        center,
        center_radius: spec.center_radius,
    })
}

/// 规格校验：尺寸/队伍数/密度。
fn validate_spec(spec: &MapSpec) -> Result<(), MapGenError> {
    if spec.teams < 2 || spec.teams > MAX_TEAMS.min(3) {
        return Err(MapGenError::BadTeamCount(spec.teams));
    }
    if spec.wall_density_percent as u32 + spec.void_density_percent as u32 > 100 {
        return Err(MapGenError::BadDensity {
            wall: spec.wall_density_percent,
            void: spec.void_density_percent,
        });
    }
    // 8×8 是能放下 3 个互不重叠的 3×3 阵营区的最小尺寸（见 base_layout 的布局约定）。
    let min_side = (2 * BASE_SIZE + 2) as u16;
    if spec.width < min_side || spec.height < min_side {
        return Err(MapGenError::MapTooSmall {
            width: spec.width,
            height: spec.height,
            teams: spec.teams,
            min: min_side,
        });
    }
    // 上界校验必须用 usize 算乘法：`width * height` 在 i32 下会溢出（65535² > i32::MAX），
    // 而这里的整个目的就是「在溢出发生之前拦住它」。放在尺寸下界检查之后，
    // 保证错误信息里报的是调用方真正传进来的宽高。
    let cells = (spec.width as usize) * (spec.height as usize);
    if cells > MAX_CELLS {
        return Err(MapGenError::MapTooLarge {
            width: spec.width,
            height: spec.height,
            cells,
            max_cells: MAX_CELLS,
        });
    }
    Ok(())
}

/// 计算每队 3×3 阵营区的左上角（约定见 `docs/internal-api.md` §2）。
///
/// * 2 队：team0 `(1,1)`，team1 `(w-4, h-4)`
/// * 3 队：team0 `(1,1)`，team1 `(w-4, 1)`，team2 `((w-3)/2, h-4)`
///
/// 三队时把第三队放在下边中点：这样三队到中心的初始曼哈顿距离大致相等，
/// 不会出现「某一队天生离旗更近」的固定优势。
fn base_layout(teams: u8, width: u16, height: u16) -> Result<Vec<Coord>, MapGenError> {
    let w = width as i32;
    let h = height as i32;
    let bases: Vec<Coord> = match teams {
        2 => vec![Coord::new(1, 1), Coord::new(w - 4, h - 4)],
        3 => vec![
            Coord::new(1, 1),
            Coord::new(w - 4, 1),
            Coord::new((w - 3) / 2, h - 4),
        ],
        other => return Err(MapGenError::BadTeamCount(other)),
    };
    for (team, base) in bases.iter().enumerate() {
        if base.x < 0 || base.y < 0 || base.x + BASE_SIZE > w || base.y + BASE_SIZE > h {
            return Err(MapGenError::BaseLayout(format!(
                "{team} 队阵营区左上角 ({},{}) 放不下 3×3（地图 {width}×{height}）",
                base.x, base.y
            )));
        }
    }
    for a in 0..bases.len() {
        for b in (a + 1)..bases.len() {
            if rects_overlap(bases[a], bases[b]) {
                return Err(MapGenError::BaseLayout(format!(
                    "{a} 队与 {b} 队阵营区重叠：{} 与 {}",
                    bases[a], bases[b]
                )));
            }
        }
    }
    Ok(bases)
}

/// 两个 3×3 矩形是否相交（边界接触不算重叠：格子并不共享）。
fn rects_overlap(a: Coord, b: Coord) -> bool {
    let ax2 = a.x + BASE_SIZE - 1;
    let ay2 = a.y + BASE_SIZE - 1;
    let bx2 = b.x + BASE_SIZE - 1;
    let by2 = b.y + BASE_SIZE - 1;
    a.x <= bx2 && b.x <= ax2 && a.y <= by2 && b.y <= ay2
}

/// 阵营区中心格（`左上角 + 1`），修路端点。
fn base_center(base: Coord) -> Coord {
    Coord::new(base.x + BASE_SIZE / 2, base.y + BASE_SIZE / 2)
}

/// 是否落在某个阵营区内。
fn in_any_base_rect(x: i32, y: i32, bases: &[Coord]) -> bool {
    bases
        .iter()
        .any(|b| x >= b.x && x < b.x + BASE_SIZE && y >= b.y && y < b.y + BASE_SIZE)
}

/// 第 1 步：随机撒墙与虚空。
///
/// 每个格子**都**消耗一次随机数（包括之后会被阵营区覆盖的格子）：
/// 让随机流的消耗量与「阵营区在哪」解耦，这样以后即使调整阵营区布局约定，
/// 也不会连带改变地形序列（降低「改一个常量导致所有历史地图变化」的风险）。
fn scatter_terrain(rng: &mut Rng, spec: &MapSpec) -> Vec<Terrain> {
    let w = spec.width as i32;
    let h = spec.height as i32;
    let wall = spec.wall_density_percent as i32;
    let void = spec.void_density_percent as i32;
    let mut terrain = Vec::with_capacity((w * h) as usize);
    for _y in 0..h {
        for _x in 0..w {
            let roll = rng.gen_range_i32(0, 100);
            let t = if roll < wall {
                Terrain::Wall
            } else if roll < wall + void {
                Terrain::Void
            } else {
                Terrain::Empty
            };
            terrain.push(t);
        }
    }
    terrain
}

/// 第 2 步：把每队 3×3 阵营区刻成 `TeamBase(team)`。
///
/// 放在撒地形之后：阵营区必须是完整 9 格，不能被随机墙破坏。
fn carve_bases(terrain: &mut [Terrain], width: u16, bases: &[Coord]) {
    let w = width as i32;
    for (team, base) in bases.iter().enumerate() {
        for dy in 0..BASE_SIZE {
            for dx in 0..BASE_SIZE {
                let idx = ((base.y + dy) * w + (base.x + dx)) as usize;
                terrain[idx] = Terrain::TeamBase(team as TeamId);
            }
        }
    }
}

/// 第 3 步：确保中心区域至少有一个可站人的格子。
///
/// 中心区全是墙/虚空时整局都不会有旗，游戏直接失去意义。这里只做最小干预：
/// 优先把地图中心格改成 `Empty`；若它落在阵营区内（极小地图才可能），
/// 就挑中心区里第一个非阵营格。
fn ensure_center_walkable(
    terrain: &mut [Terrain],
    width: u16,
    height: u16,
    center: Coord,
    radius: u8,
    bases: &[Coord],
) {
    let w = width as i32;
    let h = height as i32;
    let r = radius as i32;

    let mut has_walkable = false;
    'scan: for y in 0..h {
        for x in 0..w {
            if (x - center.x).abs() + (y - center.y).abs() > r {
                continue;
            }
            if terrain[(y * w + x) as usize].is_walkable() {
                has_walkable = true;
                break 'scan;
            }
        }
    }
    if has_walkable {
        return;
    }

    if center.x >= 0
        && center.y >= 0
        && center.x < w
        && center.y < h
        && !in_any_base_rect(center.x, center.y, bases)
    {
        terrain[(center.y * w + center.x) as usize] = Terrain::Empty;
        return;
    }
    for y in 0..h {
        for x in 0..w {
            if (x - center.x).abs() + (y - center.y).abs() > r {
                continue;
            }
            if !in_any_base_rect(x, y, bases) {
                terrain[(y * w + x) as usize] = Terrain::Empty;
                return;
            }
        }
    }
}

/// 第 4 步：BFS 校验「每个阵营区 → 中心区域」连通。
///
/// 从阵营区所有格出发做一次多源 BFS（阵营区内部 9 格当然互通），
/// 只要触到中心区域内任意一个可走格即算连通。
/// 用 BFS 而不是 DFS：栈深可控（避免大地图上的深递归），实现也更直白。
fn is_connected(
    terrain: &[Terrain],
    width: u16,
    height: u16,
    bases: &[Coord],
    center: Coord,
    radius: u8,
) -> bool {
    let w = width as i32;
    let h = height as i32;
    let r = radius as i32;
    for base in bases {
        let mut seen = vec![false; terrain.len()];
        let mut queue: std::collections::VecDeque<(i32, i32)> = std::collections::VecDeque::new();
        for dy in 0..BASE_SIZE {
            for dx in 0..BASE_SIZE {
                let (x, y) = (base.x + dx, base.y + dy);
                let idx = (y * w + x) as usize;
                if !seen[idx] {
                    seen[idx] = true;
                    queue.push_back((x, y));
                }
            }
        }
        let mut reached = false;
        while let Some((x, y)) = queue.pop_front() {
            if (x - center.x).abs() + (y - center.y).abs() <= r
                && terrain[(y * w + x) as usize].is_walkable()
            {
                reached = true;
                break;
            }
            for (dx, dy) in [(0, -1), (0, 1), (-1, 0), (1, 0)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w || ny >= h {
                    continue;
                }
                let nidx = (ny * w + nx) as usize;
                if seen[nidx] || !terrain[nidx].is_walkable() {
                    continue;
                }
                seen[nidx] = true;
                queue.push_back((nx, ny));
            }
        }
        if !reached {
            return false;
        }
    }
    true
}

/// 兜底修路：把 `from → to` 的一条 L 形走廊上的墙/虚空改成空地。
///
/// 只「挖空」不「填实」：绝不覆盖 `TeamBase`，也绝不把空地改成墙，
/// 因此修路只可能让地图更连通，不会破坏已经算好的任何东西。
/// 先沿 x 走再沿 y 走，顺序固定，保证修路结果确定。
fn carve_path(terrain: &mut [Terrain], width: u16, height: u16, from: Coord, to: Coord) {
    let w = width as i32;
    let h = height as i32;

    let (x0, x1) = if from.x <= to.x {
        (from.x, to.x)
    } else {
        (to.x, from.x)
    };
    for x in x0..=x1 {
        carve_cell(terrain, w, h, x, from.y);
    }
    let (y0, y1) = if from.y <= to.y {
        (from.y, to.y)
    } else {
        (to.y, from.y)
    };
    for y in y0..=y1 {
        carve_cell(terrain, w, h, to.x, y);
    }
}

/// 把单格的墙/虚空改成空地；越界忽略，阵营格与空地保持原样。
fn carve_cell(terrain: &mut [Terrain], w: i32, h: i32, x: i32, y: i32) {
    if x < 0 || y < 0 || x >= w || y >= h {
        return;
    }
    let idx = (y * w + x) as usize;
    if matches!(terrain[idx], Terrain::Wall | Terrain::Void) {
        terrain[idx] = Terrain::Empty;
    }
}

impl MapData {
    /// 转成回放 `init` 行使用的地图结构。
    pub fn to_map_init(&self) -> MapInit {
        MapInit {
            width: self.width,
            height: self.height,
            map_gen_version: self.map_gen_version,
            terrain: self.terrain.clone(),
        }
    }

    /// 行优先下标换算；越界返回 `None`。
    pub fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        Some(y as usize * self.width as usize + x as usize)
    }

    /// 是否在地图内。
    pub fn in_bounds(&self, x: i32, y: i32) -> bool {
        self.index(x, y).is_some()
    }

    /// 取地形；越界或数组异常返回 `None`（不 panic）。
    pub fn terrain_at(&self, x: i32, y: i32) -> Option<Terrain> {
        self.index(x, y).and_then(|i| self.terrain.get(i).copied())
    }

    /// 地形层面是否可站人（空地与任意阵营格）。
    ///
    /// 虚空**不算**：走进去会死（`rules_version = 2` 起的规则）。刷旗/掉旗/复活选点用本函数。
    pub fn is_walkable(&self, x: i32, y: i32) -> bool {
        self.terrain_at(x, y).is_some_and(Terrain::is_walkable)
    }

    /// 移动层面是否可以进入该格（空地与阵营格，**以及虚空**；越界不可进入）。
    ///
    /// 与 [`MapData::is_walkable`] 的区别就是虚空：墙在移动层被拒绝（`illegal_action`、不消耗 AP），
    /// 虚空则允许进入、消耗 AP，随后由 `sim` 的虚空致死步处决。移动合法性判定用本函数。
    pub fn can_enter(&self, x: i32, y: i32) -> bool {
        self.terrain_at(x, y).is_some_and(Terrain::can_be_entered)
    }

    /// 该格是否会杀死踏上去的单位（当前只有虚空）。
    pub fn is_lethal(&self, x: i32, y: i32) -> bool {
        self.terrain_at(x, y).is_some_and(Terrain::is_lethal)
    }

    /// 地形层面是否阻挡视线（越界视为阻挡，避免射线泄漏到地图外）。
    pub fn blocks_sight(&self, x: i32, y: i32) -> bool {
        self.terrain_at(x, y).is_none_or(Terrain::blocks_sight)
    }

    /// 某队阵营区左上角。
    pub fn base_of(&self, team: TeamId) -> Option<Coord> {
        self.bases.get(team as usize).copied()
    }

    /// 某格是否属于指定队伍的阵营区。
    pub fn in_base(&self, team: TeamId, x: i32, y: i32) -> bool {
        self.base_of(team)
            .is_some_and(|b| x >= b.x && x < b.x + BASE_SIZE && y >= b.y && y < b.y + BASE_SIZE)
    }

    /// 某格属于哪个队伍的阵营区（不属于任何阵营返回 `None`）。
    pub fn in_any_base(&self, x: i32, y: i32) -> Option<TeamId> {
        self.bases
            .iter()
            .position(|b| x >= b.x && x < b.x + BASE_SIZE && y >= b.y && y < b.y + BASE_SIZE)
            .map(|i| i as TeamId)
    }

    /// 是否在中心区域内（到中心曼哈顿距离 ≤ `center_radius`，且在地图内）。
    pub fn in_center_region(&self, x: i32, y: i32) -> bool {
        self.in_bounds(x, y)
            && Coord::new(x, y).manhattan(self.center) <= self.center_radius as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(seed: u64, teams: u8) -> MapSpec {
        MapSpec {
            seed,
            teams,
            ..Default::default()
        }
    }

    /// 测试 2 用：独立实现的 BFS 连通性校验。
    /// 刻意不调用 `is_connected`，避免「用被测函数验证被测函数」。
    fn assert_bases_connect_to_center(map: &MapData) {
        for (team, base) in map.bases.iter().enumerate() {
            let mut seen = vec![false; map.terrain.len()];
            let mut queue = std::collections::VecDeque::new();
            for dy in 0..BASE_SIZE {
                for dx in 0..BASE_SIZE {
                    let (x, y) = (base.x + dx, base.y + dy);
                    let idx = map.index(x, y).expect("阵营格必须在地图内");
                    seen[idx] = true;
                    queue.push_back((x, y));
                }
            }
            let mut reached = false;
            while let Some((x, y)) = queue.pop_front() {
                if map.in_center_region(x, y) {
                    reached = true;
                    break;
                }
                for (dx, dy) in [(0, -1), (0, 1), (-1, 0), (1, 0)] {
                    let (nx, ny) = (x + dx, y + dy);
                    if !map.is_walkable(nx, ny) {
                        continue;
                    }
                    let idx = map.index(nx, ny).expect("可走格必在地图内");
                    if seen[idx] {
                        continue;
                    }
                    seen[idx] = true;
                    queue.push_back((nx, ny));
                }
            }
            assert!(reached, "第 {team} 队阵营区无法到达中心区域");
        }
    }

    // ---- 测试 1：同种子同图 / 不同种子大概率不同 ----
    #[test]
    fn same_seed_same_map_and_different_seed_differs() {
        let a = generate(&spec(12345, 2)).expect("生成成功");
        let b = generate(&spec(12345, 2)).expect("生成成功");
        assert_eq!(a.terrain, b.terrain, "同种子必须逐格相同");
        assert_eq!(a.bases, b.bases);
        assert_eq!(a.center, b.center);

        let c = generate(&spec(12346, 2)).expect("生成成功");
        assert_ne!(a.terrain, c.terrain, "不同种子理应产出不同地图");

        assert!(is_supported_version(MAP_GEN_VERSION));
        assert!(!is_supported_version(2));
    }

    // ---- 测试 2：连通性 ----
    #[test]
    fn every_base_reaches_center_for_many_seeds() {
        for seed in 1..=8u64 {
            for teams in [2u8, 3] {
                let map = generate(&spec(seed, teams)).expect("生成成功");
                assert_bases_connect_to_center(&map);
            }
        }
    }

    // ---- 测试 3：阵营区完整、不与墙/虚空重叠；中心区不全是墙 ----
    #[test]
    fn bases_are_intact_and_center_is_not_all_wall() {
        for seed in [7u64, 99, 20240501] {
            for teams in [2u8, 3] {
                let map = generate(&spec(seed, teams)).expect("生成成功");
                for (team, base) in map.bases.iter().enumerate() {
                    for dy in 0..BASE_SIZE {
                        for dx in 0..BASE_SIZE {
                            let t = map
                                .terrain_at(base.x + dx, base.y + dy)
                                .expect("阵营格在地图内");
                            assert_eq!(
                                t,
                                Terrain::TeamBase(team as TeamId),
                                "阵营区必须是完整 3×3 的 TeamBase"
                            );
                        }
                    }
                }
                let mut center_cells = 0usize;
                let mut any_walkable_in_center = false;
                for y in 0..map.height as i32 {
                    for x in 0..map.width as i32 {
                        if !map.in_center_region(x, y) {
                            continue;
                        }
                        center_cells += 1;
                        if !map.blocks_sight(x, y) {
                            any_walkable_in_center = true;
                        }
                    }
                }
                assert!(center_cells > 0, "中心区域必须至少有一格");
                assert!(any_walkable_in_center, "中心区域不能全是墙");
            }
        }
    }

    // ---- 测试 4：非法输入返回 Err（不 panic） ----
    #[test]
    fn invalid_specs_return_err() {
        let bad_teams = MapSpec {
            teams: 1,
            ..spec(1, 2)
        };
        assert!(matches!(
            generate(&bad_teams),
            Err(MapGenError::BadTeamCount(1))
        ));

        let four_teams = MapSpec { teams: 4, ..spec(1, 2) };
        assert!(matches!(
            generate(&four_teams),
            Err(MapGenError::BadTeamCount(4))
        ));

        let tiny = MapSpec {
            width: 5,
            height: 5,
            ..spec(1, 2)
        };
        assert!(matches!(generate(&tiny), Err(MapGenError::MapTooSmall { .. })));

        let dense = MapSpec {
            wall_density_percent: 90,
            void_density_percent: 20,
            ..spec(1, 2)
        };
        assert!(matches!(generate(&dense), Err(MapGenError::BadDensity { .. })));

        assert!(matches!(
            generate_versioned(99, &spec(1, 2)),
            Err(MapGenError::UnsupportedVersion(99))
        ));

        // 超大尺寸必须在分配之前被拦下：65535×65535 若走到 `i32` 相乘会溢出
        // （debug 下 panic、release 下尝试分配数 GB）。这里断言的是「返回 Err」
        // 这一外部可观察行为，不依赖具体是哪种错误分支。
        let huge = MapSpec {
            width: 65535,
            height: 65535,
            ..spec(1, 2)
        };
        assert!(matches!(generate(&huge), Err(MapGenError::MapTooLarge { .. })));

        let min3 = MapSpec {
            width: 8,
            height: 8,
            seed: 3,
            teams: 3,
            ..Default::default()
        };
        assert!(generate(&min3).is_ok(), "8×8 应能放下 3 队阵营区");
    }

    #[test]
    fn extreme_densities_still_produce_a_playable_map() {
        // 95% 墙 + 5% 虚空：随机尝试几乎必然失败，走确定性兜底修路。
        let mut s = spec(4242, 2);
        s.wall_density_percent = 95;
        s.void_density_percent = 5;
        let map = generate(&s).expect("兜底修路必须保证连通");
        assert_bases_connect_to_center(&map);
    }

    #[test]
    fn map_gen_version_is_recorded() {
        let map = generate(&spec(1, 2)).expect("生成成功");
        assert_eq!(map.map_gen_version, MAP_GEN_VERSION);
        assert_eq!(map.to_map_init().map_gen_version, MAP_GEN_VERSION);
    }

    #[test]
    fn two_team_and_three_team_base_layout_matches_contract() {
        let two = generate(&MapSpec {
            width: 25,
            height: 25,
            teams: 2,
            seed: 1,
            ..Default::default()
        })
        .expect("生成成功");
        assert_eq!(two.bases[0], Coord::new(1, 1));
        assert_eq!(two.bases[1], Coord::new(21, 21));
        assert_eq!(two.center, Coord::new(12, 12));

        let three = generate(&MapSpec {
            width: 25,
            height: 25,
            teams: 3,
            seed: 1,
            ..Default::default()
        })
        .expect("生成成功");
        assert_eq!(three.bases[0], Coord::new(1, 1));
        assert_eq!(three.bases[1], Coord::new(21, 1));
        assert_eq!(three.bases[2], Coord::new(11, 21));
    }
}
