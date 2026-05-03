//! Permission prompt widget — replaces the input area when a tool needs approval.
//!
//! The footer rows adapt to terminal width. Description, buttons, and hints may
//! each take multiple wrapped rows on narrow terminals.

use super::MUTED;
use clawed_bus::events::{PermissionRequest, PermissionResponse, RiskLevel};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

/// Which button is currently highlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionChoice {
    Allow,
    Deny,
    AllowAlways,
}

impl PermissionChoice {
    /// Cycle to next choice (wrapping).
    pub fn next(self) -> Self {
        match self {
            Self::Allow => Self::Deny,
            Self::Deny => Self::AllowAlways,
            Self::AllowAlways => Self::Allow,
        }
    }

    /// Cycle to previous choice (wrapping).
    pub fn prev(self) -> Self {
        match self {
            Self::Allow => Self::AllowAlways,
            Self::Deny => Self::Allow,
            Self::AllowAlways => Self::Deny,
        }
    }
}

/// A pending permission prompt currently displayed to the user.
pub struct PendingPermission {
    pub request: PermissionRequest,
    pub selected: PermissionChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionLayout {
    pub desc_rows: u16,
    pub detail_rows: u16,
    pub button_rows: u16,
    pub hint_rows: u16,
}

impl PermissionLayout {
    pub const fn total_rows(self) -> u16 {
        self.desc_rows + self.detail_rows + self.button_rows + self.hint_rows
    }
}

impl PendingPermission {
    pub fn new(request: PermissionRequest) -> Self {
        // Default to Allow for low risk, Deny for high risk
        let selected = match request.risk_level {
            RiskLevel::High => PermissionChoice::Deny,
            _ => PermissionChoice::Allow,
        };
        Self { request, selected }
    }

    /// Build a `PermissionResponse` from the current selection.
    pub fn to_response(&self) -> PermissionResponse {
        let (granted, remember) = match self.selected {
            PermissionChoice::Allow => (true, false),
            PermissionChoice::Deny => (false, false),
            PermissionChoice::AllowAlways => (true, true),
        };
        PermissionResponse {
            request_id: self.request.request_id.clone(),
            granted,
            remember,
            reason: None,
        }
    }

    /// Build a deny response (for Esc shortcut).
    pub fn deny_response(&self) -> PermissionResponse {
        PermissionResponse {
            request_id: self.request.request_id.clone(),
            granted: false,
            remember: false,
            reason: None,
        }
    }
}

/// Render the permission prompt into the given areas.
///
/// `desc_area`   — 1+ row area for the description line
/// `detail_area` — 1+ row area for raw tool input JSON
/// `btn_area`    — 1+ row area for the button row
/// `hint_area`   — 1+ row area for the hint text (optional, may be zero-height)
pub fn layout_for(width: u16, perm: &PendingPermission) -> PermissionLayout {
    let width = width.max(1);
    let desc_rows = Paragraph::new(build_description_line(perm))
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1) as u16;
    let detail_rows = Paragraph::new(build_detail_lines(perm))
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1) as u16;
    let button_rows = build_button_lines(perm.selected, risk_color(perm), width)
        .len()
        .max(1) as u16;
    let hint_rows = Paragraph::new(build_hint_line())
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1) as u16;

    PermissionLayout {
        desc_rows,
        detail_rows,
        button_rows,
        hint_rows,
    }
}

pub fn render(
    frame: &mut Frame,
    desc_area: Rect,
    detail_area: Rect,
    btn_area: Rect,
    hint_area: Rect,
    perm: &PendingPermission,
) {
    let accent = risk_color(perm);

    frame.render_widget(
        Paragraph::new(build_description_line(perm)).wrap(Wrap { trim: false }),
        desc_area,
    );

    frame.render_widget(
        Paragraph::new(build_detail_lines(perm)).wrap(Wrap { trim: false }),
        detail_area,
    );

    // --- Button row ---
    frame.render_widget(
        Paragraph::new(build_button_lines(perm.selected, accent, btn_area.width)),
        btn_area,
    );

    // --- Hint row ---
    if hint_area.height > 0 {
        frame.render_widget(
            Paragraph::new(build_hint_line()).wrap(Wrap { trim: false }),
            hint_area,
        );
    }
}

fn risk_color(perm: &PendingPermission) -> Color {
    match perm.request.risk_level {
        RiskLevel::Low => Color::Green,
        RiskLevel::Medium => Color::Yellow,
        RiskLevel::High => Color::Red,
    }
}

fn sanitize_inline_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = true;

    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }

    if out.ends_with(' ') {
        out.pop();
    }

    out
}

fn build_description_line(perm: &PendingPermission) -> Line<'static> {
    let risk_icon = match perm.request.risk_level {
        RiskLevel::Low => "\u{1F513}",         // 🔓
        RiskLevel::Medium => "\u{1F512}",      // 🔒
        RiskLevel::High => "\u{26A0}\u{FE0F}", // ⚠️
    };
    let accent = risk_color(perm);
    let desc = sanitize_inline_text(&perm.request.description);

    Line::from(vec![
        Span::styled(format!("{risk_icon} "), Style::default().fg(accent)),
        Span::styled(
            perm.request.tool_name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(": {desc}"), Style::default().fg(Color::Gray)),
    ])
}

