//! Tool call history panel — rendered in the right-side column below tasks.

use super::MUTED;
use ratatui::{
    layout::Rect,
    symbols::border,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
    Frame,
};
use std::time::Instant;

/// A completed or in-progress tool invocation recorded for the monitor panel.
#[derive(Debug, Clone)]
pub struct ToolHistoryEntry {
    pub tool_name: String,
    pub input_summary: String,
    pub duration_ms: u64,
    pub is_error: bool,
    #[allow(dead_code)]
    pub timestamp: Instant,
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    entries: &[ToolHistoryEntry],
    active_tool_names: &[String],
    scroll_offset: &mut usize,
    is_focused: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let dim = Style::default().fg(MUTED);
    let error_style = Style::default().fg(Color::Red);
    let ok_style = Style::default().fg(Color::Green);
    let active_style = Style::default().fg(Color::Cyan);
    let focus_color = if is_focused { Color::Yellow } else { MUTED };

    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    if inner_height == 0 || inner_width == 0 {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    if entries.is_empty() && active_tool_names.is_empty() {
        lines.push(Line::styled("  (no tool calls yet)", dim));
    } else {
        // Active tools first
        for name in active_tool_names {
            let short = shorten(name, inner_width.saturating_sub(4));
            lines.push(Line::from(vec![
                Span::styled("\u{25B6} ", active_style),
                Span::styled(short, active_style),
                Span::styled(" running", dim),
            ]));
        }
        // Then completed history (most recent last = at bottom)
        for entry in entries.iter().rev().take(inner_height.saturating_sub(lines.len())) {
            let (icon, style) = if entry.is_error {
                ("\u{2717}", error_style)
            } else {
                ("\u{2713}", ok_style)
            };
            let dur = format_duration(entry.duration_ms);
            let label = if entry.input_summary.is_empty() {
                format!("{} {}", entry.tool_name, dur)
            } else {
                let summary = &entry.input_summary;
                let max_name = inner_width.saturating_sub(dur.len() + summary.len() + 5);
                if max_name > 4 {
                    format!(
                        "{} {} {}",
                        shorten(&entry.tool_name, max_name),
                        summary,
                        dur
                    )
                } else {
                    format!("{} {}", shorten(&entry.tool_name, inner_width - dur.len() - 4), dur)
                }
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", icon), style),
                Span::styled(label, dim),
            ]));
        }
    }

    // Scroll
    let max_scroll = lines.len().saturating_sub(inner_height);
    *scroll_offset = (*scroll_offset).min(max_scroll);
    let visible: Vec<Line> = lines
        .iter()
        .skip(*scroll_offset)
        .take(inner_height)
        .cloned()
        .collect();

    let title = if *scroll_offset > 0 {
        format!(" Tools \u{2191}{} ", scroll_offset)
    } else {
        " Tools ".to_string()
    };

    let block = Block::bordered()
        .border_set(border::PLAIN)
        .title(title)
        .title_style(Style::default().fg(focus_color).add_modifier(Modifier::BOLD))
        .border_style(Style::default().fg(focus_color));

    let para = Paragraph::new(visible).block(block).wrap(Wrap { trim: false });
    frame.render_widget(para, area);
}

fn shorten(name: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let w = unicode_width::UnicodeWidthStr::width(name);
    if w <= max {
        return name.to_string();
    }
    if max <= 2 {
        return name.chars().take(max).collect();
    }
    let mut result = String::new();
    let mut cur = 0usize;
    let target = max - 1;
    for ch in name.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur + cw > target {
            result.push('\u{2026}');
            break;
        }
        result.push(ch);
        cur += cw;
    }
    result
}

fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}m{}s", secs / 60, secs % 60)
    }
}
