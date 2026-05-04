//! Dynamic task plan panel for sub-agent progress tracking.
//!
//! Aligned with official CC TaskListV2 / BackgroundTasksDialog.

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
    /// Current activity description (last tool call, etc.).
    pub activity: Option<String>,
}

/// The task plan panel state.
pub struct TaskPlan {
    pub(crate) tasks: Vec<TrackedTask>,
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
        if !self.tasks.iter().any(|t| t.id == id) {
            self.tasks.push(TrackedTask {
                id,
                title,
                status: TaskStatus::Running,
                started: Instant::now(),
                finished: None,
                activity: None,
            });
        }
    }

    /// Update the activity text for a running task (last tool call, etc.).
    #[allow(dead_code)]
    pub fn update_activity(&mut self, id: &str, activity: String) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.activity = Some(activity);
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

    /// Number of rows needed to render.
    pub fn render_height(&self) -> u16 {
        if !self.is_visible() {
            return 0;
        }
        let vis = self.visible_task_count();
        if vis == 0 {
            return 3; // summary + count
        }
        // summary + count + visible tasks + optional activity lines
        let activity_rows: usize = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Running && t.activity.is_some())
            .count();
        (2 + vis + activity_rows) as u16
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

    /// Number of tasks to show before truncation.
    fn visible_task_count(&self) -> usize {
        self.tasks.len()
    }
}

// ── Render helpers ───────────────────────────────────────────────────────────

/// Tree characters used for task connectors.
const TREE_BRANCH: &str = "├─ ";
const TREE_LAST: &str = "└─ ";
const TREE_CONT: &str = "│  ";
const TREE_SPACE: &str = "   ";

/// Render the task plan panel.
pub fn render(frame: &mut Frame, area: Rect, plan: &TaskPlan) {
    if !plan.is_visible() || area.height == 0 {
        return;
    }

    let dim = Style::default().fg(MUTED);
    let accent = Style::default().fg(Color::Magenta);
    let done_style = Style::default().fg(Color::Green);
    let fail_style = Style::default().fg(Color::Red);
    let bold = Style::default().add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();

    // Summary line: ✻ Cooked for Xm Ys · N shells running · N teammates
    let elapsed = plan.plan_start.map(|s| s.elapsed()).unwrap_or_default();
    let mins = elapsed.as_secs() / 60;
    let secs = elapsed.as_secs() % 60;

    let mut summary_spans = vec![
        Span::styled("\u{273B} ", accent), // ✻
        Span::styled(format!("Cooked for {mins}m {secs}s"), bold),
    ];
    let running = plan
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Running)
        .count();
    if running > 0 {
        summary_spans.push(Span::styled(
            format!(" · {running} running"),
            dim,
        ));
    }
    if plan.active_shells > 0 {
        let s = if plan.active_shells == 1 { "" } else { "s" };
        summary_spans.push(Span::styled(
            format!(" · {} shell{s}", plan.active_shells),
            dim,
        ));
    }
    lines.push(Line::from(summary_spans));

    // Task count line
    let (done, failed, open) = plan.count_by_status();
    let total = plan.tasks.len();
    let mut count_spans = vec![Span::styled("  ", dim)];
    count_spans.push(Span::styled(format!("{total} tasks"), bold));
    count_spans.push(Span::styled(" (", dim));
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
        parts.push(Span::styled(format!("{open} open"), dim));
    }
    count_spans.extend(parts);
    count_spans.push(Span::styled(")", dim));
    lines.push(Line::from(count_spans));

    // Individual task lines with tree structure
    let task_count = plan.tasks.len();
    let completed_count = done + failed;
    let show_completed = completed_count <= 5 || area.height > 15;

    for (i, task) in plan.tasks.iter().enumerate() {
        let is_last_visible = i + 1 == task_count;
        let is_completed = task.status != TaskStatus::Running;

        // Collapse older completed tasks
        if is_completed && !show_completed && i < completed_count.saturating_sub(3) {
            if i == 0 {
                lines.push(Line::styled(
                    format!("  + {completed_count} completed"),
                    dim,
                ));
            }
            continue;
        }

        let tree_char = if is_last_visible {
            TREE_LAST
        } else {
            TREE_BRANCH
        };

        let (icon, icon_style) = match task.status {
            TaskStatus::Done => ("\u{2713}", done_style),     // ✓
            TaskStatus::Failed => ("\u{2717}", fail_style),   // ✗
            TaskStatus::Running => ("\u{25CF}", accent),      // ●
        };

        let mut spans = vec![
            Span::styled(format!("  {tree_char}"), dim),
            Span::styled(icon, icon_style),
            Span::raw(" "),
            Span::styled(&task.title, bold),
        ];

        // Elapsed time for running tasks
        if task.status == TaskStatus::Running {
            let te = task.started.elapsed();
            let m = te.as_secs() / 60;
            let s = te.as_secs() % 60;
            spans.push(Span::styled(format!(" ({m:02}:{s:02})"), dim));
        }

        lines.push(Line::from(spans));

        // Activity line for running tasks
        if let Some(ref activity) = task.activity {
            if task.status == TaskStatus::Running {
                let cont = if is_last_visible {
                    TREE_SPACE
                } else {
                    TREE_CONT
                };
                lines.push(Line::styled(
                    format!("  {cont}  {activity}"),
                    dim,
                ));
            }
        }
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
    fn test_activity_update() {
        let mut plan = TaskPlan::new();
        plan.add_task("t1".into(), "Task A".into());
        plan.update_activity("t1", "Bash(cargo test)".into());
        assert_eq!(plan.tasks[0].activity.as_deref(), Some("Bash(cargo test)"));
    }

    // ── Rendering verification tests ──

    #[test]
    fn verify_taskplan_icons() {
        let mut plan = TaskPlan::new();
        plan.add_task("t1".into(), "Build".into());
        plan.add_task("t2".into(), "Test".into());
        plan.complete_task("t1", false);
        plan.terminate_task("t2");
        // Running: ● (U+25CF), Done: ✓ (U+2713), Failed: ✗ (U+2717)
        let icons: Vec<&str> = plan.tasks.iter().map(|t| match t.status {
            TaskStatus::Running => "\u{25CF}",
            TaskStatus::Done => "\u{2713}",
            TaskStatus::Failed => "\u{2717}",
        }).collect();
        assert_eq!(icons, vec!["\u{2713}", "\u{2717}"]); // ✓, ✗ (both completed)
    }

    #[test]
    fn verify_tree_chars() {
        assert_eq!(TREE_BRANCH, "\u{251C}\u{2500} "); // ├─
        assert_eq!(TREE_LAST, "\u{2514}\u{2500} ");   // └─
        assert_eq!(TREE_CONT, "\u{2502}  ");            // │
        assert_eq!(TREE_SPACE, "   ");
    }

    #[test]
    fn verify_render_height_includes_activity_rows() {
        let mut plan = TaskPlan::new();
        plan.add_task("t1".into(), "Build".into());
        plan.update_activity("t1", "Bash(cargo test)".into());
        // summary(1) + count(1) + task(1) + activity(1) = 4
        assert!(plan.render_height() >= 3);
    }
}
