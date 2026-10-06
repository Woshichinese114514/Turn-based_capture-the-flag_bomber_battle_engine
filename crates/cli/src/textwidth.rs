//! 终端表格对齐：按**显示宽度**（东亚宽字符算 2 列）而不是 `str::len()`（字节）或
//! `chars().count()`（字符数）来补空格。
//!
//! # 为什么需要这个小模块
//!
//! CLI 的汇总表用的是「人类可读的列对齐」，而表格里同时有 `greedy_flag`（ASCII）和
//! `局数`/`胜率`/`评分`（中日韩宽字符）两类文本。Rust 的 `{:<16}` 补的是 **Unicode 标量
//! 个数**，不是终端列数：一个汉字在等宽终端里占 2 列，于是「局数」只有 2 个字符却占 4 列，
//! 按字符数补齐后整张表会越往右越歪（用户反馈的现象就是中文表头挤在一起、数字列错位）。
//!
//! 这里只做「宽度」这一件事，判定规则按 Unicode East Asian Width 的常见实现简化：
//! 宽/全角（W、F）算 2，组合附加符号与零宽字符算 0，其余算 1。之所以不引入 `unicode-width`
//! 依赖：本 crate 只要覆盖「表头 CJK + ASCII 数字」这一种场景，几十行代码就够，而且
//! 依赖越少越不容易因为版本变化改变输出（`docs/internal-api.md` 也要求 CLI 尽量无额外依赖）。
//!
//! # 依赖方向
//!
//! 纯字符串处理，不依赖本 crate 其它模块，也不依赖任何游戏逻辑——因此放在 `cli` 的库层
//! 而不是 `main.rs`，这样表格对齐可以用普通单测钉死（`main.rs` 里的代码只能靠子进程测）。

/// 对齐方式：左对齐（文本列）或右对齐（数字列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// 左对齐：内容贴左，右侧补空格。
    Left,
    /// 右对齐：内容贴右，左侧补空格（数字列用，便于按位比较）。
    Right,
}

