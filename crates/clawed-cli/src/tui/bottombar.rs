use super::MUTED;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

/// Single-line status bar shown at the bottom of the TUI.
///
/// Render agent pills (aligned with CC BackgroundTaskStatus).
/// Horizontal scrollable pills showing @agentName with status color.
#[allow(dead_code)]
pub fn render_agent_pills(
    frame: &mut Frame,
    area: Rect,
    agents: &HashMap<String, crate::tui::status::AgentInfo>,
) {
    if area.height == 0 || area.width == 0 || agents.is_empty() {
        return;
    }
    let dim = Style::default().fg(MUTED);
    let mut spans: Vec<Span> = Vec::new();
    for (i, (_id, agent)) in agents.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let badge_style = Style::default()
            .fg(agent.color)
            .add_modifier(Modifier::BOLD);
        let name_span = Span::styled(format!("@{}", agent.name), badge_style);
        spans.push(name_span);

        match agent.state {
            crate::tui::status::AgentState::Active => {
                if let Some(ref act) = agent.activity {
                    spans.push(Span::styled(format!(" {}", act), dim));
                }
            }
            crate::tui::status::AgentState::Idle => {
                spans.push(Span::styled(" idle", dim));
            }
            crate::tui::status::AgentState::Stopping => {
                spans.push(Span::styled(" stopping\u{2026}", dim));
            }
            crate::tui::status::AgentState::AwaitingApproval => {
                spans.push(Span::styled(" \u{26A0} approval", dim));
            }
        }
    }
    spans.push(Span::styled("  \u{2191}\u{2193} view", dim));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn render(frame: &mut Frame, area: Rect, is_generating: bool, permission_mode: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let dim = Style::default().fg(MUTED);
    let key_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let mut left_spans: Vec<Span> = Vec::new();

    if is_generating {
        left_spans.push(Span::styled("Esc", key_style));
        left_spans.push(Span::styled(" interrupt  ", dim));
        left_spans.push(Span::styled("Ctrl+O", key_style));
        left_spans.push(Span::styled(" expand  ", dim));
        left_spans.push(Span::styled("Ctrl+E", key_style));
        left_spans.push(Span::styled(" tool expand  ", dim));
        left_spans.push(Span::styled("Ctrl+C", key_style));
        left_spans.push(Span::styled(" abort", dim));
    } else {
        // Permission mode indicator (primary left content in idle state)
        if !permission_mode.is_empty() && permission_mode != "default" {
            let mode_color = permission_mode_color(permission_mode);
            left_spans.push(Span::styled(
                format!("{} ", permission_mode_symbol(permission_mode)),
                Style::default().fg(mode_color),
            ));
            left_spans.push(Span::styled(
                format!("{} permissions on", permission_mode.to_lowercase()),
                Style::default().fg(mode_color),
            ));
            left_spans.push(Span::styled(" (shift+tab) · ", dim));
        }
        left_spans.push(Span::styled("ctrl+t", key_style));
        left_spans.push(Span::styled(" to hide tasks  ", dim));
        left_spans.push(Span::styled("ctrl+p", key_style));
        left_spans.push(Span::styled(" model", dim));
    }

    let left_width = left_spans.iter().map(|s| s.content.width()).sum::<usize>() as u16;

    if left_width > 0 && left_width < area.width {
        let chunks = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(area.width.saturating_sub(left_width)),
        ])
        .split(area);
        frame.render_widget(Paragraph::new(Line::from(left_spans)), chunks[0]);
    } else {
        frame.render_widget(Paragraph::new(Line::from(left_spans)), area);
    }
}

/// Color for each permission mode (aligned with official CC theme).
fn permission_mode_color(mode: &str) -> Color {
    match mode {
        "bypass" => Color::Green,
        "auto" => Color::Green,
        "acceptEdits" => Color::Yellow,
        "plan" => Color::Blue,
        "dontAsk" => Color::Red,
        _ => Color::Yellow,
    }
}

/// Unicode symbol for each permission mode (aligned with official CC).
fn permission_mode_symbol(mode: &str) -> &'static str {
    match mode {
        "bypass" => "\u{25B8}",      // ▸
        "auto" => "\u{25B8}",        // ▸
        "acceptEdits" => "\u{270E}", // ✎
        "plan" => "\u{25B6}",        // ▶
        "dontAsk" => "\u{26A0}",     // ⚠
        _ => "",
    }
}
