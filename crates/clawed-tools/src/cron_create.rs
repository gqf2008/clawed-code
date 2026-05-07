//! CronCreate tool — schedule a prompt to run at a future time.

use async_trait::async_trait;
use serde_json::{json, Value};

use clawed_core::cron::{cron_to_human, parse_cron_expression};
use clawed_core::cron_tasks::{
    add_cron_task, default_max_age_days, next_cron_run_ms, read_cron_tasks, MAX_CRON_JOBS,
};
use clawed_core::tool::{Tool, ToolContext, ToolResult};

/// Schedule a prompt to run at a future time (one-shot or recurring).
pub struct CronCreateTool;

#[async_trait]
impl Tool for CronCreateTool {
    fn name(&self) -> &'static str {
        "CronCreate"
    }

    fn description(&self) -> &'static str {
        "Schedule a prompt to run at a future time. Uses standard 5-field cron in the user's local timezone: \
         minute hour day-of-month month day-of-week. \"0 9 * * *\" means 9am local — no timezone conversion needed. \
         \n\n## One-shot tasks (recurring: false)\n\nFor \"remind me at X\" or \"at <time>, do Y\" requests — fire once then auto-delete. \
         Pin minute/hour/day-of-month/month to specific values.\n \
         \"remind me at 2:30pm today to check the deploy\" → cron: \"30 14 <today_dom> <today_month> *\", recurring: false\n \
         \"tomorrow morning, run the smoke test\" → cron: \"57 8 <tomorrow_dom> <tomorrow_month> *\", recurring: false\n \
         \n\n## Recurring jobs (recurring: true, the default)\n\nFor \"every N minutes\" / \"every hour\" / \"weekdays at 9am\" requests. \
         \n \"*/5 * * * *\" (every 5 min), \"0 * * * *\" (hourly), \"0 9 * * 1-5\" (weekdays at 9am local)\n \
         \n\n## Avoid the :00 and :30 minute marks when the task allows it.\n \
         Every user who asks for \"9am\" gets `0 9`, and every user who asks for \"hourly\" gets `0 *` — which means requests \
         from across the planet land on the API at the same instant. When the user's request is approximate, pick a minute that is NOT 0 or 30.\n \
         \"every morning around 9\" → \"57 8 * * *\" or \"3 9 * * *\" (not \"0 9 * * *\")\n \
         \"hourly\" → \"7 * * * *\" (not \"0 * * * *\")\n \
         \"in an hour or so, remind me to...\" → pick whatever minute you land on, don't round\n \
         \nOnly use minute 0 or 30 when the user names that exact time and clearly means it (\"at 9:00 sharp\", \"at half past\", coordinating with a meeting). \
         When in doubt, nudge a few minutes early or late.\n \
         \n\n## Session-only\n\nJobs live only in this Claude session — nothing is written to disk, and the job is gone when Claude exits. \
         \n\n## Runtime behavior\n\nJobs only fire while the REPL is idle (not mid-query). The scheduler adds a small deterministic jitter on top of whatever you pick: \
         recurring tasks fire up to 10% of their period late (max 15 min); one-shot tasks landing on :00 or :30 fire up to 90 s early. Picking an off-minute is still the bigger lever.\n \
         \nRecurring tasks auto-expire after 7 days — they fire one final time, then are deleted. Tell the user about the 7-day limit when scheduling recurring jobs.\n \
         \nReturns a job ID you can pass to CronDelete."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "cron": {
                    "type": "string",
                    "description": "Standard 5-field cron expression in local time: \"M H DoM Mon DoW\" (e.g. \"*/5 * * * *\" = every 5 minutes)."
                },
                "prompt": {
                    "type": "string",
                    "description": "The prompt to enqueue at each fire time."
                },
                "recurring": {
                    "type": "boolean",
                    "description": "true (default) = fire on every cron match until deleted or auto-expired. false = fire once at the next match, then auto-delete. Use false for \"remind me at X\" one-shot requests with pinned minute/hour/dom/month."
                },
                "durable": {
                    "type": "boolean",
                    "description": "true = persist to .claude/scheduled_tasks.json and survive restarts. false (default) = in-memory only, dies when this Claude session ends. Use true only when the user asks for the task to survive across sessions."
                }
            },
            "required": ["cron", "prompt"],
            "additionalProperties": false
        })
    }

    async fn call(&self, input: Value, context: &ToolContext) -> anyhow::Result<ToolResult> {
        let cron_expr = input["cron"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'cron'"))?;
        let prompt = input["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'prompt'"))?;
        let recurring = input["recurring"].as_bool().unwrap_or(true);
        let durable = input["durable"].as_bool().unwrap_or(false);

        // Validate cron expression
        if parse_cron_expression(cron_expr).is_none() {
            return Ok(ToolResult::error(format!(
                "Invalid cron expression '{}'. Expected 5 fields: M H DoM Mon DoW.",
                cron_expr
            )));
        }

        let now = chrono::Utc::now().timestamp_millis();
        if next_cron_run_ms(cron_expr, now).is_none() {
            return Ok(ToolResult::error(format!(
                "Cron expression '{}' does not match any calendar date in the next year.",
                cron_expr
            )));
        }

        let tasks = read_cron_tasks(&context.cwd).await;
        if tasks.len() >= MAX_CRON_JOBS {
            return Ok(ToolResult::error(format!(
                "Too many scheduled jobs (max {}). Cancel one first.",
                MAX_CRON_JOBS
            )));
        }

        let id = add_cron_task(cron_expr, prompt, recurring, durable, &context.cwd)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to add cron task: {}", e))?;

        let human = cron_to_human(cron_expr);
        let days = default_max_age_days();

        let kind = if recurring {
            "recurring job"
        } else {
            "one-shot task"
        };
        let session = if durable { "" } else { ", session-only" };
        let suffix = if recurring {
            format!(
                ". Auto-expires after {} days. Use CronDelete to cancel sooner.",
                days
            )
        } else {
            ". It will fire once then auto-delete.".to_string()
        };
        let msg = format!("Scheduled {} {} ({}{}{})", kind, id, human, session, suffix);

        Ok(ToolResult::text(msg))
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawed_core::permissions::PermissionMode;
    use clawed_core::tool::AbortSignal;
    use tempfile::TempDir;

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            cwd: dir.to_path_buf(),
            abort_signal: AbortSignal::new(),
            permission_mode: PermissionMode::Default,
            messages: vec![],
            output_line: None,
        }
    }

    #[tokio::test]
    async fn test_create_task() {
        let dir = TempDir::new().unwrap();
        let tool = CronCreateTool;

        let result = tool
            .call(
                json!({
                    "cron": "*/5 * * * *",
                    "prompt": "check status",
                    "recurring": true
                }),
                &ctx(dir.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        let text = result.to_text();
        assert!(text.contains("recurring job"));
        assert!(text.contains("Every 5 minutes"));
    }

    #[tokio::test]
    async fn test_invalid_cron() {
        let dir = TempDir::new().unwrap();
        let tool = CronCreateTool;

        let result = tool
            .call(
                json!({
                    "cron": "bad cron",
                    "prompt": "test"
                }),
                &ctx(dir.path()),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.to_text().contains("Invalid cron"));
    }
}
