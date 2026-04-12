use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Static hints shown when keyboard enhancement IS available.
const HINTS_ENHANCED: &[(&str, &str)] = &[
    ("Tab", "complete"),
    ("Shift+↵", "newline"),
    ("↑↓", "history"),
    ("Ctrl+V", "paste image"),
    ("Ctrl+O", "thinking"),
    ("Ctrl+C", "abort/quit"),
];

/// Static hints shown when keyboard enhancement is NOT available.
const HINTS_BASIC: &[(&str, &str)] = &[
    ("Tab", "complete"),
    ("Ctrl+J", "newline"),
    ("↑↓", "history"),
    ("Ctrl+V", "paste image"),
    ("Ctrl+O", "thinking"),
    ("Ctrl+C", "abort/quit"),
];

pub fn render(frame: &mut Frame, area: Rect, enhanced_keys: bool) {
    let dim = Style::default().fg(Color::DarkGray);
    let key_style = Style::default().fg(Color::Gray);
    let hints = if enhanced_keys { HINTS_ENHANCED } else { HINTS_BASIC };
    let mut spans = Vec::new();
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", dim));
        }
        spans.push(Span::styled((*key).to_string(), key_style));
        spans.push(Span::styled(format!(": {desc}"), dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
