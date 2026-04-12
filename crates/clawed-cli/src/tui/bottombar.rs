use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

const HINTS: &[(&str, &str)] = &[
    ("Tab", "complete"),
    ("Shift+\u{21B5}", "newline"),
    ("\u{2191}\u{2193}", "history"),
    ("Ctrl+O", "thinking"),
    ("Ctrl+C", "abort/quit"),
    ("Ctrl+L", "clear"),
];

pub fn render(frame: &mut Frame, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let key_style = Style::default().fg(Color::Gray);
    let mut spans = Vec::new();
    for (i, (key, desc)) in HINTS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", dim));
        }
        spans.push(Span::styled((*key).to_string(), key_style));
        spans.push(Span::styled(format!(": {desc}"), dim));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
