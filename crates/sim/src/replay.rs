//! 回放打包与 JSONL 序列化（格式契约见 `docs/replay-format.md`）。
//!
//! ## 三行结构
//!
//! 回放是「一行一个 JSON 对象」的文本（JSONL）：
//!
//! ```text
//! {"type":"init",  ...}         ← 第 1 行：地图、队伍、规则参数（ReplayInit）
//! {"type":"frame", ...}         ← 从 tick=1 起，每 tick 一行，tick 连续递增
//! ...
//! {"type":"end",   ...}         ← 最后一行：结果（MatchResult）
//! ```
//!
//! 这里**不做**任何字段加工：序列化直接交给 `protocol::ReplayLine` 的 serde 实现，
//! 保证「回放文件」与「协议类型」永远同源 —— 如果哪天协议改了字段，
//! 校验脚本和本函数会一起变化，不会出现「引擎手写字符串字段名」的漂移。
//!
//! ## 为什么每行末尾都补 `\n`
//!
//! JSONL 的惯例（也是 `tools/validate_replay.py` 的读法）是「每行一条记录」，
//! 因此**包括最后一行**也要有换行符；这样 `cat`/`tail -n +2` 之类的工具行为可预期，
//! 也让「同配置两次运行逐字节相同」的测试覆盖到行尾。

use protocol::{MatchResult, RenderFrame, ReplayInit, ReplayLine};

/// 一局完整回放：`init` 行 + 全部帧 + `end` 行。
///
/// 这是内存表示；写盘由 `cli` 负责（见 crate 文档的依赖方向：sim 不碰文件系统）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayBundle {
    /// 第一行：初始化信息（引擎/规则/地图版本、地图、队伍、规则参数、种子）。
    pub init: ReplayInit,
    /// 中间行：从 tick=1 开始的逐 tick 渲染帧。
    pub frames: Vec<RenderFrame>,
    /// 最后一行：对局结果。
    pub end: MatchResult,
}

impl ReplayBundle {
    /// 序列化为 JSONL 文本（每行一条，末尾带 `\n`）。
    ///
    /// `serde_json::Error` 只在理论上可能（比如浮点 NaN）——本回放里没有浮点字段，
    /// 因此实际不会失败；保留 `Result` 是为了不让调用方以为「永不失败」而忽略错误。
    pub fn to_jsonl(&self) -> Result<String, serde_json::Error> {
        // 预算容量：每帧几十到几百字节，给个下界避免反复扩容（纯优化，不影响结果）。
        let mut out = String::with_capacity(1024 * 64);
        write_line(&mut out, &ReplayLine::Init(self.init.clone()))?;
        for frame in &self.frames {
            write_line(&mut out, &ReplayLine::Frame(frame.clone()))?;
        }
        write_line(&mut out, &ReplayLine::End(self.end.clone()))?;
        Ok(out)
    }

    /// 帧数（等价于「已结算的 tick 数」，供调用方与 `end.ticks` 交叉核对）。
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

/// 写一行：序列化 + 换行。
fn write_line(out: &mut String, line: &ReplayLine) -> Result<(), serde_json::Error> {
    out.push_str(&serde_json::to_string(line)?);
    out.push('\n');
    Ok(())
}
