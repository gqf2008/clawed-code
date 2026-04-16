//! Dynamic status line for the TUI (ratatui version).

use super::MUTED;
use std::collections::HashMap;
use std::time::Instant;

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

pub struct ToolInfo {
    pub name: String,
    pub started: Instant,
}

pub struct TuiStatusState {
    pub active_tools: HashMap<String, ToolInfo>,
    pub active_shells: u32,
    pub active_agents: HashMap<String, String>,
    pub thinking: bool,
    /// True for the entire duration the agent is generating (set in mark_generating,
    /// cleared in mark_done). Broader than `thinking`, which goes false during TextDelta.
    pub is_generating: bool,
    pub session_start: Instant,
    /// Spinner frame counter, incremented on each render tick while generating.
    pub spinner_frame: usize,
    /// Context window usage percentage (0.0–100.0), updated from SessionStatus.
    pub context_pct: f64,
}

impl TuiStatusState {
    pub fn new() -> Self {
        Self {
            active_tools: HashMap::new(),
            active_shells: 0,
            active_agents: HashMap::new(),
            thinking: false,
            is_generating: false,
            session_start: Instant::now(),
            spinner_frame: 0,
            context_pct: 0.0,
        }
    }

    /// Whether the status line should be visible.
    pub fn should_show(&self) -> bool {
        self.is_generating
            || !self.active_tools.is_empty()
            || self.active_shells > 0
            || !self.active_agents.is_empty()
    }
}

/// Build the dynamic status spans (elapsed, spinner, tools, shells, agents).
/// Returns an empty vec when there is nothing to show.
pub fn build_spans(state: &TuiStatusState) -> Vec<Span<'static>> {
    if !state.should_show() {
        return Vec::new();
    }

    let dim = Style::default().fg(MUTED);
    let warn = Style::default().fg(Color::Yellow);
    let tool_style = Style::default().fg(Color::Blue);

    let elapsed = state.session_start.elapsed();
    let mins = elapsed.as_secs() / 60;
    let secs = elapsed.as_secs() % 60;

    let mut spans: Vec<Span<'static>> = Vec::new();

    // Show spinner first whenever generating — label changes based on phase:
    //   "thinking"   — extended thinking or between submit and first TextDelta
    //   "running"    — tool is running
    //   "generating" — streaming text back, or waiting for next response after tools
    if state.is_generating {
        const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let ch = SPINNER[state.spinner_frame % SPINNER.len()];
        let label = if state.thinking {
            "Thinking"
        } else if !state.active_tools.is_empty() {
            "Running"
        } else {
            "Generating"
        };
        spans.push(Span::styled(format!("{ch} {label}"), tool_style));
    }

    // Elapsed time after spinner
    spans.push(Span::raw("  "));
    spans.push(Span::styled(format!("{mins:02}:{secs:02}"), dim));

    let tool_count = state.active_tools.len();
    if tool_count > 0 {
        if let Some(tool) = state.active_tools.values().next() {
            let te = tool.started.elapsed();
            let suffix = if tool_count > 1 {
                format!(" +{}", tool_count - 1)
            } else {
                String::new()
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!(
                    "{} ({:02}:{:02}){suffix}",
                    tool.name,
                    te.as_secs() / 60,
                    te.as_secs() % 60
                ),
                warn,
            ));
        }
    }

    if state.active_shells > 0 {
        let s = if state.active_shells == 1 { "" } else { "s" };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("{} shell{s}", state.active_shells),
            tool_style,
        ));
    }

    let agent_count = state.active_agents.len();
    if agent_count > 0 {
        let s = if agent_count == 1 { "" } else { "s" };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("{agent_count} agent{s}"), warn));
    }

    spans
}

/// Render the status bar into the given area (standalone, for fallback use).
pub fn render(frame: &mut Frame, area: Rect, state: &TuiStatusState) {
    let spans = build_spans(state);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_show_empty() {
        let state = TuiStatusState::new();
        assert!(!state.should_show());
    }

    #[test]
    fn test_should_show_generating() {
        let mut state = TuiStatusState::new();
        state.is_generating = true;
        assert!(state.should_show());
    }

    #[test]
    fn test_should_show_tools() {
        let mut state = TuiStatusState::new();
        state.active_tools.insert(
            "1".into(),
            ToolInfo {
                name: "Bash".into(),
                started: Instant::now(),
            },
        );
        assert!(state.should_show());
    }
}
