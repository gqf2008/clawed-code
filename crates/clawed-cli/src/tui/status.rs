//! Dynamic status line for the TUI (ratatui version).

use super::verbs::{self, SPINNER_TICK_INTERVAL_MS};
use super::MUTED;
use std::collections::HashMap;
use std::time::Instant;

use ratatui::{
    style::{Color, Style},
    text::Span,
};

/// Decorative spinner characters.
/// Default set (macOS): · ✢ ✳ ✶ ✻ ✽
const SPINNER: &[&str] = &["·", "✢", "✳", "✶", "✻", "✽"];
/// Bounce: forward 0..5 then reverse 5..0 = 12 unique positions (6 + 6).
const BOUNCE_LEN: usize = SPINNER.len() * 2;

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
    /// When the current thinking phase started. Set when thinking becomes true.
    pub thinking_since: Option<Instant>,
    /// When to stop showing "thought for Ns" after thinking ends.
    /// Minimum 2s display.
    pub thought_display_until: Option<Instant>,
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
    /// Also drives the SpinnerModeGlyph: None → ↑ (requesting),
    /// Some(_) → ↓ (responding).
    pub last_token_time: Option<Instant>,
    /// Smoothed stall intensity [0.0–1.0], using exponential smoothing (diff * 0.1)
    /// Exponential smoothing for stall intensity. Reset on token arrival.
    pub smoothed_stall: f64,
    /// Random verb picked when generation starts, used as spinner label.
    /// Picked once per turn.
    /// None until the first verb is assigned (falls back to "Thinking").
    pub current_verb: Option<&'static str>,
    /// Current response length in tokens from the API.
    /// Updated from AgentNotification, drives the token counter display.
    pub response_length: u64,
    /// Smoothly interpolated display value for the token counter.
    /// Smoothly interpolated display value for the token counter.
    pub displayed_tokens: u64,
}

impl TuiStatusState {
    pub fn new() -> Self {
        Self {
            active_tools: HashMap::new(),
            active_shells: 0,
            active_agents: HashMap::new(),
            thinking: false,
            thinking_since: None,
            thought_display_until: None,
            is_generating: false,
            session_start: Instant::now(),
            spinner_frame: 0,
            context_pct: 0.0,
            generating_since: None,
            last_token_time: None,
            smoothed_stall: 0.0,
            current_verb: None,
            response_length: 0,
            displayed_tokens: 0,
        }
    }

    pub fn record_token(&mut self) {
        self.last_token_time = Some(Instant::now());
        self.smoothed_stall = 0.0;
    }

    /// Update smoothed stall intensity using exponential smoothing.
    /// Call once per render tick.
    pub fn tick_stall(&mut self) {
        let now = Instant::now();

        // Short-circuit when nothing is generating and stall is already zero.
        if self.generating_since.is_none() && self.smoothed_stall == 0.0 {
            // Still need to clear expired thought display.
            if let Some(until) = self.thought_display_until {
                if until <= now {
                    self.thought_display_until = None;
                    self.thinking_since = None;
                }
            }
            return;
        }

        let raw = if let Some(since) = self.generating_since {
            let time_since_token = self
                .last_token_time
                .map(|t| t.elapsed().as_millis())
                .unwrap_or_else(|| since.elapsed().as_millis());
            ((time_since_token as f64 - 3000.0) / 2000.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.smoothed_stall += (raw - self.smoothed_stall) * 0.1;

        // Clear expired "thought for Ns" display.
        if let Some(until) = self.thought_display_until {
            if until <= now {
                self.thought_display_until = None;
                self.thinking_since = None;
            }
        }

        // Smooth token counter increment.
        if self.response_length > 0 {
            let gap = self.response_length.saturating_sub(self.displayed_tokens);
            if gap > 0 {
                let increment = if gap < 70 {
                    3
                } else if gap < 200 {
                    (gap as f64 * 0.15).ceil() as u64
                } else {
                    50
                };
                self.displayed_tokens = self
                    .displayed_tokens
                    .saturating_add(increment)
                    .min(self.response_length);
            }
        }
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

/// Tool display expiry: fade out after 30 seconds.
const TOOL_DISPLAY_EXPIRY_SECS: u64 = 30;

fn push_active_item(
    spans: &mut Vec<Span<'static>>,
    name: &str,
    elapsed: std::time::Duration,
    count: usize,
    style: Style,
    dim: Style,
) {
    // Expire tools shown > 30s.
    if elapsed.as_secs() > TOOL_DISPLAY_EXPIRY_SECS {
        return;
    }
    let suffix = if count > 1 {
        format!(" +{}", count - 1)
    } else {
        String::new()
    };
    let elapsed_str = verbs::format_duration(elapsed.as_millis() as u64);
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("{name} ({elapsed_str}){suffix}"),
        style,
    ));
    // Show dim expiry countdown hint when approaching expiry.
    let remaining = TOOL_DISPLAY_EXPIRY_SECS - elapsed.as_secs();
    if remaining <= 5 {
        spans.push(Span::styled(format!(" {}s", remaining), dim));
    }
}

/// Linear interpolation between two u8 values.
fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (b as f64 - a as f64).mul_add(t, a as f64) as u8
}

/// Build the dynamic status spans (elapsed, spinner, tools, shells, agents).
/// Returns an empty vec when there is nothing to show.
pub fn build_spans(state: &TuiStatusState, reduced_motion: bool) -> Vec<Span<'static>> {
    if !state.should_show() {
        return Vec::new();
    }

