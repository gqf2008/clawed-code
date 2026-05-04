//! Task list panel (aligned with official CC TaskListV2).

use super::MUTED;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
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
pub enum TaskStatus { Pending, InProgress, Completed }

impl TaskItem {
    pub fn from_todo(item: &clawed_tools::todo::TodoItem) -> Self {
        Self {
            id: item.id.clone(), content: item.content.clone(),
            status: match item.status.as_str() {
                "in_progress" => TaskStatus::InProgress,
                "completed" => TaskStatus::Completed,
                _ => TaskStatus::Pending,
            },
            priority: item.priority.clone(),
            owner: None, depends_on: Vec::new(), completed_at: None,
        }
    }
}

pub struct TaskListState {
    pub(crate) tasks: Vec<TaskItem>,
    expanded: bool,
    last_mtime: Option<SystemTime>,
}

impl TaskListState {
    pub fn new() -> Self { Self { tasks: Vec::new(), expanded: false, last_mtime: None } }

    pub fn refresh(&mut self, cwd: &Path) {
        let path = cwd.join(".claude_todos.json");
        if !path.exists() { self.tasks.clear(); self.last_mtime = None; return; }
        let current_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if current_mtime == self.last_mtime { return; }
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let todos: Vec<clawed_tools::todo::TodoItem> = serde_json::from_str(&content).unwrap_or_default();
        self.tasks = todos.iter().map(TaskItem::from_todo).collect();
        self.last_mtime = current_mtime;
    }

    pub fn is_visible(&self) -> bool { self.expanded && !self.tasks.is_empty() }
    pub fn is_expanded(&self) -> bool { self.expanded }
    pub fn set_expanded(&mut self, expanded: bool) { self.expanded = expanded; }
    pub fn task_count(&self) -> usize { self.tasks.len() }

    pub fn render_height(&self) -> u16 {
        if !self.is_visible() { return 0; }
        let mut rows = 1 + self.tasks.len();
        for t in &self.tasks { if !t.depends_on.is_empty() { rows += 1; } }
        rows as u16
    }

    #[allow(dead_code)]
    pub fn sort(&mut self) {
        self.tasks.sort_by_key(sort_order);
    }
}

const RECENT_TTL_SECS: u64 = 30;

#[allow(dead_code)]
fn sort_order(task: &TaskItem) -> u8 {
    let recent = task.status == TaskStatus::Completed
        && task.completed_at.map(|t| t.elapsed().as_secs() < RECENT_TTL_SECS).unwrap_or(false);
    match task.status {
        TaskStatus::InProgress => 0,
        TaskStatus::Completed if recent => 0,
        TaskStatus::Completed => 1,
        TaskStatus::Pending if task.depends_on.is_empty() => 2,
        TaskStatus::Pending => 3,
    }
}

