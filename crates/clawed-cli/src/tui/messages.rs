use super::{markdown, MUTED};

use std::cell::RefCell;
use std::time::Instant;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Returns an appropriate style for a line in a unified diff.
fn diff_line_style(line: &str) -> Style {
    if line.starts_with("+++") || line.starts_with("---") {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') {
        Style::default().fg(Color::Red)
    } else if line.starts_with("@@") {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default().fg(Color::Gray)
    }
}

/// The content type of a single message in the conversation history.
#[derive(Debug, Clone)]
pub enum MessageContent {
    UserInput(String),
    AssistantText(String),
    ThinkingText(String),
    ToolUseStart { name: String },
    ToolResult {
        name: String,
        preview: String,
        full_result: Option<String>,
        is_error: bool,
        duration_ms: u64,
    },
    TurnDivider {
        turn: u32,
        input_tokens: u64,
        output_tokens: u64,
    },
    System(String),
}

/// A single message with timestamp and line cache.
#[derive(Debug, Clone)]
pub struct Message {
    pub content: MessageContent,
    #[allow(dead_code)] // Reserved for Phase 5 timestamp display
    pub timestamp: Instant,
    /// Whether the tool result is collapsed (true = show preview only).
    pub collapsed: bool,
    /// Cached rendered lines. Invalidated when content changes.
    cached_lines: RefCell<Option<Vec<Line<'static>>>>,
}

impl Message {
    pub fn new(content: MessageContent) -> Self {
        Self {
            content,
            timestamp: Instant::now(),
            collapsed: true,
            cached_lines: RefCell::new(None),
        }
    }

    /// Invalidate the line cache (call after mutating content).
    pub fn invalidate_cache(&self) {
        *self.cached_lines.borrow_mut() = None;
    }

    /// Toggle collapsed state for tool results.
    pub fn toggle_collapsed(&mut self) {
        if matches!(self.content, MessageContent::ToolResult { .. }) {
            self.collapsed = !self.collapsed;
            self.invalidate_cache();
        }
    }

    /// Whether this message is a collapsible tool result (has full_result).
    pub fn is_collapsible(&self) -> bool {
        matches!(
            self.content,
            MessageContent::ToolResult {
                full_result: Some(_),
                ..
            }
        )
    }

    /// Convert this message to ratatui `Line`s for display.
    /// Results are cached; subsequent calls return the cached version.
    pub fn to_lines(&self) -> Vec<Line<'static>> {
        if let Some(ref cached) = *self.cached_lines.borrow() {
            return cached.clone();
        }

