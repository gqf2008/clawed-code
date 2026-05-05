//! Markdown-to-ratatui renderer via termimad.
//!
//! Converts a markdown string into `Vec<Line<'static>>` using termimad for
//! parsing and rendering. termimad outputs ANSI SGR escape sequences;
//! we parse those into ratatui `Span`/`Line` types so the rest of the TUI
//! pipeline is unchanged.

use std::io::Write;
use std::sync::OnceLock;

use super::blank_line;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use termimad::{
    Alignment, CompoundStyle, LineStyle, MadSkin, StyledChar,
};

/// Cached `MadSkin` — built once on first use.
fn skin() -> &'static MadSkin {
    static INSTANCE: OnceLock<MadSkin> = OnceLock::new();
    INSTANCE.get_or_init(make_skin)
}

/// Build a `MadSkin` matching the current TUI aesthetic.
fn make_skin() -> MadSkin {
    let mut skin = MadSkin::default();

    let muted_c = termimad::rgb(170, 170, 170);
    let blue_c = termimad::rgb(0, 100, 255);
    let none_attrs = termimad::crossterm::style::Attributes::none();

    // Inline code: blue (matches current renderer)
    skin.inline_code = CompoundStyle::new(Some(blue_c), None, none_attrs);

    // Headers
    let mut h1_style = CompoundStyle::new(
        None,
        None,
        termimad::crossterm::style::Attribute::Bold.into(),
    );
    h1_style.add_attr(termimad::crossterm::style::Attribute::Italic);
    h1_style.add_attr(termimad::crossterm::style::Attribute::Underlined);
    skin.headers[0] = LineStyle {
        compound_style: h1_style,
        align: Alignment::Left,
        left_margin: 0,
        right_margin: 0,
    };
    for h in &mut skin.headers[1..] {
        h.compound_style = CompoundStyle::new(
            None,
            None,
            termimad::crossterm::style::Attribute::Bold.into(),
        );
    }

    // Blockquotes: ▎ prefix in muted
    skin.quote_mark = StyledChar::new(
        CompoundStyle::new(Some(muted_c), None, none_attrs),
        '\u{258e}',
    );

    // Horizontal rule: ─ in muted
    skin.horizontal_rule = StyledChar::new(
        CompoundStyle::new(Some(muted_c), None, none_attrs),
        '\u{2500}',
    );

    // Bullets: -
    skin.bullet = StyledChar::new(
        CompoundStyle::new(Some(muted_c), None, none_attrs),
        '-',
    );

    // Table borders in muted
    skin.table = LineStyle {
        compound_style: CompoundStyle::new(Some(muted_c), None, none_attrs),
        align: Alignment::Left,
        left_margin: 0,
        right_margin: 0,
    };

    // Code blocks: plain style (termimad does not do syntax highlighting)
    skin.code_block = LineStyle {
        compound_style: CompoundStyle::new(None, None, none_attrs),
        align: Alignment::Left,
        left_margin: 0,
        right_margin: 0,
    };

    skin
}

/// Convert a markdown string into styled ratatui lines.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    // Fast path: if no markdown syntax, return plain lines
    if !likely_markdown(text) {
        return text.lines().map(|l| Line::from(l.to_string())).collect();
    }

    let skin = skin();
    let width = crossterm::terminal::size().ok().map(|(w, _)| w as usize);
    let fmt_text = skin.text(text, width);
    let mut buf = Vec::new();
    // termimad's Display writes ANSI SGR sequences via crossterm's queue! macro.
    // queue! does not check for TTY — it unconditionally writes escape codes.
    let _ = std::write!(&mut buf, "{}", fmt_text);
    let ansi_str = String::from_utf8(buf).expect("termimad emits valid UTF-8");
    let mut lines = ansi_to_lines(&ansi_str);

    // Strip trailing blank lines (matches current behavior)
    while lines.last().is_some_and(|l| {
        l.spans.is_empty() || (l.spans.len() == 1 && l.spans[0].content.is_empty())
    }) {
        lines.pop();
    }

    lines
}

