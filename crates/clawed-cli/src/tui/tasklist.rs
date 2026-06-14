//! Task list side panel — rendered in a right-side column.

use super::MUTED;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
    Frame,
};
use std::path::Path;
use std::time::{Instant, SystemTime};

#[derive(Debug, Clone)]
pub struct TaskItem {
    #[allow(dead_code)]
    pub id: String,
    pub content: String,
    pub status: TaskStatus,
    #[allow(dead_code)]
    pub priority: String,
    pub owner: Option<String>,
    pub depends_on: Vec<String>,
    pub completed_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskItem {
    pub fn from_todo(item: &clawed_tools::todo::TodoItem) -> Self {
        Self {
            id: item.id.clone(),
            content: item.content.clone(),
            status: match item.status.as_str() {
                "in_progress" => TaskStatus::InProgress,
                "completed" => TaskStatus::Completed,
                _ => TaskStatus::Pending,
            },
            priority: item.priority.clone(),
            owner: None,
            depends_on: Vec::new(),
            completed_at: None,
        }
    }
}

pub struct TaskListState {
    pub(crate) tasks: Vec<TaskItem>,
    pub(crate) side_panel_visible: bool,
    pub(crate) scroll_offset: usize,
    last_mtime: Option<SystemTime>,
}

impl TaskListState {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            side_panel_visible: false,
            scroll_offset: 0,
            last_mtime: None,
        }
    }

    pub fn refresh(&mut self, cwd: &Path) {
        let path = cwd.join(".claude_todos.json");
        if !path.exists() {
            self.tasks.clear();
            self.last_mtime = None;
            return;
        }
        let current_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if current_mtime == self.last_mtime {
            return;
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let todos: Vec<clawed_tools::todo::TodoItem> =
            serde_json::from_str(&content).unwrap_or_default();
        self.tasks = todos.iter().map(TaskItem::from_todo).collect();
        self.last_mtime = current_mtime;
    }

    #[allow(dead_code)]
    pub fn is_visible(&self) -> bool {
        self.side_panel_visible && !self.tasks.is_empty()
    }
    pub fn is_expanded(&self) -> bool {
        self.side_panel_visible
    }
    #[allow(dead_code)]
    pub fn set_expanded(&mut self, expanded: bool) {
        self.side_panel_visible = expanded;
    }
    pub fn toggle_side_panel(&mut self) {
        self.side_panel_visible = !self.side_panel_visible;
    }
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    #[allow(dead_code)]
    pub fn render_height() -> u16 {
        // Side panel no longer uses vertical layout constraints.
        0
    }

    #[allow(dead_code)]
    pub fn sort(&mut self) {
        self.tasks.sort_by_key(sort_order);
    }

    /// Width the side panel should occupy. Uses the dynamic `panel_width` setting
    /// from the App state.
    pub fn panel_width(&self, dynamic_width: u16) -> u16 {
        if !self.side_panel_visible {
            return 0;
        }
        dynamic_width
    }
}

const RECENT_TTL_SECS: u64 = 30;

#[allow(dead_code)]
fn sort_order(task: &TaskItem) -> u8 {
    let recent = task.status == TaskStatus::Completed
        && task
            .completed_at
            .map(|t| t.elapsed().as_secs() < RECENT_TTL_SECS)
            .unwrap_or(false);
    match task.status {
        TaskStatus::InProgress => 0,
        TaskStatus::Completed if recent => 0,
        TaskStatus::Completed => 1,
        TaskStatus::Pending if task.depends_on.is_empty() => 2,
        TaskStatus::Pending => 3,
    }
}

