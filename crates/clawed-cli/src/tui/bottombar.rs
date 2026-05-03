use super::MUTED;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Static hints for the bottom bar in normal (idle) mode.
const NORMAL_HINTS: &[(&str, &str)] = &[
    ("Enter", "submit"),
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

    let sep = Style::default().fg(MUTED);
    let key_style = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(MUTED);
    let mut spans = Vec::new();
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", sep));
        }
        spans.push(Span::styled((*key).to_string(), key_style));
        spans.push(Span::styled(format!("  {desc}"), desc_style));
    }

    // Show permission mode when not generating and not default.
    if !is_generating && !permission_mode.is_empty() && permission_mode != "default" {
        let mode_symbol = match permission_mode {
            "auto" => "\u{2713} auto on",        // ✓ auto on
            "acceptEdits" => "\u{2713} accept edits", // ✓ accept edits
            "bypass" => "\u{2713} bypass",        // ✓ bypass
            "plan" => "\u{2713} plan",            // ✓ plan
            "dontAsk" => "\u{2713} don't ask",    // ✓ don't ask
            other => other,
        };
        spans.push(Span::styled("  ", sep));
        spans.push(Span::styled(
            format!("{mode_symbol} (shift+tab: cycle)"),
            Style::default().fg(Color::Yellow),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