/// 单个字符在等宽终端里占用的列数。
///
/// 判据是 Unicode East Asian Width：`W`（Wide）与 `F`（Fullwidth）算 2 列，
/// 组合记号/零宽控制符算 0 列，其余算 1 列。范围按常见实现列举，覆盖：
/// 韩文字母与音节、CJK 部首与汉字、日文假名与标点、全角 ASCII 变体、CJK 扩展区（含
/// U+20000 以上的补充平面，用 `u32` 比较以免 `char` 范围写法啰嗦）。
///
/// 注意 `char` 的码位上限是 U+10FFFF，所以 `0x3FFFD` 这个上界是安全的。
pub fn char_width(c: char) -> usize {
    let code = c as u32;
    match code {
        // 控制字符（C0/C1）与「退格」之类：终端不占位，算 0，避免把不可见字符
        // 计入列宽导致后面整列右移。
        0x0000..=0x001F | 0x007F..=0x009F => 0,
        // 组合附加符号（重音等）叠加在前一个字符上，不独立占列。
        0x0300..=0x036F => 0,
        // 零宽字符：零宽空格、零宽连字符、字节序标记、方向控制符。
        0x200B..=0x200F | 0xFEFF => 0,
        // 韩文字母（Jamo）与音节：U+1100..U+115F、U+AC00..U+D7A3。
        0x1100..=0x115F | 0xAC00..=0xD7A3 => 2,
        // CJK 部首、假名、注音、CJK 统一表意文字（U+4E00..U+9FFF 在下面的范围里）、
        // 韩文兼容字母、彝文等一大段连续宽字符。
        0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF => 2,
        // 彝文音节、CJK 兼容表意文字、竖排标点、小写假名扩展。
        0xA000..=0xA4CF | 0xF900..=0xFAFF | 0xFE10..=0xFE19 | 0xFE30..=0xFE6F => 2,
        // 全角 ASCII 变体与全角符号：U+FF00..U+FF60、U+FFE0..U+FFE6。
        0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 => 2,
        // CJK 扩展 B 及以后（U+20000 起），以及兼容表意文字补充。
        0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// 字符串在等宽终端里的显示宽度（列数）。
pub fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// 左侧放 `text`，右侧补空格到 `width` 列；若 `text` 本身已超过 `width` 则原样返回
/// （**不截断**：宁可把表格撑宽，也不要把 AI 名字或数字截掉看不全）。
pub fn pad_right(text: &str, width: usize) -> String {
    let current = display_width(text);
    if current >= width {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + (width - current));
    out.push_str(text);
    for _ in 0..(width - current) {
        out.push(' ');
    }
    out
}

/// 右侧放 `text`，左侧补空格到 `width` 列；超宽时同样原样返回。
pub fn pad_left(text: &str, width: usize) -> String {
    let current = display_width(text);
    if current >= width {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + (width - current));
    for _ in 0..(width - current) {
        out.push(' ');
    }
    out.push_str(text);
    out
}

/// 把一张表渲染成若干行字符串（不含行尾换行），列与列之间用一个空格分隔。
///
/// * `headers`：表头文本，长度与 `aligns` 一致；
/// * `aligns`：每列的对齐方式；
/// * `rows`：数据行，允许比 `headers` 短（缺的单元格按空串处理），也允许更长
///   （多出来的列会被忽略——这与「表格列数固定」的预期一致，且不会 panic）。
///
/// 列宽取「表头与该列所有单元格」的最大显示宽度：先扫一遍确定宽度，再逐行补齐，
/// 这样即便某列出现超长文本，后续列仍然对齐（只是整体右移），符合终端表格的直觉。
pub fn render_table(headers: &[&str], aligns: &[Align], rows: &[Vec<String>]) -> Vec<String> {
    let columns = headers.len().min(aligns.len());
    let mut widths: Vec<usize> = headers
        .iter()
        .take(columns)
        .map(|header| display_width(header))
        .collect();
    for row in rows {
        for (index, cell) in row.iter().take(columns).enumerate() {
            let cell_width = display_width(cell);
            if cell_width > widths[index] {
                widths[index] = cell_width;
            }
        }
    }

    let render_line = |cells: Vec<String>| -> String {
        let mut parts = Vec::with_capacity(columns);
        for (index, cell) in cells.into_iter().take(columns).enumerate() {
            parts.push(match aligns[index] {
                Align::Left => pad_right(&cell, widths[index]),
                Align::Right => pad_left(&cell, widths[index]),
            });
        }
        // 右对齐列左侧的空格已经补在 cell 里；这里统一用单空格作列分隔，
        // 因此相邻两列之间至少能看到 1 个空格，中文密集的表头也不会糊在一起。
        parts.join(" ").trim_end().to_string()
    };

    let mut lines = Vec::with_capacity(rows.len() + 1);
    lines.push(render_line(
        headers.iter().map(|header| header.to_string()).collect(),
    ));
    for row in rows {
        lines.push(render_line(row.clone()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ASCII 就是「一字符一列」，用来说明最朴素的基线。
    #[test]
    fn ascii_width_equals_char_count() {
        assert_eq!(display_width("greedy_flag"), 11);
        assert_eq!(display_width(""), 0);
    }

    /// 中文表头（用户反馈的错位场景）：`局数` 是 2 个字符但占 4 列。
    #[test]
    fn cjk_characters_take_two_columns() {
        assert_eq!(display_width("局数"), 4);
        assert_eq!(display_width("评分"), 4);
        // 全角标点/字母也算 2 列（U+FF21 是全角 'A'）。
        assert_eq!(display_width("Ａ"), 2);
        // 组合重音不占额外列：'e' + U+0301。
        assert_eq!(display_width("e\u{0301}"), 1);
    }

    /// 补空格按显示宽度算：4 列宽的中文不需要再补；2 列的中文要补 2 个空格。
    #[test]
    fn padding_uses_display_width_not_char_count() {
        assert_eq!(pad_right("局数", 4), "局数");
        assert_eq!(pad_right("胜", 4), "胜  ");
        assert_eq!(pad_right("胜率", 4), "胜率");
        assert_eq!(pad_left("12", 4), "  12");
        // 已经超宽时不截断，原样返回（保证数字/名字完整）。
        assert_eq!(pad_right("greedy_flag", 4), "greedy_flag");
        assert_eq!(pad_left("局数", 2), "局数");
    }

    /// 关键回归：同一张表里每一行的**显示宽度必须相等**。
    ///
    /// 这条断言直接就锁死了用户报的「中文列没对齐」问题——只需把中英文混排塞进同一张表，
    /// 任何按字节或按字符数补齐的实现都会让两行的 `display_width` 不相等。
    #[test]
    fn rendered_table_has_equal_display_width_per_line() {
        let headers = ["AI", "局数", "胜", "平", "负", "胜率", "评分"];
        let aligns = [
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ];
        let rows = vec![
            vec![
                "greedy_flag".to_string(),
                "2000".into(),
                "883".into(),
                "234".into(),
                "883".into(),
                "0.442".into(),
                "969.5".into(),
            ],
            vec![
                "random".to_string(),
                "2000".into(),
                "0".into(),
                "0".into(),
                "2000".into(),
                "0.000".into(),
                "0.0".into(),
            ],
        ];
        let lines = render_table(&headers, &aligns, &rows);
        assert_eq!(lines.len(), 3);
        let widths: Vec<usize> = lines.iter().map(|line| display_width(line)).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "各行显示宽度不一致：{widths:?}\n{}",
            lines.join("\n")
        );
        // 数字必须右对齐到同一列：两行的「评分」都以 `969.5` / `  0.0` 结尾。
        assert!(lines[1].ends_with("969.5"), "{}", lines[1]);
        assert!(lines[2].ends_with("  0.0"), "{}", lines[2]);
    }

    /// 数据行缺列 / 列数不一致时不能 panic，也不能把多出来的列画出去。
    #[test]
    fn short_and_long_rows_are_handled_without_panic() {
        let lines = render_table(
            &["A", "B"],
            &[Align::Left, Align::Right],
            &[vec!["x".into()], vec!["y".into(), "1".into(), "多余".into()]],
        );
        assert_eq!(lines.len(), 3);
        assert!(lines[1].starts_with('x'));
        // 第三列被忽略：行里不应出现「多余」。
        assert!(!lines[2].contains("多余"));
    }
}
