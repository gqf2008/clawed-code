//! Dynamic status line for the TUI (ratatui version).
//! Aligned with official Claude Code SpinnerAnimationRow.

use super::verbs::{self, SHIMMER_INTERVAL_REQUESTING_MS, SHIMMER_INTERVAL_THINKING_MS, SPINNER_TICK_INTERVAL_MS};
use super::MUTED;
use std::collections::HashMap;
use std::time::Instant;

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

/// Official CC platform-specific spinner characters.
/// Bounce oscillation: 6 forward + 6 reverse - 2 overlap = 10 frames.
const SPINNER: &[&str] = &["·", "✢", "✳", "✶", "✻", "✽"];
const BOUNCE_LEN: usize = SPINNER.len() * 2 - 2;

/// ERROR_RED from official Claude Code: rgb(171, 43, 63).
const ERROR_RED: (u8, u8, u8) = (171, 43, 63);

/// Stall threshold (ms) before color interpolation begins.
const STALL_THRESHOLD_MS: u128 = 3000;
/// Duration (ms) over which stall color fades to ERROR_RED.
const STALL_FADE_MS: u128 = 2000;

/// Minimum delay (ms) before showing "thought for Ns".
const THOUGHT_DISPLAY_DELAY_MS: u64 = 3000;
/// Minimum duration (ms) that "thought for Ns" remains visible.
const THOUGHT_DISPLAY_MIN_MS: u64 = 2000;

