//! Scheduled prompts, stored in `<project>/.claude/scheduled_tasks.json`.
//!
//! Tasks come in two flavors:
//!   - One-shot (`recurring: false`) — fire once, then auto-delete.
//!   - Recurring (`recurring: true`) — fire on schedule, reschedule from now,
//!     persist until explicitly deleted or auto-expire after `recurring_max_age_ms`.

use chrono::Timelike;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::cron::parse_cron_expression;

/// A single scheduled task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronTask {
    pub id: String,
    /// 5-field cron string (local time).
    pub cron: String,
    /// Prompt to enqueue when the task fires.
    pub prompt: String,
    /// Epoch ms when the task was created.
    pub created_at: i64,
    /// Epoch ms of the most recent fire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<i64>,
    /// When true, the task reschedules after firing instead of being deleted.
    #[serde(default, skip_serializing_if = "is_false")]
    pub recurring: bool,
    /// When true, exempt from auto-expiry.
    #[serde(default, skip_serializing_if = "is_false")]
    pub permanent: bool,
    /// When true (default), persisted to disk. When false, in-memory only — dies with the session.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub durable: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

fn default_true() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CronFile {
    tasks: Vec<CronTask>,
}

/// Jitter configuration for cron scheduling.
#[derive(Debug, Clone)]
pub struct CronJitterConfig {
    /// Recurring-task forward delay as a fraction of the interval.
    pub recurring_frac: f64,
    /// Upper bound on recurring forward delay (ms).
    pub recurring_cap_ms: i64,
    /// One-shot backward lead: maximum ms a task may fire early.
    pub one_shot_max_ms: i64,
    /// One-shot backward lead: minimum ms a task fires early.
    pub one_shot_floor_ms: i64,
    /// Jitter fires landing on minutes where `minute % N == 0`.
    pub one_shot_minute_mod: u32,
    /// Recurring tasks auto-expire this many ms after creation.
    /// 0 = unlimited.
    pub recurring_max_age_ms: i64,
}

impl Default for CronJitterConfig {
    fn default() -> Self {
        Self {
            recurring_frac: 0.1,
            recurring_cap_ms: 15 * 60 * 1000,
            one_shot_max_ms: 90 * 1000,
            one_shot_floor_ms: 0,
            one_shot_minute_mod: 30,
            recurring_max_age_ms: 7 * 24 * 60 * 60 * 1000,
        }
    }
}

const CRON_FILE_REL: &str = ".claude/scheduled_tasks.json";

/// Global in-memory store for non-durable (session-only) tasks, keyed by directory.
type MemoryMap = std::collections::HashMap<String, Vec<CronTask>>;
static MEMORY_TASKS: std::sync::OnceLock<std::sync::Mutex<MemoryMap>> =
    std::sync::OnceLock::new();

fn memory_tasks_map() -> &'static std::sync::Mutex<MemoryMap> {
    MEMORY_TASKS.get_or_init(|| std::sync::Mutex::new(MemoryMap::new()))
}

fn lock_map() -> std::sync::MutexGuard<'static, MemoryMap> {
    crate::sync::lock_or_recover(memory_tasks_map())
}

fn dir_key(dir: &Path) -> String {
    dir.to_string_lossy().to_string()
}

fn read_memory_tasks_for(dir: &Path) -> Vec<CronTask> {
    lock_map().get(&dir_key(dir)).cloned().unwrap_or_default()
}

fn with_memory_tasks<F, R>(dir: &Path, f: F) -> R
where
    F: FnOnce(&mut Vec<CronTask>) -> R,
{
    let key = dir_key(dir);
    f(lock_map().entry(key).or_default())
}

pub fn clear_memory_cron_tasks_for(dir: &Path) {
    lock_map().remove(&dir_key(dir));
}

pub fn clear_memory_cron_tasks() {
    lock_map().clear();
}

/// Path to the cron file in the given project directory.
pub fn get_cron_file_path(dir: &Path) -> PathBuf {
    dir.join(CRON_FILE_REL)
}

/// Read and parse .claude/scheduled_tasks.json (disk only).
/// Returns an empty list if the file is missing, empty, or malformed.
/// Tasks with invalid cron strings are silently dropped.
async fn read_disk_tasks(dir: &Path) -> Vec<CronTask> {
    let path = get_cron_file_path(dir);
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    parse_cron_file(&raw)
}

