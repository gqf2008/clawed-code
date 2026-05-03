use super::MUTED;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_width::UnicodeWidthStr;

/// Static hints for the bottom bar in normal (idle) mode.
const NORMAL_HINTS: &[(&str, &str)] = &[
    ("Esc", "help"),
    ("Tab", "complete"),
    ("Ctrl+J", "newline"),
    ("\u{2191}\u{2193}", "history"),
    ("Ctrl+V", "paste image"),
    ("Ctrl+O", "thinking"),
    ("Ctrl+C", "abort/quit"),
];

/// Hints shown while the LLM is generating or tools are running.
const GENERATING_HINTS: &[(&str, &str)] = &[
    ("Esc", "interrupt"),
    ("Ctrl+O", "expand"),
    ("Ctrl+E", "tool expand"),
    ("Ctrl+C", "abort"),
];

pub fn render(frame: &mut Frame, area: Rect, is_generating: bool, permission_mode: &str) {
    let hints: &[(&str, &str)] = if is_generating {
        GENERATING_HINTS
    } else {
        NORMAL_HINTS
    };

    // Build left side: keyboard shortcut hints
    let sep = Style::default().fg(MUTED);
    let key_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(MUTED);
    let mut left_spans = Vec::new();
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            left_spans.push(Span::styled("  ", sep));
        }
        left_spans.push(Span::styled((*key).to_string(), key_style));
        left_spans.push(Span::styled(format!("  {desc}"), desc_style));
    }

    // Build right side: permission mode indicator (aligned with official CC footer)
    let mut right_spans = Vec::new();
    if !is_generating && !permission_mode.is_empty() && permission_mode != "default" {
        let mode_color = permission_mode_color(permission_mode);
        right_spans.push(Span::styled(
            format!("{} ", permission_mode_symbol(permission_mode)),
            Style::default().fg(mode_color),
        ));
        right_spans.push(Span::styled(
            format!("{} on", permission_mode.to_lowercase()),
            Style::default().fg(mode_color),
        ));
    }

    let right_width = right_spans.iter().map(|s| s.content.width()).sum::<usize>() as u16;

    if right_width > 0 && right_width < area.width {
        let chunks = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(right_width),
        ])
        .split(area);
        frame.render_widget(Paragraph::new(Line::from(left_spans)), chunks[0]);
        frame.render_widget(Paragraph::new(Line::from(right_spans)), chunks[1]);
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
        "bypass" => "\u{2713}",      // ✓
        "auto" => "\u{2713}",       // ✓
        "acceptEdits" => "\u{270E}", // ✎
        "plan" => "\u{25B6}",       // ▶
        "dontAsk" => "\u{26A0}",    // ⚠
        _ => "",
    }
}
