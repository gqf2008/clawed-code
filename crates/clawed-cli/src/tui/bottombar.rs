use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Static hints for the bottom bar in normal (idle) mode.
const NORMAL_HINTS: &[(&str, &str)] = &[
    ("Tab", "complete"),
    ("Ctrl+J/N", "newline"),
    ("↑↓", "history"),
    ("Ctrl+V", "paste image"),
    ("Ctrl+O", "thinking"),
    ("Ctrl+C", "abort/quit"),
];

/// Hints shown while the LLM is generating or tools are running.
const GENERATING_HINTS: &[(&str, &str)] = &[
    ("Esc", "interrupt"),
    ("Ctrl+O", "expand/collapse"),
    ("Ctrl+C", "abort"),
];

pub fn render(frame: &mut Frame, area: Rect, is_generating: bool, permission_mode: &str) {
    let hints: &[(&str, &str)] = if is_generating {
        GENERATING_HINTS
    } else {
        NORMAL_HINTS
    };

    let sep = Style::default();
    let key_style = Style::default().fg(Color::Cyan);
    let desc_style = Style::default();
    let mut spans = Vec::new();
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", sep));
        }
        spans.push(Span::styled((*key).to_string(), key_style));
        spans.push(Span::styled(format!(": {desc}"), desc_style));
    }

    // Show permission mode when not generating and not default.
    if !is_generating && !permission_mode.is_empty() && permission_mode != "default" {
        spans.push(Span::styled(" │ ", sep));
        spans.push(Span::styled(
            format!("permissions: {permission_mode}"),
            Style::default().fg(Color::Yellow),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