/// Sync variant for disk-only read.
fn read_disk_tasks_sync(dir: &Path) -> Vec<CronTask> {
    let path = get_cron_file_path(dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    parse_cron_file(&raw)
}

/// Read all tasks — disk (durable) + in-memory (non-durable), merged.
pub async fn read_cron_tasks(dir: &Path) -> Vec<CronTask> {
    let mut tasks = read_disk_tasks(dir).await;
    tasks.extend(read_memory_tasks_for(dir));
    tasks
}

/// Sync variant — merges disk + memory.
pub fn read_cron_tasks_sync(dir: &Path) -> Vec<CronTask> {
    let mut tasks = read_disk_tasks_sync(dir);
    tasks.extend(read_memory_tasks_for(dir));
    tasks
}

fn parse_cron_file(raw: &str) -> Vec<CronTask> {
    let file: CronFile = match serde_json::from_str(raw) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    file.tasks
        .into_iter()
        .filter(|t| {
            if parse_cron_expression(&t.cron).is_none() {
                tracing::debug!(id = %t.id, cron = %t.cron, "skipping task with invalid cron");
                return false;
            }
            true
        })
        .collect()
}

/// Check if the cron file has any valid tasks (sync).
pub fn has_cron_tasks_sync(dir: &Path) -> bool {
    !read_cron_tasks_sync(dir).is_empty()
}

/// Overwrite .claude/scheduled_tasks.json with the given tasks.
/// Creates .claude/ if missing.
pub async fn write_cron_tasks(tasks: &[CronTask], dir: &Path) -> std::io::Result<()> {
    let path = get_cron_file_path(dir);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let body = CronFile {
        tasks: tasks.to_vec(),
    };
    let json = serde_json::to_string_pretty(&body)?;
    tokio::fs::write(&path, format!("{}\n", json)).await
}

/// Append a task. Returns the generated id.
/// If `durable` is true, persists to disk; otherwise in-memory only (session-scoped).
pub async fn add_cron_task(
    cron: &str,
    prompt: &str,
    recurring: bool,
    durable: bool,
    dir: &Path,
) -> std::io::Result<String> {
    let id = Uuid::new_v4().to_string()[..8].to_string();
    let task = CronTask {
        id: id.clone(),
        cron: cron.to_string(),
        prompt: prompt.to_string(),
        created_at: chrono::Utc::now().timestamp_millis(),
        last_fired_at: None,
        recurring,
        permanent: false,
        durable,
    };
    if durable {
        let mut tasks = read_disk_tasks(dir).await;
        tasks.push(task);
        write_cron_tasks(&tasks, dir).await?;
    } else {
        with_memory_tasks(dir, |list| list.push(task));
    }
    Ok(id)
}

/// Remove tasks by id from both memory and disk. No-op if none match.
pub async fn remove_cron_tasks(ids: &[String], dir: &Path) -> std::io::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let id_set: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();

    // Remove from memory
    with_memory_tasks(dir, |list| list.retain(|t| !id_set.contains(t.id.as_str())));

    // Remove from disk
    let disk_tasks = read_disk_tasks(dir).await;
    let remaining: Vec<CronTask> = disk_tasks
        .into_iter()
        .filter(|t| !id_set.contains(t.id.as_str()))
        .collect();
    write_cron_tasks(&remaining, dir).await
}

