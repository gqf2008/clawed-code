use super::diff_style;
use super::verbs::{ERROR_MARKER, THINKING_MARKER, TURN_COMPLETION_MARKER, WARNING_MARKER};
use super::{blank_line, line_text, markdown, muted, MUTED};

use clawed_core::text_util::strip_system_reminders;
use unicode_width::UnicodeWidthStr;

use std::cell::RefCell;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Returns an appropriate style for a JSON line (used by MCP tool rendering).
fn json_line_style(line: &str) -> Style {
    let trimmed = line.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('}') {
        Style::default().fg(Color::Cyan)
    } else if trimmed.starts_with('[') || trimmed.starts_with(']') {
        Style::default().fg(Color::Magenta)
    } else if trimmed.starts_with('"') {
        Style::default().fg(Color::Green)
    } else if trimmed.starts_with("true") || trimmed.starts_with("false") || trimmed.starts_with("null") {
        Style::default().fg(Color::Yellow)
    } else if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-') {
        Style::default().fg(Color::Blue)
    } else {
        muted()
    }
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

/// Renders a diff line with line-number gutter and background coloring.
/// Returns (gutter_span, content_spans) for multi-span content with word-level diff.
fn diff_gutter_line(
    line: &str,
    _line_num: Option<u32>,
    prev_line: Option<&str>,
) -> (Span<'static>, Vec<Span<'static>>) {
    let palette = diff_style::palette();
    let (marker, bg, word_bg) = if line.starts_with('+') {
        ("+", Some(palette.added_bg), Some(palette.added_word_bg))
    } else if line.starts_with('-') {
        ("-", Some(palette.removed_bg), Some(palette.removed_word_bg))
    } else {
        (" ", None, None)
    };

    let gutter = format!("{} ", marker);

    let gutter_style = if let Some(b) = bg {
        Style::default().bg(b).fg(MUTED)
    } else {
        Style::default().fg(MUTED)
    };

    // Word-level diff: compare with previous line if it's a complementary change
    let content_spans = if let (Some(bg_color), Some(word_color), Some(prev)) = (bg, word_bg, prev_line) {
        let prev_prefix = if prev.starts_with('+') { "+" } else if prev.starts_with('-') { "-" } else { " " };
        let curr_prefix = if line.starts_with('+') { "+" } else if line.starts_with('-') { "-" } else { " " };
        // Only do word diff for opposite add/remove pairs
        if prev_prefix != curr_prefix && prev_prefix != " " && curr_prefix != " " {
            word_diff_spans(prev, line, bg_color, word_color)
        } else {
            vec![Span::styled(format!(" {}", line), Style::default().bg(bg_color))]
        }
    } else if let Some(bg_color) = bg {
        vec![Span::styled(format!(" {}", line), Style::default().bg(bg_color))]
    } else {
        vec![Span::styled(format!(" {}", line), Style::default())]
    };

    (Span::styled(gutter, gutter_style), content_spans)
}

/// Compute word-level diff spans between removed (prev) and added (curr) lines.
fn word_diff_spans(prev: &str, curr: &str, bg: Color, word_bg: Color) -> Vec<Span<'static>> {
    let prev_trimmed = prev.trim_start_matches(&['+', '-', ' '][..]);
    let curr_trimmed = curr.trim_start_matches(&['+', '-', ' '][..]);

    // Find common prefix/suffix
    let common_prefix_len = prev_trimmed
        .chars()
        .zip(curr_trimmed.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let common_suffix_len = prev_trimmed
        .chars()
        .rev()
        .zip(curr_trimmed.chars().rev())
        .take_while(|(a, b)| a == b)
        .count();

    let curr_mid = &curr_trimmed[common_prefix_len..curr_trimmed.len().saturating_sub(common_suffix_len)];

    let prefix = &prev_trimmed[..common_prefix_len];
    let suffix = &prev_trimmed[prev_trimmed.len().saturating_sub(common_suffix_len)..];

    let base = Style::default().bg(bg);
    let highlight = Style::default().bg(word_bg);

    let mut spans = vec![
        Span::styled(format!(" {}", prefix), base),
        Span::styled(curr_mid.to_string(), highlight),
        Span::styled(suffix.to_string(), base),
    ];
    // Filter out empty spans
    spans.retain(|s| !s.content.is_empty());
    spans
}