pub struct ToolInfo {
    pub name: String,
    pub started: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AgentState {
    Active,
    Idle,
    Stopping,
    AwaitingApproval,
}

pub struct AgentInfo {
    pub name: String,
    pub started: Instant,
    #[allow(dead_code)]
    pub state: AgentState,
    /// Current activity description (last tool call, etc.).
    pub activity: Option<String>,
    /// Number of tool uses completed by this agent.
    #[allow(dead_code)]
    pub tool_count: u32,
    /// Estimated token usage by this agent.
    #[allow(dead_code)]
    pub token_estimate: u64,
    /// When the agent became idle (for "Idle for X" display).
    #[allow(dead_code)]
    pub idle_since: Option<Instant>,
    /// Agent identity color (stable hash-based assignment).
    pub color: Color,
}

/// Return active agents sorted by name (stable ordering for UI selection).
pub fn sorted_agent_entries(
    agents: &HashMap<String, AgentInfo>,
) -> Vec<(&String, &AgentInfo)> {
    let mut sorted: Vec<(&String, &AgentInfo)> = agents.iter().collect();
    sorted.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    sorted
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

    // --- Token counter (smooth increment) ---
    /// Cumulative character count of the response (used to estimate tokens).
    pub response_char_count: usize,
    /// Currently displayed token estimate (smoothly incremented toward actual).
    pub displayed_token_estimate: u32,

    // --- "Thought for Ns" ---
    /// When the current thinking block started.
    pub thinking_start: Option<Instant>,
    /// Total accumulated thinking time across all thinking blocks this turn (ms).
    pub total_thinking_ms: u64,
    /// Duration of the most recent completed thinking block (ms).
    pub last_thinking_elapsed_ms: u64,
    /// When the thinking block ended (for display timing).
    pub thinking_end: Option<Instant>,

    // --- Remote status ---
    /// Bridge gateway platforms (e.g. ["lark", "telegram"]). Empty = no bridge.
    pub bridge_platforms: Vec<String>,
    /// Bridge active session count.
    pub bridge_sessions: usize,
    /// Teleport / CCR remote active flag.
    pub teleport_remote: bool,
    /// Teleport environment name (if connected).
    pub teleport_env: Option<String>,
    /// Voice input/output state.
    pub voice_state: Option<clawed_bus::events::VoiceState>,
    /// Detected IDE environment (e.g. "vscode", "jetbrains").
    pub ide_kind: Option<String>,
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
            response_char_count: 0,
            displayed_token_estimate: 0,
            thinking_start: None,
            total_thinking_ms: 0,
            last_thinking_elapsed_ms: 0,
            thinking_end: None,
            bridge_platforms: Vec::new(),
            bridge_sessions: 0,
            teleport_remote: false,
            teleport_env: None,
            voice_state: None,
            ide_kind: None,
        }
    }

    pub fn record_token(&mut self) {
        self.last_token_time = Some(Instant::now());
    }

    /// Accumulate response characters for token counter estimation.
    pub fn add_response_chars(&mut self, n: usize) {
        self.response_char_count += n;
    }

    /// Start tracking a thinking block.
    pub fn start_thinking(&mut self) {
        if self.thinking_start.is_none() {
            self.thinking_start = Some(Instant::now());
        }
    }

    /// Stop the current thinking block and accumulate its duration.
    pub fn stop_thinking(&mut self) {
        if let Some(start) = self.thinking_start.take() {
            let elapsed = start.elapsed().as_millis() as u64;
            self.last_thinking_elapsed_ms = elapsed;
            self.total_thinking_ms += elapsed;
            self.thinking_end = Some(Instant::now());
        }
    }

    /// Smoothly advance the displayed token estimate toward the actual value.
    /// Called on each spinner tick (120ms).
    pub fn update_token_counter(&mut self) {
        let actual = (self.response_char_count / 4) as u32;
        let gap = actual.saturating_sub(self.displayed_token_estimate);
        if gap == 0 {
            return;
        }
        let increment = if gap < 70 {
            3
        } else if gap < 200 {
            ((gap as f64 * 0.15).ceil() as u32).max(8)
        } else {
            50
        };
        // Scale increment by tick interval relative to official 50ms base.
        let scaled = ((increment as u64 * SPINNER_TICK_INTERVAL_MS) / 50).max(1) as u32;
        self.displayed_token_estimate = (self.displayed_token_estimate + scaled).min(actual);
    }

    pub fn should_show(&self) -> bool {
        self.is_generating
            || !self.active_tools.is_empty()
            || self.active_shells > 0
            || !self.active_agents.is_empty()
    }

    /// Whether a tip line should be rendered below the status bar.
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

    /// Compute the stall intensity [0.0, 1.0] based on time since last token.
    fn stall_intensity(&self) -> f64 {
        let time_since_token = self
            .generating_since
            .and_then(|since| {
                self.last_token_time
                    .map(|t| t.elapsed().as_millis())
                    .or_else(|| Some(since.elapsed().as_millis()))
            })
            .unwrap_or(0);
        if time_since_token <= STALL_THRESHOLD_MS {
            0.0
        } else {
            ((time_since_token as f64 - STALL_THRESHOLD_MS as f64) / STALL_FADE_MS as f64)
                .clamp(0.0, 1.0)
        }
    }

    /// Whether "thought for Ns" should be shown.
    fn should_show_thought_for(&self) -> bool {
        if self.last_thinking_elapsed_ms == 0 {
            return false;
        }
        let Some(end) = self.thinking_end else {
            return false;
        };
        let since_end = end.elapsed().as_millis() as u64;
        // Show after 3s delay, persist for at least 2s.
        since_end >= THOUGHT_DISPLAY_DELAY_MS
            && since_end < THOUGHT_DISPLAY_DELAY_MS + THOUGHT_DISPLAY_MIN_MS.max(self.last_thinking_elapsed_ms)
    }

    /// Format "thought for Ns" text.
    fn thought_for_text(&self) -> String {
        let secs = (self.last_thinking_elapsed_ms + 500) / 1000;
        format!("thought for {secs}s")
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

/// Interpolate a Style's foreground color toward ERROR_RED based on stall intensity.
fn stall_style(base: Style, intensity: f64) -> Style {
    if intensity <= 0.0 {
        return base;
    }
    let color = base.fg.unwrap_or(Color::Rgb(100, 149, 237));
    let (br, bg, bb) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (100, 149, 237),
    };
    let (er, eg, eb) = ERROR_RED;
    base.fg(Color::Rgb(
        lerp_u8(br, er, intensity),
        lerp_u8(bg, eg, intensity),
        lerp_u8(bb, eb, intensity),
    ))
}

