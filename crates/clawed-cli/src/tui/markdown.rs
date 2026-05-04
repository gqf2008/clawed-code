//! Markdown-to-ratatui renderer.
//!
//! Converts a markdown string into `Vec<Line<'static>>` using `pulldown-cmark`
//! for parsing and `syntect` for code block syntax highlighting.

use super::{blank_line, muted};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// Lazy-initialized syntax highlighting resources.
struct SyntaxResources {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl SyntaxResources {
    fn get() -> &'static Self {
        use std::sync::OnceLock;
        static INSTANCE: OnceLock<SyntaxResources> = OnceLock::new();
        INSTANCE.get_or_init(|| SyntaxResources {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        })
    }
}

/// Style stack entry for nested markdown formatting.
#[derive(Clone, Copy, Default)]
struct StyleState {
    bold: bool,
    italic: bool,
    strikethrough: bool,
    code_inline: bool,
    heading_level: u8,
    blockquote_depth: u8,
    list_depth: u8,
}

impl StyleState {
    fn to_style(self) -> Style {
        let mut style = Style::default();

        if self.heading_level > 0 {
            style = style.add_modifier(Modifier::BOLD);
            if self.heading_level == 1 {
                style = style.add_modifier(Modifier::ITALIC);
                style = style.add_modifier(Modifier::UNDERLINED);
            }
        }

        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strikethrough {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.code_inline {
            style = style.fg(Color::Blue);
        }
        if self.blockquote_depth > 0 {
            style = style.add_modifier(Modifier::ITALIC);
        }

        style
    }
}

/// Convert a markdown string into styled ratatui lines.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    // Fast path: if no markdown syntax, return plain lines
    if !likely_markdown(text) {
        return text.lines().map(|l| Line::from(l.to_string())).collect();
    }

    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(text, options);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut state = StyleState::default();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_block_buf = String::new();
    let mut list_item_started = false;
    let mut ordered_list_index: Option<u64> = None;
    // Table tracking: column index within the current row (reset per row).
    let mut table_col_idx: usize = 0;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    // Flush any pending content (e.g. from a preceding table that
                    // never emitted its own flush) before starting a new heading.
                    if !current_spans.is_empty() {
                        flush_line(&mut lines, &mut current_spans);
                    }
                    state.heading_level = level as u8;
                }
                Tag::Paragraph => {}
                Tag::BlockQuote(_) => {
                    state.blockquote_depth += 1;
                }
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    code_block_buf.clear();
                    code_lang = match kind {
                        CodeBlockKind::Fenced(lang) => lang.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                }
                Tag::List(start) => {
                    ordered_list_index = start;
                    state.list_depth += 1;
                }
                Tag::Item => {
                    list_item_started = true;
                }
                Tag::Emphasis => {
                    state.italic = true;
                }
                Tag::Strong => {
                    state.bold = true;
                }
                Tag::Strikethrough => {
                    state.strikethrough = true;
                }
                Tag::Link { dest_url, .. } => {
                    // We'll handle the link text inside, then append URL after
                    state.bold = true;
                    let _ = dest_url; // URL appended on TagEnd
                }
                Tag::Table(_) => {}
                Tag::TableHead | Tag::TableRow => {
                    // Reset column counter for each new row.
                    table_col_idx = 0;
                }
                Tag::TableCell => {
                    // Add a column separator before every cell except the first.
                    if table_col_idx > 0 {
                        current_spans.push(Span::styled(" │ ", muted()));
                    }
                    table_col_idx += 1;
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    state.heading_level = 0;
                    flush_line(&mut lines, &mut current_spans);
                    lines.push(blank_line());
                }
                TagEnd::Paragraph => {
                    flush_line(&mut lines, &mut current_spans);
                    lines.push(blank_line());
                }
                TagEnd::BlockQuote(_) => {
                    state.blockquote_depth = state.blockquote_depth.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    render_code_block(&code_lang, &code_block_buf, &mut lines);
                    code_block_buf.clear();
                    code_lang.clear();
                }
                TagEnd::List(_) => {
                    state.list_depth = state.list_depth.saturating_sub(1);
                    ordered_list_index = None;
                    if state.list_depth == 0 {
                        lines.push(blank_line());
                    }
                }
                TagEnd::Item => {
                    // Flush any remaining spans for tight-list items.
                    // Loose-list items are already flushed by End(Paragraph);
                    // only flush here when there are pending spans to avoid a
                    // double blank line.
                    if !current_spans.is_empty() {
                        flush_line(&mut lines, &mut current_spans);
                    }
                }
                TagEnd::Emphasis => {
                    state.italic = false;
                }
                TagEnd::Strong => {
                    state.bold = false;
                }
                TagEnd::Strikethrough => {
                    state.strikethrough = false;
                }
                TagEnd::Link => {
                    state.bold = false;
                }
                // Table rows: flush each row as its own line.
                // Table head end: add a separator line after flushing the header row.
                // Table end: add a blank line after the table.
                TagEnd::TableRow => {
                    flush_line(&mut lines, &mut current_spans);
                }
                TagEnd::TableHead => {
                    // Header cells are direct children of TableHead (no TableRow wrapper),
                    // so we flush the accumulated header spans here.
                    flush_line(&mut lines, &mut current_spans);
                    // Add a visual separator between header and data rows.
                    lines.push(Line::styled("---", muted()));
                }
                TagEnd::Table => {
                    // Flush any remaining content (data rows are flushed by TagEnd::TableRow).
                    if !current_spans.is_empty() {
                        flush_line(&mut lines, &mut current_spans);
                    }
                    lines.push(blank_line());
                }
                TagEnd::TableCell => {}
                _ => {}
            },
            Event::Text(cow_text) => {
                let txt = cow_text.to_string();
                if in_code_block {
                    code_block_buf.push_str(&txt);
                    continue;
                }

                let bq_prefix = if state.blockquote_depth > 0 && current_spans.is_empty() {
                    let bars = "\u{258e} ".repeat(state.blockquote_depth as usize);
                    Some(Span::styled(bars, muted()))
                } else {
                    None
                };

                let list_prefix = if list_item_started {
                    list_item_started = false;
                    let indent = "  ".repeat((state.list_depth.saturating_sub(1)) as usize);
                    let bullet = if let Some(ref mut idx) = ordered_list_index {
                        let num_str = match state.list_depth {
                            3 => number_to_roman(*idx),
                            2 => number_to_letter(*idx),
                            _ => idx.to_string(),
                        };
                        let s = format!("{indent}{num_str}. ");
                        *idx += 1;
                        s
                    } else {
                        format!("{indent}- ")
                    };
                    Some(Span::raw(bullet))
                } else {
                    None
                };

                if let Some(prefix) = bq_prefix {
                    current_spans.push(prefix);
                }
                if let Some(prefix) = list_prefix {
                    current_spans.push(prefix);
                }

                let style = state.to_style();
                let text_lines: Vec<&str> = txt.split('\n').collect();
                for (i, tl) in text_lines.iter().enumerate() {
                    if i > 0 {
                        flush_line(&mut lines, &mut current_spans);
                    }
                    if !tl.is_empty() {
                        current_spans.push(Span::styled((*tl).to_string(), style));
                    }
                }
            }
            Event::Code(code_text) => {
                current_spans.push(Span::styled(
                    code_text.to_string(),
                    Style::default().fg(Color::Blue),
                ));
            }
            Event::SoftBreak => {
                current_spans.push(Span::raw(" "));
            }
            Event::HardBreak => {
                flush_line(&mut lines, &mut current_spans);
            }
            Event::Rule => {
                flush_line(&mut lines, &mut current_spans);
                lines.push(Line::styled("---", muted()));
            }
            Event::TaskListMarker(checked) => {
                let marker = if checked { "\u{2611} " } else { "\u{2610} " };
                let indent = "  ".repeat((state.list_depth.saturating_sub(1)) as usize);
                current_spans.push(Span::styled(
                    format!("{indent}{marker}"),
                    Style::default().fg(Color::Green),
                ));
                list_item_started = false;
            }
            _ => {}
        }
    }

    // Flush remaining spans
    flush_line(&mut lines, &mut current_spans);

    // Remove trailing empty lines
    while lines.last().is_some_and(|l| {
        l.spans.is_empty() || (l.spans.len() == 1 && l.spans[0].content.is_empty())
    }) {
        lines.pop();
    }

    lines
}

