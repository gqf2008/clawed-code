//! Task list panel (aligned with official CC TaskListV2).
//!
//! Displays user todo tasks below the message list when expanded.
//! Reads from `.claude_todos.json` in the current working directory.

use super::MUTED;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::path::Path;
use std::time::SystemTime;

/// A single todo item (mirrors clawed_tools::todo::TodoItem).
#[derive(Debug, Clone)]
pub struct TaskItem {
    pub id: String,
    pub content: String,
    pub status: TaskStatus,
    pub priority: String,
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
        }
    }
}

/// Task list panel state.
pub struct TaskListState {
    tasks: Vec<TaskItem>,
    expanded: bool,
    /// Cached mtime of `.claude_todos.json` to avoid re-reading unchanged files.
    last_mtime: Option<SystemTime>,
}

impl TaskListState {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            expanded: false,
            last_mtime: None,
        }
    }

    /// Load tasks from `.claude_todos.json` in the given directory.
    /// Skips re-reading if the file mtime hasn't changed since last refresh.
    pub fn refresh(&mut self, cwd: &Path) {
        let path = cwd.join(".claude_todos.json");
        if !path.exists() {
            self.tasks.clear();
            self.last_mtime = None;
            return;
        }
        let current_mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok();
        if current_mtime == self.last_mtime {
            return;
        }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let todos: Vec<clawed_tools::todo::TodoItem> =
            serde_json::from_str(&content).unwrap_or_default();
        self.tasks = todos.iter().map(TaskItem::from_todo).collect();
        self.last_mtime = current_mtime;
    }

    pub fn is_visible(&self) -> bool {
        self.expanded && !self.tasks.is_empty()
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Number of rows needed: header line + task lines.
    pub fn render_height(&self) -> u16 {
        if !self.is_visible() {
            return 0;
        }
        (1 + self.tasks.len()) as u16
    }
}

/// Render the task list panel.
pub fn render(frame: &mut Frame, area: Rect, state: &TaskListState) {
    if !state.is_visible() || area.height == 0 {
        return;
    }

    let dim = Style::default().fg(MUTED);
    let bold_white = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let done_style = Style::default().fg(Color::Green);
    let progress_style = Style::default().fg(Color::Cyan);
    let pending_style = Style::default().fg(MUTED);

    let done = state.tasks.iter().filter(|t| t.status == TaskStatus::Completed).count();
    let in_prog = state
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::InProgress)
        .count();
    let pending = state.tasks.iter().filter(|t| t.status == TaskStatus::Pending).count();

    let mut lines: Vec<Line> = Vec::new();

    // Header: "N tasks (X done, Y in progress, Z open)"
    let mut header_spans = vec![
        Span::styled("  ", dim),
        Span::styled(format!("{} tasks", state.tasks.len()), bold_white),
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
        parts.push(Span::styled(format!("{in_prog} in progress"), progress_style));
    }
    if pending > 0 {
        if !parts.is_empty() {
            parts.push(Span::styled(", ", dim));
        }
        parts.push(Span::styled(format!("{pending} open"), pending_style));
    }
    header_spans.extend(parts);
    header_spans.push(Span::styled(")", dim));
    lines.push(Line::from(header_spans));

    // Task lines
    for task in &state.tasks {
        let (icon, icon_style) = match task.status {
            TaskStatus::Completed => ("\u{2713}", done_style),   // ✓
            TaskStatus::InProgress => ("\u{25A0}", progress_style), // ■
            TaskStatus::Pending => ("\u{25A1}", pending_style),   // □
        };
        let priority_marker = match task.priority.as_str() {
            "high" => "\u{2757} ", // ❗
            _ => "",
        };
        lines.push(Line::from(vec![
            Span::styled("  ", dim),
            Span::styled(icon, icon_style),
            Span::raw(" "),
            Span::styled(format!("{priority_marker}{}", task.content), Style::default().fg(Color::White)),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_not_visible() {
        let state = TaskListState::new();
        assert!(!state.is_visible());
        assert_eq!(state.render_height(), 0);
    }

    #[test]
    fn expanded_with_tasks_is_visible() {
        let mut state = TaskListState::new();
        state.expanded = true;
        state.tasks.push(TaskItem {
            id: "t1".into(),
            content: "Fix bug".into(),
            status: TaskStatus::Pending,
            priority: "medium".into(),
        });
        assert!(state.is_visible());
        assert_eq!(state.render_height(), 2); // header + 1 task
    }
}