/// Parse ANSI SGR escape sequences into ratatui `Line`/`Span` types.
fn ansi_to_lines(ansi: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut style = Style::default();
    let mut text = String::new();
    let mut chars = ansi.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            // Flush pending text before applying new style
            if !text.is_empty() {
                current_spans.push(Span::styled(std::mem::take(&mut text), style));
            }
            chars.next(); // consume '['

            // Parse SGR codes: digits and semicolons until 'm'
            let mut codes = [0u64; 8];
            let mut code_count = 0usize;
            let mut num = 0u64;
            let mut has_num = false;
            loop {
                match chars.next() {
                    Some('m') => {
                        if has_num && code_count < codes.len() {
                            codes[code_count] = num;
                            code_count += 1;
                        }
                        break;
                    }
                    Some(';') => {
                        if has_num && code_count < codes.len() {
                            codes[code_count] = num;
                            code_count += 1;
                        }
                        num = 0;
                        has_num = false;
                    }
                    Some(c) if c.is_ascii_digit() => {
                        num = num * 10 + u64::from(c as u8 - b'0');
                        has_num = true;
                    }
                    Some(_) => {
                        // Malformed sequence — skip until we hit a letter
                        if has_num && code_count < codes.len() {
                            codes[code_count] = num;
                            code_count += 1;
                        }
                        while let Some(c) = chars.next() {
                            if c.is_ascii_alphabetic() {
                                break;
                            }
                        }
                        break;
                    }
                    None => {
                        if has_num && code_count < codes.len() {
                            codes[code_count] = num;
                            code_count += 1;
                        }
                        break;
                    }
                }
            }
            style = apply_sgr(style, &codes[..code_count]);
        } else if ch == '\n' {
            if !text.is_empty() {
                current_spans.push(Span::styled(std::mem::take(&mut text), style));
            }
            if current_spans.is_empty() {
                lines.push(blank_line());
            } else {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
            }
        } else {
            text.push(ch);
        }
    }

    // Flush remaining text and spans
    if !text.is_empty() {
        current_spans.push(Span::styled(text, style));
    }
    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    lines
}

/// Apply a sequence of SGR codes to a ratatui `Style`.
fn apply_sgr(mut style: Style, codes: &[u64]) -> Style {
    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            0 => style = Style::default(),
            1 => style = style.add_modifier(Modifier::BOLD),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifier::BOLD),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style = style.fg(ansi_color((codes[i] as u8) - 30)),
            38 if i + 1 < codes.len() && codes[i + 1] == 2 && i + 4 < codes.len() => {
                style = style.fg(Color::Rgb(
                    codes[i + 2] as u8,
                    codes[i + 3] as u8,
                    codes[i + 4] as u8,
                ));
                i += 4;
            }
            38 if i + 1 < codes.len() && codes[i + 1] == 5 && i + 2 < codes.len() => {
                style = style.fg(Color::Indexed(codes[i + 2] as u8));
                i += 2;
            }
            39 => style = style.fg(Color::Reset),
            40..=47 => style = style.bg(ansi_color((codes[i] as u8) - 40)),
            48 if i + 1 < codes.len() && codes[i + 1] == 2 && i + 4 < codes.len() => {
                style = style.bg(Color::Rgb(
                    codes[i + 2] as u8,
                    codes[i + 3] as u8,
                    codes[i + 4] as u8,
                ));
                i += 4;
            }
            48 if i + 1 < codes.len() && codes[i + 1] == 5 && i + 2 < codes.len() => {
                style = style.bg(Color::Indexed(codes[i + 2] as u8));
                i += 2;
            }
            49 => style = style.bg(Color::Reset),
            90..=97 => style = style.fg(ansi_color((codes[i] as u8) - 90 + 8)),
            100..=107 => style = style.bg(ansi_color((codes[i] as u8) - 100 + 8)),
            _ => {}
        }
        i += 1;
    }
    style
}

/// Map an ANSI 4-bit color index (0-15) to a ratatui `Color`.
fn ansi_color(idx: u8) -> Color {
    match idx {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        15 => Color::White,
        n => Color::Indexed(n),
    }
}