pub fn render(frame: &mut Frame, area: Rect, state: &TaskListState) {
    if !state.is_visible() || area.height == 0 { return; }

    let dim = Style::default().fg(MUTED);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let done_style = Style::default().fg(Color::Green);
    let progress_style = Style::default().fg(Color::Cyan);
    let accent = Style::default().fg(Color::Magenta);

    let done = state.tasks.iter().filter(|t| t.status == TaskStatus::Completed).count();
    let in_prog = state.tasks.iter().filter(|t| t.status == TaskStatus::InProgress).count();
    let pending = state.tasks.iter().filter(|t| t.status == TaskStatus::Pending).count();

    let mut lines: Vec<Line> = Vec::new();

    // Header
    let mut header_spans = vec![Span::styled("  ", dim), Span::styled(format!("{} tasks", state.tasks.len()), bold), Span::styled(" (", dim)];
    let mut parts: Vec<Span> = Vec::new();
    if done > 0 { parts.push(Span::styled(format!("{done} done"), done_style)); }
    if in_prog > 0 { if !parts.is_empty() { parts.push(Span::styled(", ", dim)); } parts.push(Span::styled(format!("{in_prog} in progress"), progress_style)); }
    if pending > 0 { if !parts.is_empty() { parts.push(Span::styled(", ", dim)); } parts.push(Span::styled(format!("{pending} open"), dim)); }
    header_spans.extend(parts); header_spans.push(Span::styled(")", dim));
    lines.push(Line::from(header_spans));

    for task in &state.tasks {
        let (icon, icon_style) = match task.status {
            TaskStatus::Completed => ("\u{2713}", done_style),
            TaskStatus::InProgress => ("\u{25FC}", progress_style),
            TaskStatus::Pending => ("\u{25FB}", Style::default()),
        };
        let content_style = if task.status == TaskStatus::Completed {
            Style::default().add_modifier(Modifier::CROSSED_OUT)
        } else { Style::default() };

        let mut task_spans = vec![Span::styled("  ", dim), Span::styled(icon, icon_style), Span::raw(" "), Span::styled(&task.content, content_style)];
        if area.width >= 60 { if let Some(ref owner) = task.owner { task_spans.push(Span::styled(format!(" (@{owner})"), accent)); } }
        lines.push(Line::from(task_spans));

        if !task.depends_on.is_empty() {
            let blocked_list = task.depends_on.iter().map(|id| format!("#{id}")).collect::<Vec<_>>().join(", ");
            lines.push(Line::styled(format!("     \u{25B8} blocked by {blocked_list}"), dim));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_task(content: &str, status: TaskStatus) -> TaskItem {
        TaskItem { id: "x".into(), content: content.into(), status, priority: "".into(), owner: None, depends_on: vec![], completed_at: None }
    }

    #[test]
    fn empty_list_not_visible() {
        let state = TaskListState::new();
        assert!(!state.is_visible()); assert_eq!(state.render_height(), 0);
    }

    #[test]
    fn expanded_with_tasks_is_visible() {
        let mut state = TaskListState::new(); state.expanded = true;
        state.tasks.push(mk_task("Fix bug", TaskStatus::Pending));
        assert!(state.is_visible()); assert_eq!(state.render_height(), 2);
    }

    #[test]
    fn blocked_task_has_extra_row() {
        let mut state = TaskListState::new(); state.expanded = true;
        state.tasks.push(TaskItem { id: "t1".into(), content: "Fix".into(), status: TaskStatus::Pending, priority: "".into(), owner: None, depends_on: vec!["t2".into()], completed_at: None });
        assert_eq!(state.render_height(), 3);
    }

    #[test]
    fn sort_puts_in_progress_first() {
        let mut state = TaskListState::new();
        state.tasks.push(TaskItem { id: "a".into(), content: "Done".into(), status: TaskStatus::Completed, priority: "".into(), owner: None, depends_on: vec![], completed_at: Some(Instant::now() - std::time::Duration::from_secs(60)) });
        state.tasks.push(TaskItem { id: "b".into(), content: "Active".into(), status: TaskStatus::InProgress, priority: "".into(), owner: None, depends_on: vec![], completed_at: None });
        state.sort();
        assert_eq!(state.tasks[0].content, "Active");
    }

    #[test]
    fn recently_completed_boosted() {
        let mut state = TaskListState::new();
        state.tasks.push(TaskItem { id: "a".into(), content: "JustDone".into(), status: TaskStatus::Completed, priority: "".into(), owner: None, depends_on: vec![], completed_at: Some(Instant::now()) });
        state.tasks.push(TaskItem { id: "b".into(), content: "OldDone".into(), status: TaskStatus::Completed, priority: "".into(), owner: None, depends_on: vec![], completed_at: Some(Instant::now() - std::time::Duration::from_secs(60)) });
        state.sort();
        assert_eq!(state.tasks[0].content, "JustDone");
    }

    #[test]
    fn verify_task_icons() {
        assert_eq!("\u{25FC}".chars().count(), 1); // ◼ in-progress
        assert_eq!("\u{25FB}".chars().count(), 1); // ◻ pending
        assert_eq!("\u{2713}".chars().count(), 1); // ✓ completed
    }
}
