use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Static hints for the bottom bar.
const HINTS: &[(&str, &str)] = &[
    ("Tab", "complete"),
    ("Ctrl+J/N", "newline"),
    ("↑↓", "history"),
    ("Ctrl+V", "paste image"),
    ("Ctrl+O", "thinking"),
    ("Ctrl+C", "abort/quit"),
];

pub fn render(frame: &mut Frame, area: Rect) {
    let sep = Style::default();
    let key_style = Style::default().fg(Color::Cyan);
    let desc_style = Style::default();
    let mut spans = Vec::new();
    for (i, (key, desc)) in HINTS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", sep));
        }
        spans.push(Span::styled((*key).to_string(), key_style));
        spans.push(Span::styled(format!(": {desc}"), desc_style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