/// Render an agent progress line (aligned with CC AgentProgressLine).
/// Tree-char + colored badge + stats + status sub-line.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn render_agent_progress_line(
    agent_name: &str,
    agent_color: Color,
    is_last: bool,
    is_resolved: bool,
    is_selected: bool,
    tool_count: u32,
    token_count: u64,
    status_text: String,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let dim = muted();
    let (tree_char, tree_style) = if is_selected {
        if is_last { ("\u{2558}\u{2550} ", Style::default().fg(Color::Yellow)) }
        else { ("\u{255E}\u{2550} ", Style::default().fg(Color::Yellow)) }
    } else {
        if is_last { ("\u{2514}\u{2500} ", muted()) }
        else { ("\u{251C}\u{2500} ", muted()) }
    };
    let badge_style = Style::default()
        .fg(agent_color)
        .add_modifier(Modifier::BOLD);

    // Main row: tree char + colored badge + stats
    let mut spans = vec![
        Span::styled(tree_char, tree_style),
        Span::styled(format!("{} ", agent_name), badge_style),
    ];
    if !is_resolved {
        spans.push(Span::styled(
            format!(" \u{00B7} {} tool{}", tool_count, if tool_count == 1 { "" } else { "s" }),
            dim,
        ));
        if token_count > 0 {
            spans.push(Span::styled(
                format!(" \u{00B7} ~{} tokens", token_count),
                dim,
            ));
        }
    }
    lines.push(Line::from(spans));

    // Status sub-line: continuation prefix + status text
    let cont = if is_last { "   " } else { "\u{2502}  " };
    let sub_prefix = format!("{}{} ", cont, "\u{23BF}");
    lines.push(Line::styled(
        format!("{}{}", sub_prefix, status_text),
        dim,
    ));

    lines
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

impl MessageContent {
    /// Extract plain searchable text from this content.
    #[allow(dead_code)]
pub fn plain_text(&self) -> String {
        match self {
            MessageContent::UserInput(s)
            | MessageContent::AssistantText(s)
            | MessageContent::ThinkingText(s)
            | MessageContent::System(s) => s.clone(),
            MessageContent::ToolExecution {
                name,
                input,
                output_lines,
                full_result,
                ..
            } => {
                let mut parts = vec![name.clone()];
                if let Some(inp) = input {
                    parts.push(inp.clone());
                }
                for line in output_lines {
                    parts.push(line.clone());
                }
                if let Some(result) = full_result {
                    parts.push(result.clone());
                }
                parts.join("\n")
            }
        }
    }
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
// CONTINUATION_PREFIX removed — CC uses plain indent)

/// A single message with timestamp and line cache.
#[derive(Debug, Clone)]
pub struct Message {
    pub content: MessageContent,
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
            let style = muted().add_modifier(Modifier::ITALIC);
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