/// Flush current_spans into a Line and push to lines.
fn flush_line(lines: &mut Vec<Line<'static>>, spans: &mut Vec<Span<'static>>) {
    if spans.is_empty() {
        lines.push(blank_line());
    } else {
        lines.push(Line::from(std::mem::take(spans)));
    }
}

/// Render a code block with syntax highlighting via syntect.
/// Aligned with official CC: no borders, just highlighted code.
fn render_code_block(lang: &str, code: &str, lines: &mut Vec<Line<'static>>) {
    let res = SyntaxResources::get();

    let syntax = if !lang.is_empty() {
        res.syntax_set.find_syntax_by_token(lang)
    } else {
        None
    };

    let code_trimmed = code.trim_end_matches('\n');

    if let Some(syntax) = syntax {
        let theme = &res.theme_set.themes["base16-ocean.dark"];
        let mut highlighter = HighlightLines::new(syntax, theme);
        let mut line_buf = String::new();

        for src_line in code_trimmed.split('\n') {
            line_buf.clear();
            line_buf.push_str(src_line);
            line_buf.push('\n');
            if let Ok(ranges) = highlighter.highlight_line(&line_buf, &res.syntax_set) {
                let line_spans: Vec<Span<'static>> = ranges
                    .into_iter()
                    .filter_map(|(hl_style, text)| {
                        let t = text.trim_end_matches('\n');
                        if t.is_empty() {
                            return None;
                        }
                        let fg = Color::Rgb(
                            hl_style.foreground.r,
                            hl_style.foreground.g,
                            hl_style.foreground.b,
                        );
                        Some(Span::styled(t.to_string(), Style::default().fg(fg)))
                    })
                    .collect();
                if line_spans.is_empty() {
                    lines.push(blank_line());
                } else {
                    lines.push(Line::from(line_spans));
                }
            } else {
                lines.push(Line::from(src_line.to_string()));
            }
        }
    } else {
        // No syntax found — plain text
        for src_line in code_trimmed.split('\n') {
            lines.push(Line::from(src_line.to_string()));
        }
    }
}

