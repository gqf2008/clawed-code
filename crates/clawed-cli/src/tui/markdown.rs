//! Markdown-to-ratatui renderer.
//!
//! Converts a markdown string into `Vec<Line<'static>>` using `pulldown-cmark`
//! for parsing and `syntect` for code block syntax highlighting.

use super::MUTED;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, CodeBlockKind};
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
#[derive(Clone, Copy)]
#[derive(Default)]
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
            style = style
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD);
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
            style = style.fg(Color::Yellow);
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

    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(text, options);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut state = StyleState::default();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_block_buf = String::new();
    let mut list_item_started = false;
    let mut ordered_list_index: Option<u64> = None;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    state.heading_level = level as u8;
                    let prefix = "#".repeat(level as usize);
                    current_spans.push(Span::styled(
                        format!("{prefix} "),
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ));
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
                Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {}
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    state.heading_level = 0;
                    flush_line(&mut lines, &mut current_spans);
                    lines.push(Line::from(""));
                }
                TagEnd::Paragraph => {
                    flush_line(&mut lines, &mut current_spans);
                    lines.push(Line::from(""));
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
                        lines.push(Line::from(""));
                    }
                }
                TagEnd::Item => {
                    // Tight lists (no blank lines) don't wrap in Paragraph,
                    // so flush here to ensure each item gets its own line.
                    flush_line(&mut lines, &mut current_spans);
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
                TagEnd::Table | TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell => {}
                _ => {}
            },
            Event::Text(cow_text) => {
                let txt = cow_text.to_string();
                if in_code_block {
                    code_block_buf.push_str(&txt);
                    continue;
                }

                // Handle blockquote prefix
                let bq_prefix = if state.blockquote_depth > 0 && current_spans.is_empty() {
                    let bars = "\u{2502} ".repeat(state.blockquote_depth as usize);
                    Some(Span::styled(bars, Style::default().fg(MUTED)))
                } else {
                    None
                };

                // Handle list item bullet/number
                let list_prefix = if list_item_started {
                    list_item_started = false;
                    let indent = "  ".repeat((state.list_depth.saturating_sub(1)) as usize);
                    let bullet = if let Some(ref mut idx) = ordered_list_index {
                        let s = format!("{indent}{idx}. ");
                        *idx += 1;
                        s
                    } else {
                        format!("{indent}\u{2022} ")
                    };
                    Some(Span::styled(bullet, Style::default().fg(Color::Blue)))
                } else {
                    None
                };

                if let Some(prefix) = bq_prefix {
                    current_spans.push(prefix);
                }
                if let Some(prefix) = list_prefix {
                    current_spans.push(prefix);
                }

                // Split text on newlines
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
                    format!("`{code_text}`"),
                    Style::default().fg(Color::Yellow),
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
                lines.push(Line::styled(
                    "\u{2500}".repeat(40),
                    Style::default().fg(MUTED),
                ));
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
    while lines.last().is_some_and(|l| l.spans.is_empty() || (l.spans.len() == 1 && l.spans[0].content.is_empty())) {
        lines.pop();
    }

    lines
}

/// Flush current_spans into a Line and push to lines.
fn flush_line(lines: &mut Vec<Line<'static>>, spans: &mut Vec<Span<'static>>) {
    if spans.is_empty() {
        lines.push(Line::from(""));
    } else {
        lines.push(Line::from(std::mem::take(spans)));
    }
}

/// Render a code block with syntax highlighting via syntect.
fn render_code_block(lang: &str, code: &str, lines: &mut Vec<Line<'static>>) {
    let res = SyntaxResources::get();

    // Language header
    let lang_display = if lang.is_empty() { "text" } else { lang };
    lines.push(Line::from(vec![
        Span::styled(
            format!("\u{250C}\u{2500}\u{2500} {lang_display} "),
            Style::default().fg(MUTED),
        ),
        Span::styled(
            "\u{2500}".repeat(20),
            Style::default().fg(MUTED),
        ),
    ]));

    // Try to highlight with syntect
    let syntax = if !lang.is_empty() {
        res.syntax_set.find_syntax_by_token(lang)
    } else {
        None
    };

    let code_trimmed = code.trim_end_matches('\n');

    if let Some(syntax) = syntax {
        let theme = &res.theme_set.themes["base16-ocean.dark"];
        let mut highlighter = HighlightLines::new(syntax, theme);

        for src_line in code_trimmed.split('\n') {
            let line_with_nl = format!("{src_line}\n");
            if let Ok(ranges) = highlighter.highlight_line(&line_with_nl, &res.syntax_set) {
                let spans: Vec<Span<'static>> = ranges
                    .into_iter()
                    .map(|(hl_style, text)| {
                        let fg = Color::Rgb(
                            hl_style.foreground.r,
                            hl_style.foreground.g,
                            hl_style.foreground.b,
                        );
                        Span::styled(
                            format!("\u{2502} {}", text.trim_end_matches('\n')),
                            Style::default().fg(fg),
                        )
                    })
                    .collect();
                if spans.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("\u{2502} {src_line}"),
                        Style::default().fg(Color::White),
                    )));
                } else {
                    lines.push(Line::from(spans));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    format!("\u{2502} {src_line}"),
                    Style::default().fg(Color::White),
                )));
            }
        }
    } else {
        // No syntax found — plain monospace style
        let code_style = Style::default().fg(Color::White);
        for src_line in code_trimmed.split('\n') {
            lines.push(Line::from(Span::styled(
                format!("\u{2502} {src_line}"),
                code_style,
            )));
        }
    }

    // Bottom border
    lines.push(Line::styled(
        format!("\u{2514}{}", "\u{2500}".repeat(30)),
        Style::default().fg(MUTED),
    ));
}

/// Quick heuristic: does this text look like it contains markdown?
fn likely_markdown(text: &str) -> bool {
    // Check first 2048 chars for common markdown markers
    let sample = if text.len() > 2048 { &text[..2048] } else { text };
    sample.contains("**")
        || sample.contains('`')
        || sample.contains("```")
        || sample.contains("## ")
        || sample.contains("# ")
        || sample.contains("- ")
        || sample.contains("* ")
        || sample.contains("> ")
        || sample.contains("1. ")
        || sample.contains("[")
        || sample.contains("---")
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
        let code_span = lines[0].spans.iter().find(|s| s.content.contains("foo"));
        assert!(code_span.is_some());
        assert_eq!(code_span.unwrap().style.fg, Some(Color::Yellow));
    }

    #[test]
    fn heading() {
        let lines = render_markdown("## Title\n\nBody text");
        assert!(!lines.is_empty());
        // First non-empty line should be cyan/bold heading
        let heading = &lines[0];
        assert!(heading.spans[0].content.contains('#'));
    }

    #[test]
    fn code_block() {
        let md = "```rust\nfn main() {}\n```";
        let lines = render_markdown(md);
        // Should have: header line, code line, footer line
        assert!(lines.len() >= 3);
        // Header should mention "rust"
        let header = &lines[0];
        assert!(header.spans.iter().any(|s| s.content.contains("rust")));
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
    }

    #[test]
    fn horizontal_rule() {
        let md = "above\n\n---\n\nbelow";
        let lines = render_markdown(md);
        let rule_line = lines.iter().find(|l| {
            l.spans.iter().any(|s| s.content.contains('\u{2500}'))
        });
        assert!(rule_line.is_some());
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
}