    /// Extract plain searchable text from this message.
    #[allow(dead_code)]
pub fn plain_text(&self) -> String {
        self.content.plain_text()
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
                let mut lines = vec![blank_line()];
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
                let dim = muted();
                let prefix_text = "\u{25CF} ";
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
                let dim_italic = muted().add_modifier(Modifier::ITALIC);
                if self.collapsed {
                    return vec![Line::from(vec![
                        Span::styled(
                            format!("{THINKING_MARKER} Thinking"),
                            dim_italic,
                        ),
                        Span::raw("  "),
                        Span::styled("(Ctrl+O to expand)", muted()),
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
                if markdown::likely_markdown(text) {
                    let mut lines = markdown::render_markdown(text);
                    for line in &mut lines {
                        for span in &mut line.spans {
                            if span.style.fg.is_none() {
                                span.style = span.style.fg(MUTED);
                            }
                        }
                    }
                    lines
                } else {
                    let style = if text.starts_with(ERROR_MARKER) {
                        Style::default().fg(Color::Red)
                    } else if text.starts_with(WARNING_MARKER)
                        || text.starts_with(TURN_COMPLETION_MARKER)
                    {
                        Style::default().fg(Color::Yellow)
                    } else {
                        muted()
                    };
                    text.lines()
                        .map(|l| Line::styled(l.to_string(), style))
                        .collect()
                }
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
            muted(),
        ));
        let bullet_color = if ctx.is_error {
            Color::Red
        } else if ctx.live_duration_ms.is_some() {
            MUTED
        } else {
            Color::Green
        };
        header_spans.push(Span::styled(
            "\u{25CF} ",
            Style::default().fg(bullet_color),
        ));
        let display_name = super::user_facing_tool_name(ctx.name);
        header_spans.push(Span::styled(
            display_name,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if let Some(cmd) = ctx.input {
            let display = if cmd.len() > MAX_INPUT_CHARS {
                let truncated: String = cmd.chars().take(MAX_INPUT_CHARS).collect();
                format!("({truncated}\u{2026})")
            } else {
                format!("({})", cmd)
            };
            header_spans.push(Span::styled(display, muted()));
        }
        lines.push(Line::from(header_spans));

        // ── Shell output: show live lines with elapsed (aligned with CC ShellProgressMessage) ──
        let is_shell = super::is_shell_tool(&ctx.name);
        if is_shell {
            let has_output = !ctx.output_lines.is_empty();
            if has_output {
                for line in ctx.output_lines.iter() {
                    lines.push(Line::styled(
                        format!("{output_indent}  {}", line),
                        muted(),
                    ));
                }
            } else if ctx.live_duration_ms.is_some() {
                let elapsed = ctx.live_duration_ms.unwrap_or(0);
                let elapsed_str = if elapsed >= 1000 {
                    format!("{:.1}s", elapsed as f64 / 1000.0)
                } else {
                    format!("{}ms", elapsed)
                };
                lines.push(Line::styled(
                    format!("{output_indent}  Running\u{2026} ({})", elapsed_str),
                    muted(),
                ));
            }
            // Duration hint
            let effective_dur = if ctx.duration_ms > 0 { Some(ctx.duration_ms) } else { ctx.live_duration_ms };
            if let Some(d) = effective_dur {
                if d > 0 {
                    let elapsed = if d >= 1000 {
                        format!("{:.1}s", d as f64 / 1000.0)
                    } else {
                        format!("{}ms", d)
                    };
                    lines.push(Line::styled(
                        format!("{output_indent}  ({})", elapsed),
                        muted(),
                    ));
                }
            }
        }
        let tool_lower = ctx.name.to_lowercase();
        let is_web = tool_lower.contains("web");
        let is_mcp = tool_lower.starts_with("mcp__")
            || tool_lower.starts_with("mcp_");
        let is_notebook = tool_lower.contains("notebook");

        if !is_shell {
            // Non-shell tools: output with indent
            for line in ctx.output_lines {
                let style = if is_web || is_notebook {
                    muted()
                } else {
                    diff_line_style(line)
                };
                lines.push(Line::styled(
                    format!("{output_indent}  {line}"),
                    style,
                ));
            }
        }

        // ── Duration hint (completed tools) ──
        if ctx.duration_ms > 0 && !is_shell {
            let dur = if ctx.duration_ms >= 1000 {
                format!("{:.1}s", ctx.duration_ms as f64 / 1000.0)
            } else {
                format!("{}ms", ctx.duration_ms)
            };
            let marker = if !ctx.is_error { "✓ " } else { "" };
            lines.push(Line::styled(
                format!("{output_indent}  {marker}({})", dur),
                muted(),
            ));
        }

        // ── Error indicator ──
        if ctx.is_error {
            lines.push(Line::styled(
                format!("{output_indent}  ✗ failed"),
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
                        let style = if is_mcp {
                            json_line_style(l)
                        } else if is_web || is_notebook {
                            muted()
                        } else {
                            diff_line_style(l)
                        };
                        lines.push(Line::styled(
                            format!("{output_indent}  {}", l),
                            style,
                        ));
                    }
                    if total > 5 {
                        lines.push(Line::styled(
                            format!(
                                "{output_indent}  + {} more lines (Ctrl+E to expand)",
                                total - 5
                            ),
                            muted(),
                        ));
                    }
                } else {
                    let n = full.lines().count();
                    lines.push(Line::styled(
                        format!("{output_indent}  + {n} more lines (Ctrl+E to expand)"),
                        muted(),
                    ));
                }
            } else {
                let is_file_edit = matches!(
                    ctx.name.to_lowercase().as_str(),
                    "edit" | "write" | "multiedit" | "multi_edit"
                );
                if is_file_edit {
                    // Structured diff with gutter + background colors.
                    let border = "\u{2504}".repeat(50);
                    lines.push(Line::styled(border.clone(), muted()));

                    let mut old_line: u32 = 0;
                    let mut new_line: u32 = 0;
                    let mut in_hunk = false;
                    let mut prev_line: Option<String> = None;

                    for l in full.lines() {
                        if l.starts_with("@@") {
                            if let (Some(o), Some(n)) = (
                                l.split('-').nth(1).and_then(|s| s.split(' ').next()),
                                l.split('+').nth(1).and_then(|s| s.split(' ').next()),
                            ) {
                                old_line = o.split(',').next().and_then(|s| s.parse().ok()).unwrap_or(0);
                                new_line = n.split(',').next().and_then(|s| s.parse().ok()).unwrap_or(0);
                            }
                            in_hunk = true;
                            prev_line = None;
                            lines.push(Line::styled(
                                format!("{} \u{2026}", "\u{2504}".repeat(48)),
                                muted(),
                            ));
                            lines.push(Line::styled(l.to_string(), Style::default().fg(Color::Magenta)));
                            continue;
                        }
                        if l.starts_with("---") || l.starts_with("+++") {
                            lines.push(Line::styled(l.to_string(), muted()));
                            prev_line = None;
                            continue;
                        }
                        if !in_hunk {
                            lines.push(Line::styled(l.to_string(), muted()));
                            continue;
                        }

                        let line_num = if l.starts_with('+') {
                            let n = Some(new_line);
                            new_line += 1;
                            n
                        } else if l.starts_with('-') {
                            let n = Some(old_line);
                            old_line += 1;
                            n
                        } else {
                            let n = Some(new_line);
                            old_line += 1;
                            new_line += 1;
                            n
                        };

                        let prev_ref = prev_line.as_deref();
                        let (gutter, content_spans) = diff_gutter_line(l, line_num, prev_ref);
                        let mut all_spans = vec![gutter];
                        all_spans.extend(content_spans);
                        lines.push(Line::from(all_spans));
                        prev_line = Some(l.to_string());
                    }
                    lines.push(Line::styled(border, muted()));
                } else if is_web {
                    // WebFetch/WebSearch: render as markdown (tool already converts HTML→MD).
                    let md_lines = markdown::render_markdown(full);
                    let md_count = md_lines.len();
                    if md_count > 30 {
                        // Long results: show first 30 lines + fold hint
                        for mut line in md_lines.into_iter().take(30) {
                            line.spans.insert(0, Span::raw(output_indent.clone() + "  "));
                            lines.push(line);
                        }
                        lines.push(Line::styled(
                            format!("{output_indent}  + {} more lines (Ctrl+E to expand)",
                                md_count - 30),
                            muted(),
                        ));
                    } else {
                        for mut line in md_lines {
                            line.spans.insert(0, Span::raw(output_indent.clone() + "  "));
                            lines.push(line);
                        }
                    }
                } else if is_mcp {
                    // MCP tools: pretty-print JSON if detected, else plain text.
                    let trimmed = full.trim();
                    let is_json = trimmed.starts_with('{') || trimmed.starts_with('[');
                    if is_json {
                        lines.push(blank_line());
                        for l in full.lines() {
                            let style = json_line_style(l);
                            lines.push(Line::styled(
                                format!("{output_indent}  {}", l),
                                style,
                            ));
                        }
                        lines.push(blank_line());
                    } else {
                        lines.push(blank_line());
                        for l in full.lines() {
                            lines.push(Line::styled(
                                format!("{output_indent}  {}", l),
                                muted(),
                            ));
                        }
                        lines.push(blank_line());
                    }
                } else if is_notebook {
                    // NotebookEdit: plain text with subtle icon.
                    lines.push(blank_line());
                    for l in full.lines() {
                        lines.push(Line::styled(
                            format!("{output_indent}  {}", l),
                            muted(),
                        ));
                    }
                    lines.push(blank_line());
                } else {
                    lines.push(blank_line());
                    for l in full.lines() {
                        let style = diff_line_style(l);
                        lines.push(Line::styled(
                            format!("{output_indent}  {}", l),
                            style,
                        ));
                    }
                    lines.push(blank_line());
                }
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
        assert_eq!(line_text(&lines[0]), "\u{25CF} hello world");
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

    #[test]
    fn system_msg_with_markdown_is_rendered() {
        let msg = Message::new(MessageContent::System(
            "### Heading\n\n- list item\n\n| A | B |\n|---|---|".to_string(),
        ));
        let lines = msg.to_lines_with_context(false, None);
        let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect::<String>() + " ";
        assert!(
            text.contains("Heading") && !text.contains("###"),
            "System markdown: '###' should be stripped, got: {text:?}"
        );
    }

    #[test]
    fn system_msg_plain_text_no_markdown_passthrough() {
        let msg = Message::new(MessageContent::System("plain message".to_string()));
        let lines = msg.to_lines_with_context(false, None);
        assert!(!lines.is_empty());
        let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
        assert_eq!(text, "plain message");
    }

    // ── Rendering verification ──

    #[test]
    fn verify_diff_gutter_format() {
        let line = "+added line";
        let (gutter, content) = diff_gutter_line(line, Some(5), None);
        let gutter_text: String = gutter.content.to_string();
        assert!(gutter_text.contains("+"), "gutter must have + marker");
        // Content has the active palette's added background
        let expected = diff_style::palette().added_bg;
        assert!(content.iter().any(|s| s.style.bg == Some(expected)));
    }

    #[test]
    fn verify_diff_removed_line() {
        let line = "-removed line";
        let (gutter, content) = diff_gutter_line(line, Some(3), None);
        let gutter_text: String = gutter.content.to_string();
        assert!(gutter_text.contains("-"), "gutter must have - marker");
        let expected = diff_style::palette().removed_bg;
        assert!(content.iter().any(|s| s.style.bg == Some(expected)));
    }

    #[test]
    fn verify_diff_context_no_bg() {
        let line = " unchanged line";
        let (_gutter, content) = diff_gutter_line(line, Some(10), None);
        // Context lines have no background
        assert!(content.iter().all(|s| s.style.bg.is_none()));
    }

    #[test]
    fn render_dump_diff_and_agent() {
        // Diff gutter dump
        let diff = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n unchanged\n-removed line\n+added line\n unchanged2";
        let mut old = 0u32; let mut new = 0u32;
        eprintln!("\n═══ Diff Gutter 渲染输出 ═══");
        for l in diff.lines() {
            if l.starts_with("@@") { old = 1; new = 1; eprintln!("  [{l}]  ← magenta hunk header"); continue; }
            if l.starts_with("---") || l.starts_with("+++") { eprintln!("  [{l}]  ← muted file header"); continue; }
            let num = if l.starts_with('+') { let n = new; new += 1; Some(n) }
                      else if l.starts_with('-') { let n = old; old += 1; Some(n) }
                      else { old += 1; new += 1; Some(new-1) };
            let (g, cs) = diff_gutter_line(l, num, None);
            let gutter = g.content.to_string();
            let content: String = cs.iter().map(|s| s.content.to_string()).collect();
            let has_bg = cs.iter().any(|s| s.style.bg.is_some());
            eprintln!("  gutter[{gutter}] content[{content}] bg={has_bg}");
        }

        // Agent progress dump
        eprintln!("\n═══ Agent Progress 渲染输出 ═══");
        let lines = render_agent_progress_line(
            "code-reviewer", Color::Magenta, false, false, true, 5, 3200, "Working…".to_string(),
        );
        for (i, line) in lines.iter().enumerate() {
            let parts: Vec<String> = line.spans.iter().map(|s| {
                let fg = s.style.fg.map(|c| format!("fg={c:?}")).unwrap_or_default();
                let bg = s.style.bg.map(|c| format!("bg={c:?}")).unwrap_or_default();
                let b = if s.style.add_modifier.contains(Modifier::BOLD) { "B" } else { "" };
                format!("[{}{}{}]{}", fg, bg, b, s.content)
            }).collect();
            eprintln!("  L{i}: {}", parts.join(""));
        }
    }

    #[test]
    fn verify_diff_line_style_colors() {
        assert_eq!(diff_line_style("+added").fg, Some(Color::Green));
        assert_eq!(diff_line_style("-removed").fg, Some(Color::Red));
        assert_eq!(diff_line_style("@@ -1 +1 @@").fg, Some(Color::Magenta));
        assert_eq!(diff_line_style("--- a/file").fg, Some(Color::Cyan));
        assert_eq!(diff_line_style(" context").fg, Some(Color::Gray));
    }

    #[test]
    fn verify_agent_progress_tree_chars() {
        let lines = render_agent_progress_line(
            "test-agent", Color::Cyan, true, false, true,
            3, 1500, "Working\u{2026}".to_string(),
        );
        assert_eq!(lines.len(), 2); // main row + status sub-line
        let main_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        // Selected + last: uses ╘═ (U+2558 U+2550) double-line tree char
        assert!(main_text.contains("\u{2558}\u{2550}"), "Selected tree char ╘═ missing");
        assert!(main_text.contains("test-agent"), "Agent name missing");
    }

    #[test]
    fn verify_agent_progress_badge_has_bg() {
        let lines = render_agent_progress_line(
            "agent", Color::Magenta, true, false, false,
            0, 0, "Working\u{2026}".to_string(),
        );
        let badge_span = lines[0].spans.iter().find(|s| s.content.contains("agent"));
        assert!(badge_span.is_some(), "Badge span missing");
        assert_eq!(badge_span.unwrap().style.fg, Some(Color::Magenta));
        assert!(badge_span.unwrap().style.add_modifier.contains(Modifier::BOLD));
    }
}
