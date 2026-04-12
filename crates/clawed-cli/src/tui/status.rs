//! Dynamic status line for the TUI (ratatui version).

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
    pub session_start: Instant,
    /// Spinner frame counter, incremented on each render tick while thinking.
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
            session_start: Instant::now(),
            spinner_frame: 0,
            context_pct: 0.0,
        }
    }

    /// Whether the status line should be visible.
    pub fn should_show(&self) -> bool {
        !self.active_tools.is_empty()
            || self.active_shells > 0
            || !self.active_agents.is_empty()
            || self.thinking
    }
}

/// Render the status bar into the given area.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &TuiStatusState,
    model: &str,
    total_input_tokens: u64,
    total_output_tokens: u64,
) {
    let dim = Style::default().fg(Color::DarkGray);
    let warn = Style::default().fg(Color::Yellow);
    let tool_style = Style::default().fg(Color::Blue);

    let elapsed = state.session_start.elapsed();
    let mins = elapsed.as_secs() / 60;
    let secs = elapsed.as_secs() % 60;

    let mut spans: Vec<Span> = vec![Span::styled(format!("{mins:02}:{secs:02}"), dim)];

    if state.thinking {
        const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let ch = SPINNER[state.spinner_frame % SPINNER.len()];
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("{ch} thinking"), tool_style));
    }

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
                format!("{} ({:02}:{:02}){suffix}", tool.name, te.as_secs() / 60, te.as_secs() % 60),
                warn,
            ));
        }
    }

    if state.active_shells > 0 {
        let s = if state.active_shells == 1 { "" } else { "s" };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("{} shell{s}", state.active_shells), tool_style));
    }

    let agent_count = state.active_agents.len();
    if agent_count > 0 {
        let s = if agent_count == 1 { "" } else { "s" };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("{agent_count} agent{s}"), warn));
    }

    // Token usage
    let total_tokens = total_input_tokens + total_output_tokens;
    if total_tokens > 0 {
        spans.push(Span::raw("  "));
        let token_text = if total_tokens >= 1000 {
            format!("{:.1}k tokens", total_tokens as f64 / 1000.0)
        } else {
            format!("{total_tokens} tokens")
        };
        spans.push(Span::styled(token_text, dim));
    }

    // Context window usage
    if state.context_pct > 0.0 {
        spans.push(Span::raw("  "));
        let ctx_style = if state.context_pct >= 80.0 {
            warn
        } else {
            dim
        };
        spans.push(Span::styled(format!("{:.0}% ctx", state.context_pct), ctx_style));
    }

    // Model info on the right
    spans.push(Span::raw("  "));
    spans.push(Span::styled(model.to_string(), dim));

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
    fn test_should_show_thinking() {
        let mut state = TuiStatusState::new();
        state.thinking = true;
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
