//! Dynamic status line for the TUI (ratatui version).

use super::verbs::{self, SHIMMER_INTERVAL_MS, SPINNER_TICK_INTERVAL_MS};
use super::MUTED;
use std::collections::HashMap;
use std::time::Instant;

use ratatui::{
    style::{Color, Style},
    text::Span,
};

/// Spinner characters pre-rendered as &str to avoid per-frame allocation.
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const BOUNCE_LEN: usize = SPINNER.len() * 2 - 2;

pub struct ToolInfo {
    pub name: String,
    pub started: Instant,
}

pub struct AgentInfo {
    pub name: String,
    pub started: Instant,
}

pub struct TuiStatusState {
    pub active_tools: HashMap<String, ToolInfo>,
    pub active_shells: u32,
    pub active_agents: HashMap<String, AgentInfo>,
    pub thinking: bool,
    /// True for the entire duration the agent is generating (set in mark_generating,
    /// cleared in mark_done). Broader than `thinking`, which goes false during TextDelta.
    pub is_generating: bool,
    pub session_start: Instant,
    /// Spinner frame counter, incremented on each render tick while generating.
    pub spinner_frame: usize,
    /// Context window usage percentage (0.0–100.0), updated from SessionStatus.
    pub context_pct: f64,
    /// When the current generation phase started (for stall detection).
    pub generating_since: Option<Instant>,
    /// When the last token (text/thinking/tool) was received.
    /// Used with generating_since for gradual stall color interpolation.
    /// Also drives the SpinnerModeGlyph: None → ↑ (requesting),
    /// Some(_) → ↓ (responding).
    pub last_token_time: Option<Instant>,
    /// Random verb picked when generation starts, used as spinner label.
    /// Picked once per turn, aligned with official Claude Code.
    /// None until the first verb is assigned (falls back to "Thinking").
    pub current_verb: Option<&'static str>,
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
            generating_since: None,
            last_token_time: None,
            current_verb: None,
        }
    }

    pub fn record_token(&mut self) {
        self.last_token_time = Some(Instant::now());
    }

    pub fn should_show(&self) -> bool {
        self.is_generating
            || !self.active_tools.is_empty()
            || self.active_shells > 0
            || !self.active_agents.is_empty()
    }

    /// Whether a tip line should be rendered below the status bar.
    /// Checks the threshold directly to avoid redundant elapsed() computation
    /// when the caller also needs `current_tip()`.
    pub fn has_tip(&self) -> bool {
        self.generating_since
            .map(|s| s.elapsed().as_secs() >= 30)
            .unwrap_or(false)
    }

    /// Return the tip text if a threshold has been reached.
    pub fn current_tip(&self) -> Option<&'static str> {
        let elapsed = self.generating_since?.elapsed();
        let secs = elapsed.as_secs();
        if secs >= 1800 {
            Some("Tip: Use /clear to start fresh when switching topics and free up context")
        } else if secs >= 30 {
            Some("Tip: Use /btw to ask a quick side question without interrupting Claude's current work")
        } else {
            None
        }
    }
}

fn push_active_item(
    spans: &mut Vec<Span<'static>>,
    name: &str,
    elapsed: std::time::Duration,
    count: usize,
    style: Style,
) {
    let suffix = if count > 1 {
        format!(" +{}", count - 1)
    } else {
        String::new()
    };
    let mins = elapsed.as_secs() / 60;
    let secs = elapsed.as_secs() % 60;
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("{name} ({mins:02}:{secs:02}){suffix}"),
        style,
    ));
}

/// Linear interpolation between two u8 values.
fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (b as f64 - a as f64).mul_add(t, a as f64) as u8
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

    if state.is_generating {
        // Bounce animation: forward 0→9, backward 9→0 (aligned with official CC).
        let idx = state.spinner_frame % BOUNCE_LEN;
        let ch = if idx < SPINNER.len() {
            SPINNER[idx]
        } else {
            SPINNER[BOUNCE_LEN - idx]
        };
        let label = state.current_verb.unwrap_or(verbs::THINKING_VERB);
        let spinner_style = if let Some(since) = state.generating_since {
            let time_since_token = state
                .last_token_time
                .map(|t| t.elapsed().as_millis())
                .unwrap_or_else(|| since.elapsed().as_millis());
            // Intensity ramps from 0 at 3s to 1 at 5s, clamped to [0, 1].
            let intensity = ((time_since_token as f64 - 3000.0) / 2000.0).clamp(0.0, 1.0);
            if intensity > 0.0 {
                let r = lerp_u8(0, 171, intensity);
                let g = lerp_u8(0, 43, intensity);
                let b = lerp_u8(255, 63, intensity);
                Style::default().fg(Color::Rgb(r, g, b))
            } else {
                tool_style
            }
        } else {
            tool_style
        };
        spans.push(Span::styled(ch, spinner_style));
        // SpinnerModeGlyph: ↑ requesting, ↓ responding (aligned with official CC).
        let mode = if state.last_token_time.is_some() { "\u{2193}" } else { "\u{2191}" };
        spans.push(Span::styled(mode, spinner_style));
        spans.push(Span::raw(" "));
        // Shimmer: sweep a highlight window across the verb.
        let shimmer_tick =
            ((state.spinner_frame as u64 * SPINNER_TICK_INTERVAL_MS) / SHIMMER_INTERVAL_MS)
                as usize;
        let (before, shimmer, after) =
            verbs::compute_shimmer_segments(label, shimmer_tick);
        if !before.is_empty() {
            spans.push(Span::styled(before, dim));
        }
        if !shimmer.is_empty() {
            spans.push(Span::styled(shimmer, spinner_style));
        }
        if !after.is_empty() {
            spans.push(Span::styled(after, dim));
        }
    }

    // Elapsed time after spinner
    spans.push(Span::raw("  "));
    spans.push(Span::styled(format!("{mins:02}:{secs:02}"), dim));

    let tool_count = state.active_tools.len();
    if tool_count > 0 {
        if let Some(tool) = state.active_tools.values().next() {
            push_active_item(&mut spans, &tool.name, tool.started.elapsed(), tool_count, warn);
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
        if let Some(agent) = state.active_agents.values().next() {
            push_active_item(&mut spans, &agent.name, agent.started.elapsed(), agent_count, warn);
        }
    }

    spans
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