/// Convert a number to a letter (1→a, 2→b, ..., 26→z, 27→aa, ...).
/// Aligned with official CC ordered-list depth-2 numbering.
fn number_to_letter(mut n: u64) -> String {
    let mut result = String::new();
    while n > 0 {
        n -= 1;
        result.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    result.chars().rev().collect()
}

const ROMAN_VALUES: &[(u64, &str)] = &[
    (1000, "m"),
    (900, "cm"),
    (500, "d"),
    (400, "cd"),
    (100, "c"),
    (90, "xc"),
    (50, "l"),
    (40, "xl"),
    (10, "x"),
    (9, "ix"),
    (5, "v"),
    (4, "iv"),
    (1, "i"),
];

/// Convert a number to lowercase roman numerals (1→i, 2→ii, 3→iii, ...).
/// Aligned with official CC ordered-list depth-3 numbering.
fn number_to_roman(mut n: u64) -> String {
    let mut result = String::new();
    for &(value, numeral) in ROMAN_VALUES {
        while n >= value {
            result.push_str(numeral);
            n -= value;
        }
    }
    result
}

/// Quick heuristic: does this text look like it contains markdown?
pub(crate) fn likely_markdown(text: &str) -> bool {
    // Check first ~2048 bytes for common markdown markers.
    // Must find a valid char boundary to avoid panicking on multi-byte characters.
    let sample = if text.len() > 2048 {
        let mut end = 2048;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        text
    };

    // Unambiguous inline markers (appear anywhere in text)
    if sample.contains("**")
        || sample.contains('`')
        || sample.contains("~~")
        || sample.contains("](")
    // link [text](url) — not bare [
    {
        return true;
    }

    // Block-level markers that only count at the start of a line
    sample.lines().any(|line| {
        let t = line.trim_start();
        t.starts_with("# ")
            || t.starts_with("## ")
            || t.starts_with("### ")
            || t.starts_with("#### ")
            || t.starts_with("- ")
            || t.starts_with("* ")
            || t.starts_with("+ ")
            || t.starts_with("> ")
            || t.starts_with("| ")
            || t == "---"
            || t == "***"
            // Ordered list: one or more digits followed by ". "
            || t.split_once(". ").is_some_and(|(pre, _)| {
                !pre.is_empty() && pre.chars().all(char::is_numeric)
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_fast_path() {
        let lines = render_markdown("hello world");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "hello world");
    }

    #[test]
    fn bold_text() {
        let lines = render_markdown("hello **bold** world");
        // Should produce one line with multiple spans
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.len() >= 3);
        // The bold span should have BOLD modifier
        let bold_span = &lines[0].spans[1];
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(bold_span.content, "bold");
    }

    #[test]
    fn inline_code() {
        let lines = render_markdown("use `foo` here");
        assert_eq!(lines.len(), 1);
        let code_span = lines[0].spans.iter().find(|s| s.content == "foo");
        assert!(code_span.is_some(), "inline code span should contain 'foo' without backticks");
        assert_eq!(code_span.unwrap().style.fg, Some(Color::Blue));
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
        // No borders: just the code line(s)
        assert!(!lines.is_empty());
        let code_line: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
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
        // Official CC uses ▎ (U+258E) as blockquote prefix
        assert!(
            line.spans.iter().any(|s| s.content.contains('\u{258e}')),
            "blockquote should use ▎ prefix"
        );
        // Content should be italic
        assert!(
            line.spans.iter().any(|s| {
                s.style.add_modifier.contains(Modifier::ITALIC) && s.content.contains("quoted")
            }),
            "blockquote content should be italic"
        );
    }

    #[test]
    fn horizontal_rule() {
        let md = "above\n\n---\n\nbelow";
        let lines = render_markdown(md);
        let rule_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content == "---"));
        assert!(rule_line.is_some(), "horizontal rule should render as '---'");
    }

    #[test]
    fn empty_input() {
        let lines = render_markdown("");
        // Empty or single empty line
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
        // Header row + data rows should each be on their own line.
        // At minimum: header (1), separator line (1), 2 data rows (2) = 4 lines.
        assert!(lines.len() >= 4, "expected ≥4 lines, got {}", lines.len());
        // The cell separator │ must appear in at least one line.
        let has_pipe = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains('│')));
        assert!(has_pipe, "table cells should be separated by │");
    }

    #[test]
    fn heading_after_table_does_not_merge() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n\n## Section";
        let lines = render_markdown(md);
        // The heading should be on its own line, not merged with table content.
        let has_heading_line = lines.iter().any(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            text.contains("Section") && !text.contains("│")
        });
        assert!(
            has_heading_line,
            "heading should be on a separate line from table"
        );
    }

    // ── Rendering verification ──

    #[test]
    fn render_dump_markdown() {
        let md = "### 测试覆盖\n\n- messages.rs: 20 个测试\n- markdown.rs: 10 个测试\n\n> 引用块文本\n\n---\n\n| A | B |\n|---|---|\n| v1 | v2 |\n";
        eprintln!("\n═══ Markdown 渲染输出 ═══");
        eprintln!("输入:");
        for l in md.lines() { eprintln!("  {l}"); }
        eprintln!("输出:");
        for (i, line) in render_markdown(md).iter().enumerate() {
            let t: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            let has_bold = line.spans.iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD));
            let has_italic = line.spans.iter().any(|s| s.style.add_modifier.contains(Modifier::ITALIC));
            eprintln!("  L{i}: [{t}] bold={has_bold} italic={has_italic}");
        }
    }

    #[test]
    fn verify_heading_no_hash_prefix() {
        let lines = render_markdown("### 测试覆盖");
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains('#'), "heading must not contain #, got: {text:?}");
        assert!(text.contains("测试覆盖"), "heading text must be present");
        assert!(lines[0].spans.iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "heading must be bold");
    }

    #[test]
    fn verify_unordered_list_dash_prefix() {
        let lines = render_markdown("- item one\n- item two");
        let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
        assert!(text.contains("- "), "unordered list must use '- ' prefix");
        assert!(!text.contains("\u{2022}"), "unordered list must NOT use • bullet");
    }

    #[test]
    fn verify_blockquote_uses_bar() {
        let lines = render_markdown("> quoted text");
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("\u{258e}"), "blockquote must use ▎ bar, got: {text:?}");
        assert!(lines[0].spans.iter().any(|s| s.style.add_modifier.contains(Modifier::ITALIC)),
            "blockquote content must be italic");
    }

    #[test]
    fn verify_code_block_no_border() {
        let lines = render_markdown("```rust\nfn main() {}\n```");
        let full: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
        assert!(!full.contains("\u{250C}"), "code block must NOT have ┌ border");
        assert!(!full.contains("\u{2514}"), "code block must NOT have └ border");
        assert!(!full.contains("\u{2502}"), "code block must NOT have │ prefix");
        assert!(full.contains("fn main"), "code content must be present");
    }

    #[test]
    fn verify_horizontal_rule_format() {
        let lines = render_markdown("above\n\n---\n\nbelow");
        let rule_line = lines.iter().find(|l| {
            let t: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            t == "---"
        });
        assert!(rule_line.is_some(), "horizontal rule must be '---'");
    }

    #[test]
    fn verify_inline_code_blue() {
        let lines = render_markdown("use `foo` here");
        let code_span = lines[0].spans.iter().find(|s| s.content == "foo");
        assert!(code_span.is_some(), "inline code must keep content");
        assert_eq!(code_span.unwrap().style.fg, Some(Color::Blue), "inline code must be blue");
    }
}
