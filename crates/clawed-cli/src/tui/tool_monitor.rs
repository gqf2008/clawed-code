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

/// Cached render state for the tool monitor panel.
pub struct ToolMonitorCache {
    pub lines: Vec<Line<'static>>,
    pub dirty: bool,
}

impl ToolMonitorCache {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            dirty: true,
        }
    }
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    entries: &[ToolHistoryEntry],
    active_tool_names: &[String],
    scroll_offset: &mut usize,
    is_focused: bool,
    cache: &mut ToolMonitorCache,
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

    if cache.dirty {
        cache.lines.clear();
        if entries.is_empty() && active_tool_names.is_empty() {
            cache.lines.push(Line::styled("  (no tool calls yet)", dim));
        } else {
            for name in active_tool_names {
                let short = shorten(name, inner_width.saturating_sub(4));
                cache.lines.push(Line::from(vec![
                    Span::styled("\u{25B6} ", active_style),
                    Span::styled(short, active_style),
                    Span::styled(" running", dim),
                ]));
            }
            for entry in entries.iter().rev() {
                let (icon, style) = if entry.is_error {
                    ("\u{2717}", error_style)
                } else {
                    ("\u{2713}", ok_style)
                };
                let dur = super::verbs::format_duration(entry.duration_ms);
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
                        format!(
                            "{} {}",
                            shorten(&entry.tool_name, inner_width - dur.len() - 4),
                            dur
                        )
                    }
                };
                cache.lines.push(Line::from(vec![
                    Span::styled(format!("{} ", icon), style),
                    Span::styled(label, dim),
                ]));
            }
        }
        cache.dirty = false;
    }

    // Scroll
    let max_scroll = cache.lines.len().saturating_sub(inner_height);
    *scroll_offset = (*scroll_offset).min(max_scroll);
    let visible: Vec<Line> = cache.lines
        .iter()
        .skip(*scroll_offset)
        .take(inner_height)
        .cloned()
        .collect();

    let title = if max_scroll > 0 {
        format!(
            " Tools {}/{}\u{2195} ",
            *scroll_offset + visible.len(),
            cache.lines.len()
        )
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

    // Scrollbar — on the right border line, inside the panel
    if max_scroll > 0 {
        let sb_area = Rect::new(
            area.x + area.width.saturating_sub(2),
            area.y + 1,
            1,
            area.height.saturating_sub(2),
        );
        let track_h = sb_area.height as usize;
        let total = cache.lines.len();
        let thumb_h = (track_h * track_h / total).max(1);
        let max_top = track_h - thumb_h;
        let thumb_top = (track_h * *scroll_offset / total).min(max_top);
        let track = Style::default().fg(MUTED);
        let thumb = Style::default().fg(Color::Rgb(140, 140, 140));
        let sb_lines: Vec<Line> = (0..track_h)
            .map(|row| {
                let (ch, style) = if row >= thumb_top && row < thumb_top + thumb_h {
                    ("\u{258C}", thumb)
                } else {
                    ("\u{00B7}", track)
                };
                Line::from(Span::styled(ch, style))
            })
            .collect();
        frame.render_widget(Paragraph::new(sb_lines), sb_area);
    }
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
