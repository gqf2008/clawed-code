//! BashModeProgress top-level panel (aligned with CC ShellProgressMessage).
//!
//! When one or more bash/shell commands are running, this panel appears
//! above the input separator, showing the most recent command and its
//! live output lines. It stays visible even when the user scrolls up in
//! the message history.
//!
//! NOTE: Currently shows only a single shell. Concurrent shells are handled
//! by keeping the most recently started one; earlier shells' output is not
//! shown in this panel (it remains in the inline message history).

use super::MUTED;
use std::collections::VecDeque;
use std::time::Instant;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

const MAX_OUTPUT_LINES: usize = 5;
const BORDER_CHAR: &str = "\u{2504}"; // ┄

/// State for the BashModeProgress panel.
pub struct BashModeState {
    /// The shell command being executed (e.g. "cargo test").
    command: Option<String>,
    /// Live output lines captured from the shell.
    output_lines: VecDeque<String>,
    /// When the current shell started (for elapsed display).
    started: Option<Instant>,
    /// Name of the tool that owns this panel (to disambiguate concurrent shells).
    tool_name: Option<String>,
}

impl BashModeState {
    pub fn new() -> Self {
        Self {
            command: None,
            output_lines: VecDeque::with_capacity(MAX_OUTPUT_LINES),
            started: None,
            tool_name: None,
        }
    }

    /// Start tracking a new bash command.
    /// If the panel is already tracking a different shell, the new shell is
    /// ignored (concurrent shells are not shown in this single-shell panel).
    pub fn start(&mut self, tool_name: String, command: String) {
        if let Some(ref current) = self.tool_name {
            if current != &tool_name && self.command.is_some() {
                // Already showing a different shell; keep it.
                return;
            }
        }
        self.tool_name = Some(tool_name);
        self.command = Some(command);
        self.output_lines.clear();
        self.started = Some(Instant::now());
    }

    /// Append an output line from the running shell.
    /// Lines from a mismatched tool name are ignored.
    pub fn add_line(&mut self, tool_name: &str, line: &str) {
        if self.command.is_none() {
            return;
        }
        if let Some(ref current) = self.tool_name {
            if !tool_name.is_empty() && tool_name != current {
                return;
            }
        }
        if self.output_lines.len() == MAX_OUTPUT_LINES {
            self.output_lines.pop_front();
        }
        self.output_lines.push_back(line.to_string());
    }

    /// Hide the panel (called when the shell completes).
    pub fn end(&mut self) {
        self.command = None;
        self.output_lines.clear();
        self.started = None;
        self.tool_name = None;
    }

    /// Height needed for rendering (0 when hidden).
    pub fn render_height(&self) -> u16 {
        if self.command.is_none() {
            return 0;
        }
        // Border + command line + output lines + bottom border
        let output_count = self.output_lines.len().max(1);
        (2 + output_count) as u16
    }
}

/// Render the BashModeProgress panel.
pub fn render(frame: &mut Frame, area: Rect, state: &BashModeState) {
    if state.command.is_none() || area.height == 0 {
        return;
    }

    let dim = Style::default().fg(MUTED);
    let cmd_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let output_style = Style::default().fg(Color::Gray);

    let mut lines: Vec<Line> = Vec::new();

    // Top border
    let border = BORDER_CHAR.repeat(area.width as usize);
    lines.push(Line::styled(border.clone(), dim));

    // Command line: "▸ bash (cargo test)"
    let elapsed = state.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
    let elapsed_str = super::overlay::format_elapsed(elapsed);
    let cmd = state
        .command
        .as_ref()
        .expect("render returns early when command is None");
    lines.push(Line::from(vec![
        Span::styled("\u{25B8} ", dim),
        Span::styled("bash ", cmd_style),
        Span::styled(format!("({cmd})"), dim),
        Span::styled(format!("  \u{00B7} {elapsed_str}"), dim),
    ]));

    // Output lines
    if state.output_lines.is_empty() {
        lines.push(Line::styled("  Running\u{2026}", dim));
    } else {
        for line in &state.output_lines {
            lines.push(Line::styled(format!("  {line}"), output_style));
        }
    }

    // Bottom border (drawn only if we have room)
    if lines.len() < area.height as usize {
        lines.push(Line::styled(border, dim));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}