/// Build the dynamic status spans (spinner, verb, tokens, tools, shells, agents).
/// Returns an empty vec when there is nothing to show.
///
/// `teammate_selection` is `Some(selected_index)` when the user is cycling
/// through active agents on the spinner row (pointer + tab/enter nav).
pub fn build_spans(
    state: &TuiStatusState,
    max_width: u16,
    teammate_selection: Option<usize>,
) -> Vec<Span<'static>> {
    if !state.should_show() {
        return Vec::new();
    }

    let dim = Style::default().fg(MUTED);
    let warn = Style::default().fg(Color::Yellow);
    let tool_style = Style::default().fg(Color::Blue);
    let width = max_width as usize;
    // Progressive width gating (CC behavior):
    // < 50: spinner + verb only. >= 50: tokens. >= 70: thought. >= 90: elapsed + tools/shells/agents.
    let show_tokens = width >= 50;
    let show_thought = width >= 70;
    let show_details = width >= 90;

    let elapsed = state.session_start.elapsed();
    let mins = elapsed.as_secs() / 60;
    let secs = elapsed.as_secs() % 60;

    let mut spans: Vec<Span<'static>> = Vec::new();

    if state.is_generating {
        // Bounce animation: forward 0→5, backward 5→0 (aligned with official CC).
        let idx = state.spinner_frame % BOUNCE_LEN;
        let ch = if idx < SPINNER.len() {
            SPINNER[idx]
        } else {
            SPINNER[BOUNCE_LEN - idx]
        };

        let intensity = state.stall_intensity();
        let spinner_style = stall_style(tool_style, intensity);

        spans.push(Span::styled(ch, spinner_style));

        // SpinnerModeGlyph: ↑ requesting, ↓ responding.
        let mode = if state.last_token_time.is_some() { "\u{2193}" } else { "\u{2191}" };
        spans.push(Span::styled(mode, spinner_style));
        spans.push(Span::raw(" "));

        // Label: verb or teammate summary / selection list.
        if !state.active_agents.is_empty() {
            let count = state.active_agents.len();
            if let Some(selected) = teammate_selection {
                // Selection mode: list agents with pointer on the selected one.
                let sorted = sorted_agent_entries(&state.active_agents);
                for (i, (_id, agent)) in sorted.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::styled(" \u{00B7} ", dim));
                    }
                    if i == selected {
                        spans.push(Span::styled(
                            format!("\u{25B8} {}", agent.name),
                            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                        ));
                    } else {
                        spans.push(Span::styled(agent.name.clone(), dim));
                    }
                }
                if count > 1 {
                    spans.push(Span::styled(
                        format!("  ({} / {})", selected + 1, count),
                        dim,
                    ));
                }
            } else {
                let label = if count == 1 {
                    let a = state.active_agents.values().next().unwrap();
                    format!("{} \u{00B7} {} running", a.name, count)
                } else {
                    format!("{} teammates \u{00B7} {} running", count, count)
                };
                spans.push(Span::styled(label, dim));
            }
        } else {
            // Shimmer: sweep a highlight window across the verb.
            let label = state.current_verb.unwrap_or(verbs::THINKING_VERB);
            let shimmer_interval = if state.last_token_time.is_some() {
                SHIMMER_INTERVAL_THINKING_MS
            } else {
                SHIMMER_INTERVAL_REQUESTING_MS
            };
            let shimmer_tick =
                ((state.spinner_frame as u64 * SPINNER_TICK_INTERVAL_MS) / shimmer_interval)
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

        // Token counter (>= 50 cols)
        if show_tokens && state.displayed_token_estimate > 0 {
            spans.push(Span::styled(
                format!(" \u{00B7} \u{2193} {} tokens", state.displayed_token_estimate),
                dim,
            ));
        }

        // "Thought for Ns" (>= 70 cols)
        if show_thought && state.should_show_thought_for() {
            spans.push(Span::styled(
                format!(" \u{00B7} {}", state.thought_for_text()),
                dim,
            ));
        }
    }

    // Elapsed + details (>= 90 cols)
    if show_details {
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
            spans.push(Span::styled(format!("{} shell{s}", state.active_shells), tool_style));
        }

        let agent_count = state.active_agents.len();
        if agent_count > 0 {
            let names: Vec<&str> = state.active_agents.values().map(|a| a.name.as_str()).collect();
            spans.push(Span::raw("  "));
            spans.push(Span::styled(format!("{}: {}", agent_count, names.join(", ")), dim));
        }

        // Bridge status
        if !state.bridge_platforms.is_empty() {
            let platforms = state.bridge_platforms.join(", ");
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("bridge: {platforms} ({})", state.bridge_sessions),
                dim,
            ));
        }

        // Teleport status
        if state.teleport_remote {
            let env = state.teleport_env.as_deref().unwrap_or("remote");
            spans.push(Span::raw("  "));
            spans.push(Span::styled(format!("teleport: {env}"), dim));
        }
    }

    // Voice indicator (shown only when not idle)
    if let Some(ref voice) = state.voice_state {
        if !matches!(voice, clawed_bus::events::VoiceState::Idle) {
            let (icon, label) = match voice {
                clawed_bus::events::VoiceState::Idle => unreachable!(),
                clawed_bus::events::VoiceState::Recording => ("\u{1F534}", "recording"),
                clawed_bus::events::VoiceState::Transcribing => ("\u{23F3}", "transcribing"),
                clawed_bus::events::VoiceState::Playing => ("\u{1F50A}", "playing"),
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("{icon} {label}"),
                Style::default().fg(Color::Magenta),
            ));
        }
    }

    // IDE integration hint
    if let Some(ref ide) = state.ide_kind {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("\u{1F4BB} {ide}"),
            Style::default().fg(Color::Cyan),
        ));
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

    #[test]
    fn test_stall_intensity_zero_before_threshold() {
        let mut state = TuiStatusState::new();
        state.generating_since = Some(Instant::now());
        state.last_token_time = Some(Instant::now());
        assert_eq!(state.stall_intensity(), 0.0);
    }

    #[test]
    fn test_stall_intensity_clamped_at_one() {
        let mut state = TuiStatusState::new();
        state.generating_since = Some(Instant::now() - std::time::Duration::from_secs(10));
        state.last_token_time = Some(Instant::now() - std::time::Duration::from_secs(10));
        assert_eq!(state.stall_intensity(), 1.0);
    }

    #[test]
    fn test_token_counter_smooth_increment() {
        let mut state = TuiStatusState::new();
        state.response_char_count = 400; // actual = 100
        state.displayed_token_estimate = 0;
        state.update_token_counter();
        // gap = 100, so increment = max(8, ceil(100*0.15)) = 15, scaled by 120/50 = 36
        assert!(state.displayed_token_estimate > 0);
        assert!(state.displayed_token_estimate <= 100);
    }

    #[test]
    fn test_thought_for_not_shown_immediately() {
        let mut state = TuiStatusState::new();
        state.last_thinking_elapsed_ms = 5000;
        state.thinking_end = Some(Instant::now());
        assert!(!state.should_show_thought_for());
    }

    #[test]
    fn test_thought_for_shown_after_delay() {
        let mut state = TuiStatusState::new();
        state.last_thinking_elapsed_ms = 5000;
        state.thinking_end = Some(Instant::now() - std::time::Duration::from_secs(5));
        assert!(state.should_show_thought_for());
    }

    #[test]
    fn test_thought_for_text_format() {
        let mut state = TuiStatusState::new();
        state.last_thinking_elapsed_ms = 5500;
        assert_eq!(state.thought_for_text(), "thought for 6s");
    }

    #[test]
    fn test_stall_style_at_zero() {
        let base = Style::default().fg(Color::Rgb(100, 149, 237));
        let result = stall_style(base, 0.0);
        assert_eq!(result.fg, base.fg);
    }

    #[test]
    fn test_stall_style_at_full() {
        let base = Style::default().fg(Color::Rgb(100, 149, 237));
        let result = stall_style(base, 1.0);
        assert_eq!(result.fg, Some(Color::Rgb(171, 43, 63)));
    }
}