fn build_button_lines(selected: PermissionChoice, accent: Color, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let indent = "  ";
    let indent_width = indent.width();
    let buttons = [
        build_button("Allow", PermissionChoice::Allow, selected, accent),
        build_button("Deny", PermissionChoice::Deny, selected, accent),
        build_button(
            "Allow Always",
            PermissionChoice::AllowAlways,
            selected,
            accent,
        ),
    ];

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current = vec![Span::raw(indent)];
    let mut line_width = indent_width;
    let mut items_on_line = 0usize;

    for (span, button_width) in buttons {
        let extra_gap = if items_on_line == 0 { 0 } else { 2 };
        if items_on_line > 0 && line_width + extra_gap + button_width > width {
            lines.push(Line::from(std::mem::take(&mut current)));
            current.push(Span::raw(indent));
            line_width = indent_width;
            items_on_line = 0;
        }

        if items_on_line > 0 {
            current.push(Span::raw("  "));
            line_width += 2;
        }

        current.push(span);
        line_width += button_width;
        items_on_line += 1;
    }

    lines.push(Line::from(current));
    lines
}

fn build_button(
    label: &str,
    choice: PermissionChoice,
    selected: PermissionChoice,
    accent: Color,
) -> (Span<'static>, usize) {
    let is_sel = selected == choice;
    let text = if is_sel {
        format!(" [{label}] ")
    } else {
        format!("  {label}  ")
    };
    let style = if is_sel {
        Style::default()
            .fg(Color::White)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };

    let width = text.width();
    (Span::styled(text, style), width)
}

fn build_detail_lines(perm: &PendingPermission) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Header
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Input:",
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
        ),
    ]));

    let json_str = match serde_json::to_string_pretty(&perm.request.input) {
        Ok(s) => s,
        Err(_) => perm.request.input.to_string(),
    };

    for line in json_str.lines() {
        lines.push(Line::styled(
            format!("    {line}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines
}

fn build_hint_line() -> Line<'static> {
    Line::from(vec![
        Span::styled("Tab", Style::default().fg(Color::Cyan)),
        Span::styled(": select  ", Style::default().fg(MUTED)),
        Span::styled("Shift+Tab", Style::default().fg(Color::Cyan)),
        Span::styled(": prev  ", Style::default().fg(MUTED)),
        Span::styled("Enter", Style::default().fg(Color::Cyan)),
        Span::styled(": confirm  ", Style::default().fg(MUTED)),
        Span::styled("Esc", Style::default().fg(Color::Cyan)),
        Span::styled(": deny", Style::default().fg(MUTED)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn sample_request(risk: RiskLevel) -> PermissionRequest {
        PermissionRequest {
            request_id: "req-1".to_string(),
            tool_name: "bash".to_string(),
            input: json!({"command": "rm -rf /tmp/foo"}),
            risk_level: risk,
            description: "Run shell command: rm -rf /tmp/foo".to_string(),
        }
    }

    #[test]
    fn default_selection_low_risk() {
        let p = PendingPermission::new(sample_request(RiskLevel::Low));
        assert_eq!(p.selected, PermissionChoice::Allow);
    }

    #[test]
    fn default_selection_high_risk() {
        let p = PendingPermission::new(sample_request(RiskLevel::High));
        assert_eq!(p.selected, PermissionChoice::Deny);
    }

    #[test]
    fn choice_cycling() {
        assert_eq!(PermissionChoice::Allow.next(), PermissionChoice::Deny);
        assert_eq!(PermissionChoice::Deny.next(), PermissionChoice::AllowAlways);
        assert_eq!(
            PermissionChoice::AllowAlways.next(),
            PermissionChoice::Allow
        );
        assert_eq!(
            PermissionChoice::Allow.prev(),
            PermissionChoice::AllowAlways
        );
    }

    #[test]
    fn allow_response() {
        let p = PendingPermission::new(sample_request(RiskLevel::Low));
        let resp = p.to_response();
        assert!(resp.granted);
        assert!(!resp.remember);
    }

    #[test]
    fn allow_always_response() {
        let mut p = PendingPermission::new(sample_request(RiskLevel::Low));
        p.selected = PermissionChoice::AllowAlways;
        let resp = p.to_response();
        assert!(resp.granted);
        assert!(resp.remember);
    }

    #[test]
    fn deny_response() {
        let p = PendingPermission::new(sample_request(RiskLevel::High));
        let resp = p.deny_response();
        assert!(!resp.granted);
        assert!(!resp.remember);
    }

    #[test]
    fn deny_choice_response() {
        let mut p = PendingPermission::new(sample_request(RiskLevel::Low));
        p.selected = PermissionChoice::Deny;
        let resp = p.to_response();
        assert!(!resp.granted);
    }

    #[test]
    fn sanitize_inline_text_collapses_newlines() {
        assert_eq!(
            sanitize_inline_text("run\nthis\tcommand\r\nnow"),
            "run this command now"
        );
    }

    #[test]
    fn permission_layout_expands_on_narrow_width() {
        let p = PendingPermission::new(sample_request(RiskLevel::Medium));
        let wide = layout_for(120, &p);
        let narrow = layout_for(18, &p);
        // desc + detail (JSON input) + buttons + hints
        assert_eq!(wide.total_rows(), 7);
        assert!(narrow.total_rows() > wide.total_rows());
    }

    #[test]
    fn detail_lines_include_formatted_input_json() {
        let p = PendingPermission::new(sample_request(RiskLevel::Low));
        let lines = build_detail_lines(&p);
        assert!(!lines.is_empty());
        let header = line_text(&lines[0]);
        assert!(header.contains("Input"));
        // At least one JSON line should contain the command key
        let body: String = lines[1..]
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("command"));
        assert!(body.contains("rm -rf /tmp/foo"));
    }
}
