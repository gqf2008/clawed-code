//! Permission prompt widget — replaces the input area when a tool needs approval.
//!
//! Layout (3 rows):
//! ```text
//! 🔒 bash wants to run: rm -rf /tmp/foo     ← description line
//!   [Allow]  [Deny]  [Allow Always]          ← button row
//! Tab: select  Enter: confirm  Esc: deny     ← hint row
//! ```

use super::MUTED;
use clawed_bus::events::{PermissionRequest, PermissionResponse, RiskLevel};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

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
        }
    }

    /// Build a deny response (for Esc shortcut).
    pub fn deny_response(&self) -> PermissionResponse {
        PermissionResponse {
            request_id: self.request.request_id.clone(),
            granted: false,
            remember: false,
        }
    }
}

/// Render the permission prompt into the given areas.
///
/// `desc_area` — 1+ row area for the description line
/// `btn_area`  — 1 row area for the button row
/// `hint_area` — 1 row area for the hint text (optional, may be zero-height)
pub fn render(
    frame: &mut Frame,
    desc_area: Rect,
    btn_area: Rect,
    hint_area: Rect,
    perm: &PendingPermission,
) {
    // --- Description line ---
    let risk_color = match perm.request.risk_level {
        RiskLevel::Low => Color::Green,
        RiskLevel::Medium => Color::Yellow,
        RiskLevel::High => Color::Red,
    };
    let risk_icon = match perm.request.risk_level {
        RiskLevel::Low => "\u{1F513}",    // 🔓
        RiskLevel::Medium => "\u{1F512}", // 🔒
        RiskLevel::High => "\u{26A0}\u{FE0F}",  // ⚠️
    };

    let desc = &perm.request.description;
    let desc_line = Line::from(vec![
        Span::styled(
            format!("{risk_icon} "),
            Style::default().fg(risk_color),
        ),
        Span::styled(
            perm.request.tool_name.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(": {desc}"),
            Style::default().fg(Color::Gray),
        ),
    ]);
    frame.render_widget(Paragraph::new(desc_line), desc_area);

    // --- Button row ---
    let btn_line = build_button_line(perm.selected, risk_color);
    frame.render_widget(Paragraph::new(btn_line), btn_area);

    // --- Hint row ---
    if hint_area.height > 0 {
        let hint = Line::from(vec![
            Span::styled("Tab", Style::default().fg(Color::Cyan)),
            Span::styled(": select  ", Style::default().fg(MUTED)),
            Span::styled("Enter", Style::default().fg(Color::Cyan)),
            Span::styled(": confirm  ", Style::default().fg(MUTED)),
            Span::styled("Esc", Style::default().fg(Color::Cyan)),
            Span::styled(": deny", Style::default().fg(MUTED)),
        ]);
        frame.render_widget(Paragraph::new(hint), hint_area);
    }
}

fn build_button_line(selected: PermissionChoice, accent: Color) -> Line<'static> {
    let btn = |label: &str, choice: PermissionChoice| -> Vec<Span<'static>> {
        let is_sel = selected == choice;
        if is_sel {
            vec![
                Span::styled(
                    format!(" [{label}] "),
                    Style::default()
                        .fg(Color::White)
                        .bg(accent)
                        .add_modifier(Modifier::BOLD),
                ),
            ]
        } else {
            vec![
                Span::styled(
                    format!("  {label}  "),
                    Style::default().fg(MUTED),
                ),
            ]
        }
    };

    let mut spans = vec![Span::raw("  ")];
    spans.extend(btn("Allow", PermissionChoice::Allow));
    spans.push(Span::raw("  "));
    spans.extend(btn("Deny", PermissionChoice::Deny));
    spans.push(Span::raw("  "));
    spans.extend(btn("Allow Always", PermissionChoice::AllowAlways));

    Line::from(spans)
}

/// Rows needed for the permission prompt footer.
pub const PERM_ROWS: u16 = 3; // description + buttons + hints

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(PermissionChoice::AllowAlways.next(), PermissionChoice::Allow);
        assert_eq!(PermissionChoice::Allow.prev(), PermissionChoice::AllowAlways);
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
}