pub fn render(frame: &mut Frame, area: Rect, state: &mut TaskListState) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if state.tasks.is_empty() {
        let block = Block::bordered()
            .border_set(border::PLAIN)
            .title(" Tasks ")
            .title_style(super::muted());
        let text = Paragraph::new(vec![Line::styled("  (empty)", super::muted())]);
        frame.render_widget(text.block(block), area);
        return;
    }

    let dim = Style::default().fg(MUTED);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let done_style = Style::default().fg(Color::Green);
    let progress_style = Style::default().fg(Color::Cyan);
    let accent = Style::default().fg(Color::Magenta);

    let done = state
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Completed)
        .count();
    let in_prog = state
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::InProgress)
        .count();
    let pending = state
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .count();

    let mut lines: Vec<Line> = Vec::new();

    // Header
    let mut header_spans = vec![
        Span::styled(format!("{} tasks", state.tasks.len()), bold),
        Span::styled(" (", dim),
    ];
    let mut parts: Vec<Span> = Vec::new();
    if done > 0 {
        parts.push(Span::styled(format!("{done} done"), done_style));
    }
    if in_prog > 0 {
        if !parts.is_empty() {
            parts.push(Span::styled(", ", dim));
        }
        parts.push(Span::styled(
            format!("{in_prog} in progress"),
            progress_style,
        ));
    }
    if pending > 0 {
        if !parts.is_empty() {
            parts.push(Span::styled(", ", dim));
        }
        parts.push(Span::styled(format!("{pending} open"), dim));
    }
    header_spans.extend(parts);
    header_spans.push(Span::styled(")", dim));
    lines.push(Line::from(header_spans));

    for task in &state.tasks {
        let (icon, icon_style) = match task.status {
            TaskStatus::Completed => ("\u{2713}", done_style),
            TaskStatus::InProgress => ("\u{25FC}", progress_style),
            TaskStatus::Pending => ("\u{25FB}", Style::default()),
        };
        let content_style = if task.status == TaskStatus::Completed {
            Style::default().add_modifier(Modifier::CROSSED_OUT)
        } else {
            Style::default()
        };
        let mut task_spans = vec![
            Span::styled(icon, icon_style),
            Span::raw("  "),
            Span::styled(&task.content, content_style),
        ];
        if let Some(ref owner) = task.owner {
            task_spans.push(Span::styled(format!(" @{owner}"), accent));
        }
        lines.push(Line::from(task_spans));
        if !task.depends_on.is_empty() {
            let blocked_list = task
                .depends_on
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(Line::styled(
                format!("   \u{25B8} blocked by {blocked_list}"),
                dim,
            ));
        }
    }

    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    if inner_height == 0 || inner_width == 0 {
        return;
    }

    let mut wrapped: Vec<Line<'_>> = Vec::new();
    for line in &lines {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let w = unicode_width::UnicodeWidthStr::width(text.as_str());
        if w <= inner_width {
            wrapped.push(line.clone());
        } else {
            let default_style = line
                .spans
                .iter()
                .find(|s| !s.content.trim().is_empty())
                .map(|s| s.style)
                .unwrap_or_default();
            let mut remaining = text.as_str();
            let mut first = true;
            while !remaining.is_empty() {
                let (chunk, rest) = split_at_display_width(remaining, inner_width);
                let style = if first { default_style } else { super::muted() };
                wrapped.push(Line::styled(chunk.to_string(), style));
                remaining = rest;
                first = false;
            }
        }
    }

    let max_scroll = wrapped.len().saturating_sub(inner_height);
    state.scroll_offset = state.scroll_offset.min(max_scroll);
    let visible: Vec<Line> = wrapped
        .iter()
        .skip(state.scroll_offset)
        .take(inner_height)
        .cloned()
        .collect();

    let title = if max_scroll > 0 {
        format!(
            " Tasks {}/{}\u{2195} ",
            state.scroll_offset + visible.len(),
            wrapped.len()
        )
    } else {
        " Tasks ".to_string()
    };

    let block = Block::bordered()
        .border_set(border::PLAIN)
        .title(title)
        .title_style(super::muted());

    let para = Paragraph::new(visible)
        .block(block)
        .wrap(Wrap { trim: false });
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
        let total = wrapped.len();
        let thumb_h = (track_h * track_h / total).max(1);
        let max_top = track_h - thumb_h;
        let thumb_top = (track_h * state.scroll_offset / total).min(max_top);
        let track = super::muted();
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

fn split_at_display_width(s: &str, max_width: usize) -> (&str, &str) {
    let mut w = 0usize;
    for (i, c) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max_width {
            return (&s[..i], &s[i..]);
        }
        w += cw;
    }
    (s, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_task(content: &str, status: TaskStatus) -> TaskItem {
        TaskItem {
            id: format!("task-{content}"),
            content: content.to_string(),
            status,
            priority: "medium".to_string(),
            owner: None,
            depends_on: Vec::new(),
            completed_at: None,
        }
    }

    #[test]
    fn render_height_empty() {
        assert_eq!(TaskListState::render_height(), 0);
    }

    #[test]
    fn render_height_with_tasks() {
        let mut state = TaskListState::new();
        state.tasks.push(mk_task("hello", TaskStatus::Pending));
        state.side_panel_visible = true;
        assert_eq!(TaskListState::render_height(), 0);
    }

    #[test]
    fn test_panel_width_visible() {
        let mut state = TaskListState::new();
        state.side_panel_visible = true;
        state.tasks.push(mk_task("t", TaskStatus::Pending));
        assert_eq!(state.panel_width(30), 30);
        assert_eq!(state.panel_width(40), 40);
    }

    #[test]
    fn test_panel_width_hidden() {
        let mut state = TaskListState::new();
        state.side_panel_visible = false;
        state.tasks.push(mk_task("t", TaskStatus::Pending));
        assert_eq!(state.panel_width(30), 0);
    }

    #[test]
    fn test_split_at_display_width() {
        assert_eq!(split_at_display_width("hello", 3), ("hel", "lo"));
        assert_eq!(split_at_display_width("ab", 5), ("ab", ""));
        assert_eq!(split_at_display_width("", 3), ("", ""));
    }

    #[test]
    fn test_sort_completed_last() {
        let mut state = TaskListState::new();
        state.side_panel_visible = true;
        state.tasks = vec![mk_task("one", TaskStatus::Completed)];
        state.sort();
    }
}