/// Stamp `lastFiredAt` on the given recurring tasks in both memory and disk.
pub async fn mark_cron_tasks_fired(
    ids: &[String],
    fired_at: i64,
    dir: &Path,
) -> std::io::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let id_set: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();

    // Update memory tasks
    with_memory_tasks(dir, |list| {
        for t in list.iter_mut() {
            if id_set.contains(t.id.as_str()) {
                t.last_fired_at = Some(fired_at);
            }
        }
    });

    // Update disk tasks
    let mut disk_tasks = read_disk_tasks(dir).await;
    let mut changed = false;
    for t in &mut disk_tasks {
        if id_set.contains(t.id.as_str()) {
            t.last_fired_at = Some(fired_at);
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    write_cron_tasks(&disk_tasks, dir).await
}

/// Next fire time in epoch ms, strictly after `from_ms`.
pub fn next_cron_run_ms(cron: &str, from_ms: i64) -> Option<i64> {
    crate::cron::next_cron_run_ms(cron, from_ms)
}

/// Stable jitter fraction from task id (8-hex UUID slice → [0, 1)).
fn jitter_frac(task_id: &str) -> f64 {
    let hex = &task_id[..task_id.len().min(8)];
    let n = u32::from_str_radix(hex, 16).unwrap_or(0);
    n as f64 / 0x1_0000_0000_u64 as f64
}

/// Next fire time with forward jitter for recurring tasks.
pub fn jittered_next_cron_run_ms(
    cron: &str,
    from_ms: i64,
    task_id: &str,
    cfg: &CronJitterConfig,
) -> Option<i64> {
    let t1 = next_cron_run_ms(cron, from_ms)?;
    let t2 = next_cron_run_ms(cron, t1)?;
    let jitter_raw = jitter_frac(task_id) * cfg.recurring_frac * (t2 - t1) as f64;
    let jitter = if jitter_raw.is_finite() && jitter_raw >= 0.0 {
        (jitter_raw as i64).min(cfg.recurring_cap_ms)
    } else {
        0
    };
    Some(t1.saturating_add(jitter))
}

/// Next fire time with backward jitter for one-shot tasks.
pub fn one_shot_jittered_next_cron_run_ms(
    cron: &str,
    from_ms: i64,
    task_id: &str,
    cfg: &CronJitterConfig,
) -> Option<i64> {
    let t1 = next_cron_run_ms(cron, from_ms)?;
    let dt = chrono::DateTime::from_timestamp_millis(t1)?;
    let local = dt.with_timezone(&chrono::Local);
    if local.minute() % cfg.one_shot_minute_mod != 0 {
        return Some(t1);
    }
    let lead = jitter_frac(task_id)
        .mul_add(
            (cfg.one_shot_max_ms - cfg.one_shot_floor_ms) as f64,
            cfg.one_shot_floor_ms as f64,
        )
        .clamp(0.0, cfg.one_shot_max_ms as f64);
    Some(t1.saturating_sub(lead as i64).max(from_ms))
}

/// Find missed tasks — tasks whose next run is in the past.
pub fn find_missed_tasks(tasks: &[CronTask], now_ms: i64) -> Vec<&CronTask> {
    tasks
        .iter()
        .filter(|t| {
            if let Some(next) = next_cron_run_ms(&t.cron, t.created_at) {
                next < now_ms
            } else {
                false
            }
        })
        .collect()
}

/// Check if a recurring task has aged out.
pub fn is_recurring_task_aged(task: &CronTask, now_ms: i64, max_age_ms: i64) -> bool {
    if max_age_ms == 0 {
        return false;
    }
    task.recurring && !task.permanent && (now_ms - task.created_at) >= max_age_ms
}

/// Build the missed-task notification text.
pub fn build_missed_task_notification(missed: &[&CronTask]) -> String {
    let plural = missed.len() > 1;
    let header = format!(
        "The following one-shot scheduled task{} missed while Claude was not running. \
         {} already been removed from .claude/scheduled_tasks.json.\n\n\
         Do NOT execute {} yet. \
         First use the AskUserQuestion tool to ask whether to run {} now. \
         Only execute if the user confirms.",
        if plural { "s were" } else { " was" },
        if plural { "They have" } else { "It has" },
        if plural {
            "these prompts"
        } else {
            "this prompt"
        },
        if plural { "each one" } else { "it" },
    );

    let blocks: Vec<String> = missed
        .iter()
        .map(|t| {
            let human = crate::cron::cron_to_human(&t.cron);
            let created = chrono::DateTime::from_timestamp_millis(t.created_at)
                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            let meta = format!("[{}, created {}]", human, created);
            format!("{}\n```\n{}\n```", meta, t.prompt)
        })
        .collect();

    format!("{}\n\n{}", header, blocks.join("\n\n"))
}

/// Default max age in days for display in tool prompts.
pub fn default_max_age_days() -> u64 {
    let cfg = CronJitterConfig::default();
    (cfg.recurring_max_age_ms / (24 * 60 * 60 * 1000)) as u64
}

/// Maximum number of scheduled jobs.
pub const MAX_CRON_JOBS: usize = 50;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_add_and_read() {
        let dir = TempDir::new().unwrap();
        let id = add_cron_task("*/5 * * * *", "check status", true, true, dir.path())
            .await
            .unwrap();
        assert_eq!(id.len(), 8);

        let tasks = read_cron_tasks(dir.path()).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id);
        assert_eq!(tasks[0].prompt, "check status");
        assert!(tasks[0].recurring);
    }

    #[tokio::test]
    async fn test_remove() {
        let dir = TempDir::new().unwrap();
        let id1 = add_cron_task("*/5 * * * *", "task1", false, true, dir.path())
            .await
            .unwrap();
        let _id2 = add_cron_task("0 9 * * *", "task2", true, true, dir.path())
            .await
            .unwrap();

        remove_cron_tasks(&[id1], dir.path()).await.unwrap();
        let tasks = read_cron_tasks(dir.path()).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].prompt, "task2");
    }

    #[tokio::test]
    async fn test_mark_fired() {
        let dir = TempDir::new().unwrap();
        let id = add_cron_task("*/5 * * * *", "task1", true, true, dir.path())
            .await
            .unwrap();

        mark_cron_tasks_fired(std::slice::from_ref(&id), 1234567890, dir.path())
            .await
            .unwrap();

        let tasks = read_cron_tasks(dir.path()).await;
        assert_eq!(tasks[0].last_fired_at, Some(1234567890));
    }

    #[test]
    fn test_jitter_frac() {
        let f = jitter_frac("00000000");
        assert!((f - 0.0).abs() < f64::EPSILON);

        let f = jitter_frac("80000000");
        assert!((f - 0.5).abs() < 0.01);

        let f = jitter_frac("ffffffff");
        assert!((f - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_is_recurring_task_aged() {
        let now = 1_000_000_000;
        let task = CronTask {
            id: "test".to_string(),
            cron: "* * * * *".to_string(),
            prompt: "p".to_string(),
            created_at: now - 8 * 24 * 60 * 60 * 1000, // 8 days ago
            last_fired_at: None,
            recurring: true,
            permanent: false,
            durable: true,
        };
        assert!(is_recurring_task_aged(&task, now, 7 * 24 * 60 * 60 * 1000));
    }

    #[test]
    fn test_permanent_not_aged() {
        let now = 1_000_000_000;
        let task = CronTask {
            id: "test".to_string(),
            cron: "* * * * *".to_string(),
            prompt: "p".to_string(),
            created_at: now - 30 * 24 * 60 * 60 * 1000,
            last_fired_at: None,
            recurring: true,
            permanent: true,
            durable: true,
        };
        assert!(!is_recurring_task_aged(&task, now, 7 * 24 * 60 * 60 * 1000));
    }

    #[test]
    fn test_find_missed() {
        let now = chrono::Utc::now().timestamp_millis();
        let old_task = CronTask {
            id: "t1".to_string(),
            cron: "0 9 * * *".to_string(),
            prompt: "morning".to_string(),
            created_at: now - 2 * 24 * 60 * 60 * 1000, // 2 days ago
            last_fired_at: None,
            recurring: false,
            permanent: false,
            durable: true,
        };
        let tasks = [old_task];
        let missed = find_missed_tasks(&tasks, now);
        assert_eq!(missed.len(), 1);
    }

    #[test]
    fn test_build_missed_notification() {
        let task = CronTask {
            id: "abc".to_string(),
            cron: "0 9 * * *".to_string(),
            prompt: "check deploy".to_string(),
            created_at: 1700000000000,
            last_fired_at: None,
            recurring: false,
            permanent: false,
            durable: true,
        };
        let notif = build_missed_task_notification(&[&task]);
        assert!(notif.contains("missed while Claude was not running"));
        assert!(notif.contains("check deploy"));
    }

    #[tokio::test]
    async fn test_read_empty_dir() {
        let dir = TempDir::new().unwrap();
        let tasks = read_cron_tasks(dir.path()).await;
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_invalid_cron_filtered() {
        let dir = TempDir::new().unwrap();
        let path = get_cron_file_path(dir.path());
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        let json = r#"{"tasks":[{"id":"a","cron":"bad","prompt":"p","createdAt":0}]}"#;
        tokio::fs::write(&path, json).await.unwrap();

        let tasks = read_cron_tasks(dir.path()).await;
        assert!(tasks.is_empty());
    }

    #[test]
    fn test_has_cron_tasks_sync_empty() {
        let dir = TempDir::new().unwrap();
        assert!(!has_cron_tasks_sync(dir.path()));
    }

    #[test]
    fn test_default_max_age_days() {
        assert_eq!(default_max_age_days(), 7);
    }

    #[tokio::test]
    async fn test_non_durable_in_memory_only() {
        let dir = TempDir::new().unwrap();
        clear_memory_cron_tasks_for(dir.path());

        let id = add_cron_task("*/5 * * * *", "memory task", true, false, dir.path())
            .await
            .unwrap();

        // read_cron_tasks merges disk + memory
        let all = read_cron_tasks(dir.path()).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
        assert!(!all[0].durable);

        // Disk should be empty (non-durable not persisted)
        let disk = read_disk_tasks(dir.path()).await;
        assert!(disk.is_empty());

        // Clean up
        clear_memory_cron_tasks_for(dir.path());
    }
}