        let lines = self.render_lines();
        *self.cached_lines.borrow_mut() = Some(lines.clone());
        lines
    }

    fn render_lines(&self) -> Vec<Line<'static>> {
        match &self.content {
            MessageContent::UserInput(text) => {
                let prefix_style = Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD);
                let text_style = Style::default().add_modifier(Modifier::BOLD);
                let mut lines = vec![Line::from("")];
                for (i, part) in text.split('\n').enumerate() {
                    let prefix = if i == 0 { "> " } else { "  " };
                    lines.push(Line::from(vec![
                        Span::styled(prefix.to_string(), prefix_style),
                        Span::styled(part.to_string(), text_style),
                    ]));
                }
                lines
            }
            MessageContent::AssistantText(text) => {
                if text.is_empty() {
                    return vec![];
                }
                markdown::render_markdown(text)
            }
            MessageContent::ThinkingText(text) => {
                let style = Style::default()
                    .fg(MUTED)
                    .add_modifier(Modifier::ITALIC);
                text.lines()
                    .map(|l| Line::styled(l.to_string(), style))
                    .collect()
            }
            MessageContent::ToolUseStart { name } => vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("\u{2699} ".to_string(), Style::default().fg(Color::Blue)),
                    Span::styled(name.clone(), Style::default().fg(Color::Blue)),
                ]),
            ],
            MessageContent::ToolResult {
                name,
                preview,
                full_result,
                is_error,
                duration_ms,
            } => {
                let (icon, color) = if *is_error {
                    ("\u{2717} ", Color::Red)
                } else {
                    ("\u{2713} ", Color::Green)
                };
                let dur = if *duration_ms >= 1000 {
                    format!("{:.1}s", *duration_ms as f64 / 1000.0)
                } else {
                    format!("{}ms", duration_ms)
                };
                let detail = if *is_error {
                    format!("{name} failed ({dur}): {preview}")
                } else {
                    format!("{name} ({dur}, {} bytes)", preview.len())
                };
                let mut lines = vec![Line::from(vec![
                    Span::styled(icon.to_string(), Style::default().fg(color)),
                    Span::styled(detail, Style::default().fg(color)),
                ])];

                // Show full result when expanded and available
                if !self.collapsed {
                    if let Some(ref full) = full_result {
                        lines.push(Line::from(""));
                        for l in full.lines() {
                            let style = diff_line_style(l);
                            lines.push(Line::styled(format!("  {l}"), style));
                        }
                        lines.push(Line::from(""));
                    }
                } else if full_result.is_some() {
                    // Show collapse hint
                    lines.push(Line::styled(
                        "  \u{25B6} Ctrl+E to expand".to_string(),
                        Style::default().fg(MUTED),
                    ));
                }

                lines
            }
            MessageContent::TurnDivider {
                turn,
                input_tokens,
                output_tokens,
            } => {
                let text = format!("\u{2500}\u{2500} Turn {turn} \u{2502} {input_tokens}\u{2191} {output_tokens}\u{2193} \u{2500}\u{2500}");
                vec![
                    Line::from(""),
                    Line::styled(text, Style::default().fg(Color::Magenta)),
                ]
            }
            MessageContent::System(text) => text
                .lines()
                .map(|l| Line::styled(l.to_string(), Style::default().fg(Color::Yellow)))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_input_has_prompt() {
        let msg = Message::new(MessageContent::UserInput("hello".into()));
        let lines = msg.to_lines();
        assert!(lines.len() >= 2); // blank + prompt line
    }

    #[test]
    fn multiline_user_input_renders_multiple_lines() {
        let msg = Message::new(MessageContent::UserInput("hello\nworld".into()));
        let lines = msg.to_lines();
        assert_eq!(lines.len(), 3);
        let first: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        let second: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(first, "> hello");
        assert_eq!(second, "  world");
    }

    #[test]
    fn empty_assistant_text() {
        let msg = Message::new(MessageContent::AssistantText(String::new()));
        assert!(msg.to_lines().is_empty());
    }

    #[test]
    fn multiline_assistant_text() {
        let msg = Message::new(MessageContent::AssistantText("a\nb\nc".into()));
        assert_eq!(msg.to_lines().len(), 3);
    }

    #[test]
    fn tool_result_error() {
        let msg = Message::new(MessageContent::ToolResult {
            name: "bash".into(),
            preview: "exit 1".into(),
            full_result: None,
            is_error: true,
            duration_ms: 120,
        });
        let lines = msg.to_lines();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn tool_result_collapsed_has_hint() {
        let msg = Message::new(MessageContent::ToolResult {
            name: "read_file".into(),
            preview: "hello world".into(),
            full_result: Some("hello world\nline 2".into()),
            is_error: false,
            duration_ms: 50,
        });
        // Default collapsed = true, should show hint
        let lines = msg.to_lines();
        assert!(lines.len() >= 2);
        let hint_line = &lines[1];
        let text: String = hint_line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Ctrl+E"));
    }

    #[test]
    fn tool_result_expanded() {
        let mut msg = Message::new(MessageContent::ToolResult {
            name: "read_file".into(),
            preview: "hello".into(),
            full_result: Some("hello\nworld".into()),
            is_error: false,
            duration_ms: 1500,
        });
        msg.toggle_collapsed();
        assert!(!msg.collapsed);
        let lines = msg.to_lines();
        // summary + blank + 2 content lines + blank = 5
        assert!(lines.len() >= 4);
    }

    #[test]
    fn toggle_collapsed_only_for_tool_result() {
        let mut msg = Message::new(MessageContent::AssistantText("hello".into()));
        msg.toggle_collapsed();
        assert!(msg.collapsed); // unchanged — not a tool result
    }

    #[test]
    fn diff_line_colors() {
        assert_eq!(diff_line_style("+added").fg, Some(Color::Green));
        assert_eq!(diff_line_style("-removed").fg, Some(Color::Red));
        assert_eq!(diff_line_style("@@ -1,3 +1,4 @@").fg, Some(Color::Magenta));
        assert_eq!(diff_line_style("--- a/file").fg, Some(Color::Cyan));
        assert_eq!(diff_line_style("+++ b/file").fg, Some(Color::Cyan));
        assert_eq!(diff_line_style(" context").fg, Some(Color::Gray));
    }

    #[test]
    fn cache_invalidation() {
        let msg = Message::new(MessageContent::AssistantText("hello".into()));
        let lines1 = msg.to_lines();
        assert_eq!(lines1.len(), 1);
        // Cache hit — same result
        let lines2 = msg.to_lines();
        assert_eq!(lines2.len(), 1);
        // Invalidate and verify it re-renders
        msg.invalidate_cache();
        let lines3 = msg.to_lines();
        assert_eq!(lines3.len(), 1);
    }
}