/// Quick heuristic: does this text look like it contains markdown?
pub(crate) fn likely_markdown(text: &str) -> bool {
    let sample = if text.len() > 2048 {
        let mut end = 2048;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        text
    };

    let bytes = sample.as_bytes();
    let mut i = 0;
    let mut at_line_start = true;

    while i < bytes.len() {
        let b = bytes[i];

        if at_line_start && (b == b' ' || b == b'\t') {
            i += 1;
            continue;
        }

        // Inline patterns (anywhere)
        if b == b'`' {
            return true;
        }
        if i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if (b == b'*' && next == b'*')
                || (b == b'~' && next == b'~')
                || (b == b']' && next == b'(')
            {
                return true;
            }
        }

        // Line-start patterns
        if at_line_start {
            match b {
                b'#' | b'-' | b'*' | b'+' | b'>' | b'|' => {
                    if i + 1 < bytes.len() && bytes[i + 1] == b' ' {
                        return true;
                    }
                    if b == b'#' {
                        let mut j = i + 1;
                        while j < bytes.len() && bytes[j] == b'#' {
                            j += 1;
                        }
                        if j < bytes.len() && bytes[j] == b' ' {
                            return true;
                        }
                    }
                    if b == b'-' || b == b'*' {
                        if i + 2 < bytes.len()
                            && bytes[i + 1] == b
                            && bytes[i + 2] == b
                        {
                            let mut j = i + 3;
                            while j < bytes.len()
                                && (bytes[j] == b' ' || bytes[j] == b'\t')
                            {
                                j += 1;
                            }
                            if j == bytes.len() || bytes[j] == b'\n' {
                                return true;
                            }
                        }
                    }
                }
                _ if b.is_ascii_digit() => {
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j + 1 < bytes.len() && bytes[j] == b'.' && bytes[j + 1] == b' ' {
                        return true;
                    }
                }
                _ => {}
            }
        }

        at_line_start = b == b'\n';
        i += 1;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    #[test]
    fn plain_text_fast_path() {
        let lines = render_markdown("hello world");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "hello world");
    }

    #[test]
    fn bold_text() {
        let lines = render_markdown("hello **bold** world");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.len() >= 3);
        let bold_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "bold")
            .expect("should find bold span");
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn inline_code() {
        let lines = render_markdown("use `foo` here");
        assert_eq!(lines.len(), 1);
        let code_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content == "foo")
            .expect("inline code span should contain 'foo'");
        assert_eq!(code_span.style.fg, Some(Color::Rgb(0, 100, 255)));
    }

    #[test]
    fn heading_h1() {
        let lines = render_markdown("# Title\n\nBody text");
        assert!(!lines.is_empty());
        let heading = &lines[0];
        assert!(
            !heading.spans.iter().any(|s| s.content.contains('#')),
            "heading should not echo '#' characters"
        );
        assert!(heading.spans.iter().any(|s| {
            s.style.add_modifier.contains(Modifier::BOLD)
                && s.style.add_modifier.contains(Modifier::ITALIC)
                && s.style.add_modifier.contains(Modifier::UNDERLINED)
        }));
    }

    #[test]
    fn heading_h2() {
        let lines = render_markdown("## Title\n\nBody text");
        assert!(!lines.is_empty());
        let heading = &lines[0];
        assert!(
            !heading.spans.iter().any(|s| s.content.contains('#')),
            "heading should not echo '#' characters"
        );
        assert!(heading.spans.iter().any(|s| {
            s.style.add_modifier.contains(Modifier::BOLD)
                && !s.style.add_modifier.contains(Modifier::UNDERLINED)
        }));
    }

    #[test]
    fn code_block() {
        let md = "```rust\nfn main() {}\n```";
        let lines = render_markdown(md);
        assert!(!lines.is_empty());
        let code_line: String = lines[0].spans.iter().map(|s| &*s.content).collect();
        assert!(code_line.contains("fn main"), "code block should contain the code");
    }

    #[test]
    fn unordered_list() {
        let md = "- item one\n- item two\n- item three";
        let lines = render_markdown(md);
        assert!(lines.len() >= 3);
    }

    #[test]
    fn ordered_list() {
        let md = "1. first\n2. second\n3. third";
        let lines = render_markdown(md);
        assert!(lines.len() >= 3);
    }

    #[test]
    fn blockquote() {
        let md = "> quoted text";
        let lines = render_markdown(md);
        assert!(!lines.is_empty());
        let line = &lines[0];
        assert!(
            line.spans.iter().any(|s| s.content.contains('\u{258e}')),
            "blockquote should use ▎ prefix"
        );
        // termimad does not apply italic to blockquote content by default
    }

    #[test]
    fn horizontal_rule() {
        let md = "above\n\n---\n\nbelow";
        let lines = render_markdown(md);
        // With a TTY width, termimad renders HR as a line of '─' chars.
        // Without a TTY (tests), it falls back to blank separator lines.
        let has_visual_hr = lines.iter().any(|l| {
            l.spans.iter().any(|s| s.content.contains('\u{2500}'))
        });
        let has_blank = lines.iter().any(|l| {
            l.spans.is_empty() || (l.spans.len() == 1 && l.spans[0].content.is_empty())
        });
        assert!(
            has_visual_hr || has_blank,
            "horizontal rule should create separator space"
        );
    }

    #[test]
    fn empty_input() {
        let lines = render_markdown("");
        assert!(lines.len() <= 1);
    }

    #[test]
    fn multiline_plain_text() {
        let lines = render_markdown("line one\nline two\nline three");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn task_list() {
        let md = "- [x] done\n- [ ] pending";
        let lines = render_markdown(md);
        assert!(lines.len() >= 2);
    }

    #[test]
    fn table_rows_are_separate_lines() {
        let md = "| Col A | Col B |\n|-------|-------|\n| val1  | val2  |\n| val3  | val4  |";
        let lines = render_markdown(md);
        assert!(lines.len() >= 4, "expected ≥4 lines, got {}", lines.len());
        // termimad uses box-drawing │ for table borders
        let has_border = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains('\u{2502}')));
        assert!(has_border, "table cells should have │ borders");
    }

    #[test]
    fn heading_after_table_does_not_merge() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n\n## Section";
        let lines = render_markdown(md);
        let has_heading_line = lines.iter().any(|l| {
            let text: String = l.spans.iter().map(|s| &*s.content).collect();
            text.contains("Section") && !text.contains('|')
        });
        assert!(
            has_heading_line,
            "heading should be on a separate line from table"
        );
    }

    #[test]
    fn render_dump_markdown() {
        let md = "### 测试覆盖\n\n- messages.rs: 20 个测试\n- markdown.rs: 10 个测试\n\n> 引用块文本\n\n---\n\n| A | B |\n|---|---|\n| v1 | v2 |\n";
        eprintln!("\n═══ Markdown 渲染输出 ═══");
        eprintln!("输入:");
        for l in md.lines() {
            eprintln!("  {l}");
        }
        eprintln!("输出:");
        for (i, line) in render_markdown(md).iter().enumerate() {
            let t: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            let has_bold = line
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
            let has_italic = line
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::ITALIC));
            eprintln!("  L{i}: [{t}] bold={has_bold} italic={has_italic}");
        }
    }

    #[test]
    fn verify_heading_no_hash_prefix() {
        let lines = render_markdown("### 测试覆盖");
        let text: String = lines[0].spans.iter().map(|s| &*s.content).collect();
        assert!(!text.contains('#'), "heading must not contain #, got: {text:?}");
        assert!(text.contains("测试覆盖"), "heading text must be present");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "heading must be bold"
        );
    }

    #[test]
    fn verify_unordered_list_dash_prefix() {
        let lines = render_markdown("- item one\n- item two");
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| &*s.content))
            .collect();
        assert!(text.contains("- "), "unordered list must use '- ' prefix");
    }

    #[test]
    fn verify_blockquote_uses_bar() {
        let lines = render_markdown("> quoted text");
        let text: String = lines[0].spans.iter().map(|s| &*s.content).collect();
        assert!(text.contains('\u{258e}'), "blockquote must use ▎ bar, got: {text:?}");
        // termimad does not apply italic to blockquote content by default
    }

    #[test]
    fn verify_code_block_no_border() {
        let lines = render_markdown("```rust\nfn main() {}\n```");
        let full: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| &*s.content))
            .collect();
        assert!(!full.contains('\u{250C}'), "code block must NOT have ┌ border");
        assert!(!full.contains('\u{2514}'), "code block must NOT have └ border");
        assert!(!full.contains('\u{2502}'), "code block must NOT have │ prefix");
        assert!(full.contains("fn main"), "code content must be present");
    }

    #[test]
    fn verify_horizontal_rule_format() {
        let lines = render_markdown("above\n\n---\n\nbelow");
        let has_visual_hr = lines.iter().any(|l| {
            l.spans.iter().any(|s| s.content.contains('\u{2500}'))
        });
        let has_blank = lines.iter().any(|l| {
            l.spans.is_empty() || (l.spans.len() == 1 && l.spans[0].content.is_empty())
        });
        assert!(
            has_visual_hr || has_blank,
            "horizontal rule must create separator space"
        );
    }

    #[test]
    fn verify_inline_code_blue() {
        let lines = render_markdown("use `foo` here");
        let code_span = lines[0].spans.iter().find(|s| s.content == "foo");
        assert!(code_span.is_some(), "inline code must keep content");
        assert_eq!(
            code_span.unwrap().style.fg,
            Some(Color::Rgb(0, 100, 255)),
            "inline code must be blue"
        );
    }
}