    let dim = Style::default().fg(MUTED);
    let warn = Style::default().fg(Color::Yellow);
    let tool_style = Style::default().fg(Color::Blue);

    let elapsed = state.session_start.elapsed();

    let mut spans: Vec<Span<'static>> = Vec::new();

    if state.is_generating {
        // Check if we should show "thought for Ns" after thinking ends.
        let thought_for_display = if state.thinking {
            None
        } else {
            state
                .thought_display_until
                .filter(|until| *until > Instant::now())
                .map(|_| {
                    let since = state
                        .thinking_since
                        .or(state.generating_since)
                        .unwrap_or(state.session_start);
                    let elapsed_ms = since.elapsed().as_millis() as u64;
                    format!("thought for {}", verbs::format_duration(elapsed_ms))
                })
        };

        // Determine shimmer label: "thinking" when thinking, regular verb otherwise.
        let label = if state.thinking {
            verbs::THINKING_VERB
        } else {
            state.current_verb.unwrap_or(verbs::THINKING_VERB)
        };

        if let Some(ref thought_text) = thought_for_display {
            // "thought for Ns" — simple text, no shimmer.
            if reduced_motion {
                let half_period_frames = 2500 / SPINNER_TICK_INTERVAL_MS as usize;
                let is_bright = (state.spinner_frame / half_period_frames).is_multiple_of(2);
                let dot_style = if is_bright { tool_style } else { dim };
                spans.push(Span::styled("\u{25CF}  ", dot_style));
            } else {
                let idx = state.spinner_frame % BOUNCE_LEN;
                let ch = if idx < SPINNER.len() {
                    SPINNER[idx]
                } else {
                    SPINNER[SPINNER.len() * 2 - idx]
                };
                spans.push(Span::styled(ch, tool_style));
                let mode = "\u{2193}";
                spans.push(Span::styled(mode, tool_style));
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(format!("{thought_text}\u{2026}"), dim));
        } else if reduced_motion {
            // Blinking indicator: 2s cycle (1s bright, 1s dim). Aligned with official CC.
            let half_period_frames = 2500 / SPINNER_TICK_INTERVAL_MS as usize;
            let is_bright = (state.spinner_frame / half_period_frames).is_multiple_of(2);
            let dot_style = if is_bright { tool_style } else { dim };
            spans.push(Span::styled("\u{25CF}  ", dot_style));
            spans.push(Span::styled(format!("{label}\u{2026}"), dim));
        } else {
            // Bounce animation: forward 0→5, reverse 5→0.
            let idx = state.spinner_frame % BOUNCE_LEN;
            let ch = if idx < SPINNER.len() {
                SPINNER[idx]
            } else {
                SPINNER[SPINNER.len() * 2 - idx]
            };
            let spinner_style = if state.smoothed_stall > 0.0 {
                let intensity = state.smoothed_stall;
                let r = lerp_u8(0, 171, intensity);
                let g = lerp_u8(0, 43, intensity);
                let b = lerp_u8(255, 63, intensity);
                Style::default().fg(Color::Rgb(r, g, b))
            } else {
                tool_style
            };
            spans.push(Span::styled(ch, spinner_style));
            // SpinnerModeGlyph: ↑ requesting, ↓ responding (dim).
            let mode = if state.last_token_time.is_some() { "\u{2193}" } else { "\u{2191}" };
            spans.push(Span::styled(mode, dim));
            spans.push(Span::raw(" "));
            // Shimmer: sweep a highlight window across the verb.
            // Requesting (no token yet) = forward sweep (50ms/step); Responding = reverse sweep (200ms/step).
            let (direction, shimmer_ms) = if state.last_token_time.is_some() {
                (verbs::ShimmerDirection::Responding, verbs::SHIMMER_RESPONDING_MS)
            } else {
                (verbs::ShimmerDirection::Requesting, verbs::SHIMMER_REQUESTING_MS)
            };
            let shimmer_tick =
                ((state.spinner_frame as u64 * SPINNER_TICK_INTERVAL_MS) / shimmer_ms) as usize;
            let (before, shimmer, after) =
                verbs::compute_shimmer_segments(label, shimmer_tick, direction);
            if !before.is_empty() {
                spans.push(Span::styled(before, dim));
            }
            if !shimmer.is_empty() {
                spans.push(Span::styled(shimmer, spinner_style));
            }
            if !after.is_empty() {
                spans.push(Span::styled(after, dim));
            }
            // Ellipsis after verb.
            spans.push(Span::styled("\u{2026}", dim));
        }
    }

    // Elapsed time after spinner.
    let elapsed_ms = elapsed.as_millis() as u64;
    spans.push(Span::raw("  "));
    spans.push(Span::styled(verbs::format_duration(elapsed_ms), dim));

    // Token counter: "↓ N tokens".
    if state.displayed_tokens > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled("\u{2193}", dim));
        spans.push(Span::styled(format!(" {} tokens", state.displayed_tokens), dim));
    }

    let tool_count = state.active_tools.len();
    if tool_count > 0 {
        if let Some(tool) = state.active_tools.values().next() {
            push_active_item(&mut spans, &tool.name, tool.started.elapsed(), tool_count, warn, dim);
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
            push_active_item(&mut spans, &agent.name, agent.started.elapsed(), agent_count, warn, dim);
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
