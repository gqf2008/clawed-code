//! Dynamic task plan panel for sub-agent progress tracking.
//!
//! Renders a summary line and task list like:
//! ```text
//! ✻ Cooked for 1m 32s · 2 shells running
//!
//!   7 tasks (4 done, 3 open)
//!   √ Implement hermes-batch crate
//!   √ 添加 ContextEngine trait
//!   □ Fix flaky Exa backend test
//!   □ Cron: corruption handling
//! ```

use super::MUTED;
use std::time::Instant;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Status of a tracked task.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Done,
    Failed,
}

/// A single tracked task (maps to a sub-agent or background job).
pub struct TrackedTask {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    pub started: Instant,
    pub finished: Option<Instant>,
}

/// The task plan panel state.
pub struct TaskPlan {
    tasks: Vec<TrackedTask>,
    active_shells: u32,
    plan_start: Option<Instant>,
}

impl TaskPlan {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            active_shells: 0,
            plan_start: None,
        }
    }

    /// Add a new task (from AgentSpawned).
    pub fn add_task(&mut self, id: String, title: String) {
        if self.plan_start.is_none() {
            self.plan_start = Some(Instant::now());
        }
        // Avoid duplicate IDs
        if !self.tasks.iter().any(|t| t.id == id) {
            self.tasks.push(TrackedTask {
                id,
                title,
                status: TaskStatus::Running,
                started: Instant::now(),
                finished: None,
            });
        }
    }

    /// Mark a task as completed (from AgentComplete).
    pub fn complete_task(&mut self, id: &str, is_error: bool) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.status = if is_error {
                TaskStatus::Failed
            } else {
                TaskStatus::Done
            };
            task.finished = Some(Instant::now());
        }
    }

    /// Mark a task as terminated (from AgentTerminated).
    pub fn terminate_task(&mut self, id: &str) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.status = TaskStatus::Failed;
            task.finished = Some(Instant::now());
        }
    }

    /// Update shell count.
    pub fn set_shells(&mut self, count: u32) {
        self.active_shells = count;
    }

    /// Whether the panel should be shown.
    pub fn is_visible(&self) -> bool {
        !self.tasks.is_empty()
    }

    /// Number of rows needed to render (summary + blank + count + tasks).
    pub fn render_height(&self) -> u16 {
        if !self.is_visible() {
            return 0;
        }
        // summary line + blank + "N tasks (X done, Y open)" + task lines
        (2 + self.tasks.len()) as u16
    }

    fn count_by_status(&self) -> (usize, usize, usize) {
        let done = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count();
        let failed = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed)
            .count();
        let open = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running)
            .count();
        (done, failed, open)
    }
}

/// Render the task plan panel.
pub fn render(frame: &mut Frame, area: Rect, plan: &TaskPlan) {
    if !plan.is_visible() || area.height == 0 {
        return;
    }

    let dim = Style::default().fg(MUTED);
    let accent = Style::default().fg(Color::Magenta);
    let done_style = Style::default().fg(Color::Green);
    let fail_style = Style::default().fg(Color::Red);
    let open_style = Style::default().fg(MUTED);
    let title_style = Style::default().fg(Color::White);
    let bold_white = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();

    // Summary line: ✻ Cooked for Xm Ys · N shells running
    let elapsed = plan.plan_start.map(|s| s.elapsed()).unwrap_or_default();
    let mins = elapsed.as_secs() / 60;
    let secs = elapsed.as_secs() % 60;

    let mut summary_spans = vec![
        Span::styled("✻ ", accent),
        Span::styled(format!("Cooked for {mins}m {secs}s"), bold_white),
    ];
    if plan.active_shells > 0 {
        let s = if plan.active_shells == 1 { "" } else { "s" };
        summary_spans.push(Span::styled(
            format!(" · {} shell{s} running", plan.active_shells),
            dim,
        ));
    }
    lines.push(Line::from(summary_spans));

    // Task count line
    let (done, failed, open) = plan.count_by_status();
    let total = plan.tasks.len();
    let mut count_spans = vec![
        Span::styled("  ", dim),
        Span::styled(format!("{total} tasks"), bold_white),
        Span::styled(" (", dim),
    ];
    let mut parts: Vec<Span> = Vec::new();
    if done > 0 {
        parts.push(Span::styled(format!("{done} done"), done_style));
    }
    if failed > 0 {
        if !parts.is_empty() {
            parts.push(Span::styled(", ", dim));
        }
        parts.push(Span::styled(format!("{failed} failed"), fail_style));
    }
    if open > 0 {
        if !parts.is_empty() {
            parts.push(Span::styled(", ", dim));
        }
        parts.push(Span::styled(format!("{open} open"), open_style));
    }
    count_spans.extend(parts);
    count_spans.push(Span::styled(")", dim));
    lines.push(Line::from(count_spans));

    // Individual task lines
    for task in &plan.tasks {
        let (icon, icon_style) = match task.status {
            TaskStatus::Done => ("√", done_style),
            TaskStatus::Failed => ("✗", fail_style),
            TaskStatus::Running => ("□", open_style),
        };
        let mut spans = vec![
            Span::styled("  ", dim),
            Span::styled(icon, icon_style),
            Span::raw(" "),
            Span::styled(&task.title, title_style),
        ];
        // Show elapsed time for running tasks
        if task.status == TaskStatus::Running {
            let te = task.started.elapsed();
            let m = te.as_secs() / 60;
            let s = te.as_secs() % 60;
            spans.push(Span::styled(format!(" ({m:02}:{s:02})"), dim));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_plan_not_visible() {
        let plan = TaskPlan::new();
        assert!(!plan.is_visible());
        assert_eq!(plan.render_height(), 0);
    }

    #[test]
    fn test_add_task_makes_visible() {
        let mut plan = TaskPlan::new();
        plan.add_task("t1".into(), "Fix bug".into());
        assert!(plan.is_visible());
        assert_eq!(plan.render_height(), 3); // summary + count + 1 task
    }

    #[test]
    fn test_complete_task() {
        let mut plan = TaskPlan::new();
        plan.add_task("t1".into(), "Task A".into());
        plan.add_task("t2".into(), "Task B".into());
        plan.complete_task("t1", false);
        let (done, failed, open) = plan.count_by_status();
        assert_eq!(done, 1);
        assert_eq!(failed, 0);
        assert_eq!(open, 1);
    }

    #[test]
    fn test_terminate_task() {
        let mut plan = TaskPlan::new();
        plan.add_task("t1".into(), "Task A".into());
        plan.terminate_task("t1");
        let (_, failed, _) = plan.count_by_status();
        assert_eq!(failed, 1);
    }

    #[test]
    fn test_duplicate_id_ignored() {
        let mut plan = TaskPlan::new();
        plan.add_task("t1".into(), "Task A".into());
        plan.add_task("t1".into(), "Task A again".into());
        assert_eq!(plan.tasks.len(), 1);
    }

    #[test]
    fn test_shell_count() {
        let mut plan = TaskPlan::new();
        plan.set_shells(3);
        assert_eq!(plan.active_shells, 3);
    }

    #[test]
    fn test_render_height_multiple_tasks() {
        let mut plan = TaskPlan::new();
        plan.add_task("a".into(), "A".into());
        plan.add_task("b".into(), "B".into());
        plan.add_task("c".into(), "C".into());
        // summary + count + 3 tasks = 5
        assert_eq!(plan.render_height(), 5);
    }
}
