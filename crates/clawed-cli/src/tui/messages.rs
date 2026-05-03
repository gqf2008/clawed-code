use super::verbs::{ERROR_MARKER, THINKING_MARKER, TURN_COMPLETION_MARKER, WARNING_MARKER};
use super::{markdown, MUTED};

use clawed_core::text_util::strip_system_reminders;
use unicode_width::UnicodeWidthStr;

use std::cell::RefCell;
use std::time::Instant;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Returns an appropriate style for a line in a unified diff.
fn diff_line_style(line: &str) -> Style {
    if line.starts_with("+++") || line.starts_with("---") {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
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

// ── Message content types ───────────────────────────────────────────────────

/// The content type of a single message in the conversation history.
#[derive(Debug, Clone)]
pub enum MessageContent {
    UserInput(String),
    AssistantText(String),
    ThinkingText(String),
    /// A tool execution — merges use + result into one visual unit.
    ToolExecution {
        name: String,
        /// Short command / input summary for display (e.g. `cargo test`).
        input: Option<String>,
        /// Last few output lines shown inline under the header.
        output_lines: Vec<String>,
        is_error: bool,
        duration_ms: u64,
        /// Full output for the expand view.
        full_result: Option<String>,
        /// Nesting depth for tree rendering (0 = top-level, 1+ = sub-tool).
        depth: u32,
    },
    System(String),
}

/// Rendering context for a tool execution message.
/// Bundles all parameters that affect visual layout so
/// `render_tool_execution` doesn't take 9+ positional args.
struct ToolRenderCtx<'a> {
    name: &'a str,
    input: Option<&'a str>,
    output_lines: &'a [String],
    is_error: bool,
    duration_ms: u64,
    full_result: Option<&'a str>,
    depth: u32,
    has_sibling_after: bool,
    live_duration_ms: Option<u64>,
}

/// Continuation prefix for assistant-side output lines (aligned with official CC).
const CONTINUATION_PREFIX: &str = "\u{23BF}  ";

/// A single message with timestamp and line cache.
#[derive(Debug, Clone)]
pub struct Message {
    pub content: MessageContent,
    #[allow(dead_code)]
    pub timestamp: Instant,
    /// Whether the tool execution is collapsed (true = show header only).
    pub collapsed: bool,
    /// Cached rendered lines. Invalidated when content changes.
    cached_lines: RefCell<Option<Vec<Line<'static>>>>,
}

impl Message {
    pub fn new(content: MessageContent) -> Self {
        // Tool executions default to expanded so live output is visible
        // during the tool's execution; other content types start collapsed.
        let collapsed = !matches!(content, MessageContent::ToolExecution { .. });
        Self {
            content,
            timestamp: Instant::now(),
            collapsed,
            cached_lines: RefCell::new(None),
        }
    }

    /// Invalidate the line cache (call after mutating content).
    pub fn invalidate_cache(&self) {
        *self.cached_lines.borrow_mut() = None;
    }

    pub fn append_assistant_text(&mut self, text: &str) {
        let text = strip_system_reminders(text);
        if text.is_empty() {
            return;
        }

        match &mut self.content {
            MessageContent::AssistantText(buf) => {
                buf.push_str(&text);
            }
            _ => return,
        }

        // Always invalidate cache so prefixes are re-applied correctly on re-render.
        *self.cached_lines.get_mut() = None;
    }

    pub fn append_thinking_text(&mut self, text: &str) {
        let text = strip_system_reminders(text);
        if text.is_empty() {
            return;
        }

        match &mut self.content {
            MessageContent::ThinkingText(buf) => buf.push_str(&text),
            _ => return,
        }

        // When collapsed, the cached lines contain the collapse hint, not
        // thinking content. Skip incremental update to avoid corrupting the hint.
        if self.collapsed {
            *self.cached_lines.borrow_mut() = None;
            return;
        }

        let cached_lines = self.cached_lines.get_mut();
        if let Some(lines) = cached_lines.as_mut() {
            let style = Style::default().fg(MUTED).add_modifier(Modifier::ITALIC);
            let mut iter = text.lines();
            let Some(first) = iter.next() else {
                return;
            };
            if lines.is_empty() {
                lines.push(Line::styled(
                    format!("{THINKING_MARKER} Thinking\u{2026}"),
                    style,
                ));
            }
            if let Some(last_line) = lines.last_mut() {
                let mut merged = line_text(last_line);
                merged.push_str(first);
                *last_line = Line::styled(merged, style);
            } else {
                lines.push(Line::styled(format!("  {first}"), style));
            }
            for part in iter {
                lines.push(Line::styled(format!("  {part}"), style));
            }
            return;
        }
        *cached_lines = None;
    }

    /// Update the last ToolExecution message with result info.
    /// Called when ToolUseComplete arrives.
    /// Preserves streaming output_lines — only adds full_result for expand.
    pub fn update_tool_result(&mut self, is_error: bool, duration_ms: u64, result: &str) {
        if let MessageContent::ToolExecution {
            output_lines,
            full_result,
            is_error: ref mut e,
            duration_ms: ref mut d,
            ..
        } = &mut self.content
        {
            let stripped = strip_system_reminders(result);
            let needs_full = stripped.lines().nth(5).is_some() || output_lines.is_empty();
            *full_result = if needs_full {
                Some(stripped.into_owned())
            } else {
                None
            };
            *e = is_error;
            *d = duration_ms;
            self.invalidate_cache();
        }
    }

    /// Append a live output line to the ToolExecution message.
    pub fn append_tool_output_line(&mut self, line: String) {
        if let MessageContent::ToolExecution { output_lines, .. } = &mut self.content {
            output_lines.push(line);
            // Keep only last 5 lines
            if output_lines.len() > 5 {
                output_lines.remove(0);
            }
            self.invalidate_cache();
        }
    }

    /// Toggle collapsed state for tool executions and thinking blocks.
    pub fn toggle_collapsed(&mut self) {
        if matches!(
            self.content,
            MessageContent::ToolExecution { .. } | MessageContent::ThinkingText(_)
        ) {
            self.collapsed = !self.collapsed;
            self.invalidate_cache();
        }
    }

    /// Whether this message is a collapsible tool execution (has full_result).
    pub fn is_collapsible(&self) -> bool {
        matches!(
            self.content,
            MessageContent::ToolExecution {
                full_result: Some(_),
                ..
            }
        )
    }

    /// Convert this message to ratatui `Line`s for display.
    /// Results are cached; subsequent calls return the cached version.
    /// Pass `has_sibling_after=true` when the next message is a sibling tool
    /// so tree branches render `│` connectors.
    /// Pass `live_duration_ms` for running tools to show elapsed time inline.
    pub fn to_lines_with_context(
        &self,
        has_sibling_after: bool,
        live_duration_ms: Option<u64>,
    ) -> Vec<Line<'static>> {
        if !has_sibling_after && live_duration_ms.is_none() {
            if let Some(ref cached) = *self.cached_lines.borrow() {
                return cached.clone();
            }
        }

        let lines = self.render_lines(has_sibling_after, live_duration_ms);
        if !has_sibling_after && live_duration_ms.is_none() {
            *self.cached_lines.borrow_mut() = Some(lines.clone());
        }
        lines
    }

    fn render_lines(
        &self,
        has_sibling_after: bool,
        live_duration_ms: Option<u64>,
    ) -> Vec<Line<'static>> {
        match &self.content {
            MessageContent::UserInput(text) => {
                let text_style = Style::default().add_modifier(Modifier::BOLD);
                let prefix = Span::styled(
                    "\u{276F} ",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                );
                let mut lines = vec![Line::from("")];
                for (i, part) in text.split('\n').enumerate() {
                    if i == 0 {
                        lines.push(Line::from(vec![
                            prefix.clone(),
                            Span::styled(part.to_string(), text_style),
                        ]));
                    } else {
                        lines.push(Line::styled(part.to_string(), text_style));
                    }
                }
                lines
            }
            MessageContent::AssistantText(text) => {
                if text.is_empty() {
                    return vec![];
                }
                let dim = Style::default().fg(MUTED);
                let prefix_text = "\u{23FA} ";
                let prefix = Span::styled(prefix_text.to_string(), dim);
                let blank_prefix = Span::raw(" ".repeat(prefix_text.width()));
                markdown::render_markdown(text)
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut line)| {
                        if i == 0 {
                            line.spans.insert(0, prefix.clone());
                        } else {
                            line.spans.insert(0, blank_prefix.clone());
                        }
                        line
                    })
                    .collect()
            }
            MessageContent::ThinkingText(text) => {
                if text.is_empty() {
                    return vec![];
                }
                let dim_italic = Style::default().fg(MUTED).add_modifier(Modifier::ITALIC);
                if self.collapsed {
                    return vec![Line::from(vec![
                        Span::styled(
                            format!("{THINKING_MARKER} Thinking"),
                            dim_italic,
                        ),
                        Span::raw("  "),
                        Span::styled("(Ctrl+O to expand)", Style::default().fg(MUTED)),
                    ])];
                }
                let mut lines: Vec<Line<'static>> = Vec::new();
                lines.push(Line::styled(
                    format!("{THINKING_MARKER} Thinking\u{2026}"),
                    dim_italic,
                ));
                // Body lines with indent.
                for l in text.lines() {
                    lines.push(Line::styled(format!("  {l}"), dim_italic));
                }
                lines
            }
            MessageContent::ToolExecution {
                name,
                input,
                output_lines,
                is_error,
                duration_ms,
                full_result,
                depth,
            } => self.render_tool_execution(&ToolRenderCtx {
                name,
                input: input.as_deref(),
                output_lines,
                is_error: *is_error,
                duration_ms: *duration_ms,
                full_result: full_result.as_deref(),
                depth: *depth,
                has_sibling_after,
                live_duration_ms,
            }),
            MessageContent::System(text) => {
                let style = if text.starts_with(ERROR_MARKER) {
                    Style::default().fg(Color::Red)
                } else if text.starts_with(WARNING_MARKER)
                    || text.starts_with(TURN_COMPLETION_MARKER)
                {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(MUTED)
                };
                text.lines()
                    .map(|l| Line::styled(l.to_string(), style))
                    .collect()
            }
        }
    }

    fn render_tool_execution(&self, ctx: &ToolRenderCtx<'_>) -> Vec<Line<'static>> {
        debug_assert!(ctx.depth <= 2, "unexpected tool depth: {}", ctx.depth);
        const MAX_INPUT_CHARS: usize = 80;

        let mut lines = Vec::new();

        // ── Tree indent based on depth ──
        let indent = "  ".repeat(ctx.depth as usize);
        let child_prefix = if ctx.depth > 0 {
            if ctx.has_sibling_after {
                "├─ "
            } else {
                "└─ "
            }
        } else {
            ""
        };
        let output_indent = if ctx.depth > 0 && ctx.has_sibling_after {
            format!("{}│ ", "  ".repeat(ctx.depth as usize))
        } else {
            "  ".repeat(ctx.depth as usize + 1)
        };

        // ── Header: ● Bash(command...) or   └─ Bash(command...) ──
        let mut header_spans: Vec<Span<'static>> = Vec::new();
        header_spans.push(Span::styled(
            format!("{indent}{child_prefix}"),
            Style::default().fg(MUTED),
        ));
        let bullet_color = if ctx.is_error {
            Color::Red
        } else if ctx.live_duration_ms.is_some() {
            MUTED
        } else {
            Color::Green
        };
        header_spans.push(Span::styled(
            "\u{23FA} ",
            Style::default().fg(bullet_color),
        ));
        let display_name = super::user_facing_tool_name(ctx.name);
        header_spans.push(Span::styled(
            display_name.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if let Some(cmd) = ctx.input {
            let display = if cmd.len() > MAX_INPUT_CHARS {
                let truncated: String = cmd.chars().take(MAX_INPUT_CHARS).collect();
                format!("({truncated}\u{2026})")
            } else {
                format!("({})", cmd)
            };
            header_spans.push(Span::styled(display, Style::default().fg(MUTED)));
        }
        lines.push(Line::from(header_spans));

        // ── Output lines (diff-colored like expanded results) ──
        for line in ctx.output_lines {
            lines.push(Line::styled(
                format!("{output_indent}{CONTINUATION_PREFIX}{}", line),
                diff_line_style(line),
            ));
        }

        // ── Duration hint (completed or live) ──
        let effective_dur = if ctx.duration_ms > 0 {
            Some(ctx.duration_ms)
        } else {
            ctx.live_duration_ms
        };
        if let Some(d) = effective_dur {
            let dur = if d >= 1000 {
                format!("{:.1}s", d as f64 / 1000.0)
            } else {
                format!("{}ms", d)
            };
            let marker = if ctx.duration_ms > 0 && !ctx.is_error {
                "✓ "
            } else {
                ""
            };
            lines.push(Line::styled(
                format!("{output_indent}{CONTINUATION_PREFIX}{marker}({})", dur),
                Style::default().fg(MUTED),
            ));
        }

        // ── Error indicator ──
        if ctx.is_error {
            lines.push(Line::styled(
                format!("{output_indent}{CONTINUATION_PREFIX}✗ failed"),
                Style::default().fg(Color::Red),
            ));
        }

        // ── Fold hint / expanded result ──
        if let Some(full) = ctx.full_result {
            if self.collapsed {
                if ctx.output_lines.is_empty() {
                    // No streaming happened — show first few lines inline.
                    let preview_lines: Vec<&str> = full.lines().take(5).collect();
                    let total = full.lines().count();
                    for l in &preview_lines {
                        let style = diff_line_style(l);
                        lines.push(Line::styled(
                            format!("{output_indent}{CONTINUATION_PREFIX}{}", l),
                            style,
                        ));
                    }
                    if total > 5 {
                        lines.push(Line::styled(
                            format!(
                                "{output_indent}{CONTINUATION_PREFIX}+ {} more lines (Ctrl+E to expand)",
                                total - 5
                            ),
                            Style::default().fg(MUTED),
                        ));
                    }
                } else {
                    let n = full.lines().count();
                    lines.push(Line::styled(
                        format!("{output_indent}{CONTINUATION_PREFIX}+ {n} more lines (Ctrl+E to expand)"),
                        Style::default().fg(MUTED),
                    ));
                }
            } else {
                lines.push(Line::from(""));
                for l in full.lines() {
                    let style = diff_line_style(l);
                    lines.push(Line::styled(
                        format!("{output_indent}{CONTINUATION_PREFIX}{}", l),
                        style,
                    ));
                }
                lines.push(Line::from(""));
            }
        }

        lines
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_input_has_prompt() {
        let msg = Message::new(MessageContent::UserInput("hello".into()));
        let lines = msg.to_lines_with_context(false, None);
        assert!(lines.len() >= 2); // blank + prompt line
    }

    #[test]
    fn multiline_user_input_renders_multiple_lines() {
        let msg = Message::new(MessageContent::UserInput("hello\nworld".into()));
        let lines = msg.to_lines_with_context(false, None);
        assert_eq!(lines.len(), 3);
        let first: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        let second: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(first, "❯ hello");
        assert_eq!(second, "world");
    }

    #[test]
    fn empty_assistant_text() {
        let msg = Message::new(MessageContent::AssistantText(String::new()));
        assert!(msg.to_lines_with_context(false, None).is_empty());
    }

    #[test]
    fn multiline_assistant_text() {
        let msg = Message::new(MessageContent::AssistantText("a\nb\nc".into()));
        assert_eq!(msg.to_lines_with_context(false, None).len(), 3);
    }

    #[test]
    fn tool_execution_header_shows_command() {
        let msg = Message::new(MessageContent::ToolExecution {
            name: "Bash".into(),
            input: Some("cargo test -p clawed-cli".into()),
            output_lines: vec![],
            is_error: false,
            duration_ms: 0,
            full_result: None,
            depth: 0,
        });
        let lines = msg.to_lines_with_context(false, None);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Bash"));
        assert!(text.contains("cargo test"));
    }

    #[test]
    fn tool_execution_long_command_truncated() {
        let long_cmd = "a".repeat(200);
        let msg = Message::new(MessageContent::ToolExecution {
            name: "Bash".into(),
            input: Some(long_cmd.clone()),
            output_lines: vec![],
            is_error: false,
            duration_ms: 0,
            full_result: None,
            depth: 0,
        });
        let lines = msg.to_lines_with_context(false, None);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        // Truncated with ellipsis
        assert!(text.contains('\u{2026}'));
    }

    #[test]
    fn tool_execution_shows_output_lines() {
        let msg = Message::new(MessageContent::ToolExecution {
            name: "Bash".into(),
            input: Some("date".into()),
            output_lines: vec!["2026-04-16".into(), "Thu".into()],
            is_error: false,
            duration_ms: 500,
            full_result: None,
            depth: 0,
        });
        let lines = msg.to_lines_with_context(false, None);
        // header + 2 output + 1 duration = 4
        assert_eq!(lines.len(), 4);
        let text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("2026-04-16"));
    }

    #[test]
    fn tool_execution_collapsed_shows_fold_hint() {
        let mut msg = Message::new(MessageContent::ToolExecution {
            name: "Read".into(),
            input: Some("Cargo.toml".into()),
            output_lines: vec!["line 1".into()],
            is_error: false,
            duration_ms: 100,
            full_result: Some("line 1\nline 2\nline 3\nline 4\nline 5\nline 6".into()),
            depth: 0,
        });
        msg.collapsed = true;
        let lines = msg.to_lines_with_context(false, None);
        let last_text: String = lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(last_text.contains("+ 6 more lines"));
        assert!(last_text.contains("Ctrl+E to expand"));
    }

    #[test]
    fn tool_execution_expanded_shows_full_result() {
        let msg = Message::new(MessageContent::ToolExecution {
            name: "Read".into(),
            input: Some("Cargo.toml".into()),
            output_lines: vec!["line 1".into()],
            is_error: false,
            duration_ms: 100,
            full_result: Some("line 1\nline 2".into()),
            depth: 0,
        });
        // ToolExecution defaults to expanded (collapsed = false)
        let lines = msg.to_lines_with_context(false, None);
        // header + output + duration + blank + 2 lines + blank
        assert!(lines.len() >= 5);
    }

    #[test]
    fn toggle_collapsed_only_for_tool_execution() {
        let mut msg = Message::new(MessageContent::AssistantText("hello".into()));
        msg.toggle_collapsed();
        assert!(msg.collapsed); // unchanged — not a tool execution
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
        let lines1 = msg.to_lines_with_context(false, None);
        assert_eq!(lines1.len(), 1);
        // Cache hit — same result
        let lines2 = msg.to_lines_with_context(false, None);
        assert_eq!(lines2.len(), 1);
        // Invalidate and verify it re-renders
        msg.invalidate_cache();
        let lines3 = msg.to_lines_with_context(false, None);
        assert_eq!(lines3.len(), 1);
    }

    #[test]
    fn append_assistant_text_extends_plain_cache() {
        let mut msg = Message::new(MessageContent::AssistantText("hello".into()));
        assert_eq!(msg.to_lines_with_context(false, None).len(), 1);

        msg.append_assistant_text(" world\nnext");
        let lines = msg.to_lines_with_context(false, None);

        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "\u{23FA} hello world");
        assert_eq!(line_text(&lines[1]), "  next");
    }

    #[test]
    fn append_thinking_text_extends_cache() {
        let mut msg = Message::new(MessageContent::ThinkingText("thinking".into()));
        msg.collapsed = false;
        assert_eq!(msg.to_lines_with_context(false, None).len(), 2);

        msg.append_thinking_text("...\nmore");
        let lines = msg.to_lines_with_context(false, None);

        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[1]), "  thinking...");
        assert_eq!(line_text(&lines[2]), "  more");
    }

    #[test]
    fn thinking_text_has_muted_italic_style() {
        let mut msg = Message::new(MessageContent::ThinkingText("hello".into()));
        msg.collapsed = false;
        let lines = msg.to_lines_with_context(false, None);
        // Header line: "∴ Thinking…"
        assert_eq!(lines[0].style.fg, Some(MUTED));
        assert!(lines[0].style.add_modifier.contains(Modifier::ITALIC));
        assert!(line_text(&lines[0]).starts_with('\u{2234}'));
        // Body line: "  hello"
        assert_eq!(lines[1].style.fg, Some(MUTED));
        assert!(lines[1].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn append_thinking_text_newlines_match_lines_behavior() {
        let mut msg = Message::new(MessageContent::ThinkingText("a".into()));
        msg.collapsed = false;
        // Append text that ends with a newline — should match str::lines() behavior.
        msg.append_thinking_text("\nb\n");
        let lines = msg.to_lines_with_context(false, None);
        // "a\nb\n".lines() → ["a", "b"], plus header.
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[1]), "  a");
        assert_eq!(line_text(&lines[2]), "  b");
    }
}
