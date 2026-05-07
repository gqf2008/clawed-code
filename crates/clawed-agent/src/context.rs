//! Session-start context injection (git status, date).
//!
//! Mirrors the TS `getSystemContext()` / `getUserContext()` from `context.ts`.
//! Context is injected as a `<system-reminder>` user message prepended before
//! the first user message of a conversation, so it participates in compaction.

use std::path::Path;
use std::process::Stdio;

/// Maximum characters for `git status --short` output before truncating.
const MAX_STATUS_CHARS: usize = 2048;

/// Build the combined session context string (date + git status).
///
/// Returns `None` if nothing could be collected.
pub async fn build_session_context(cwd: &Path) -> Option<String> {
    let date_str = local_date_string();
    let git_status = git_status_snapshot(cwd).await;

    if date_str.is_none() && git_status.is_none() {
        return None;
    }

    let mut parts = Vec::new();

    if let Some(date) = date_str {
        parts.push(format!("currentDate\n{}", date));
    }

    if let Some(git) = git_status {
        parts.push(format!("gitStatus\n{}", git));
    }

    Some(parts.join("\n\n"))
}

fn local_date_string() -> Option<String> {
    Some(format!(
        "Today's date is {}.",
        chrono::Local::now().format("%Y/%m/%d")
    ))
}

/// Collect a git status snapshot: branch, main branch, status, recent commits.
async fn git_status_snapshot(cwd: &Path) -> Option<String> {
    // Run all git commands concurrently (all local — no network I/O).
    // If cwd is not a git repo, `branch` will fail and we return None.
    let branch_fut = git_output(cwd, &["branch", "--show-current"]);
    let main_ref_fut = git_output(cwd, &["symbolic-ref", "refs/remotes/origin/HEAD"]);
    let status_fut = git_output(cwd, &["--no-optional-locks", "status", "--short"]);
    let log_fut = git_output(cwd, &["log", "--oneline", "-n", "5"]);
    let user_fut = git_output(cwd, &["config", "user.name"]);

    let (branch, main_ref, status, log, user_name) =
        tokio::join!(branch_fut, main_ref_fut, status_fut, log_fut, user_fut);

    // symbolic-ref returns e.g. "refs/remotes/origin/main" — extract branch name.
    // Falls back to checking common names if no origin HEAD ref exists.
    let main_branch = main_ref.ok().and_then(|s| {
        s.trim()
            .strip_prefix("refs/remotes/origin/")
            .map(String::from)
    });

    let branch = branch.ok()?;
    let status = status.unwrap_or_default();
    let log = log.unwrap_or_default();

    // Resolve main branch: prefer symbolic-ref result, fallback to checking common names
    let main_branch = match main_branch {
        Some(mb) => Some(mb),
        None => resolve_main_branch(cwd).await,
    };

    let truncated_status = if status.len() > MAX_STATUS_CHARS {
        clawed_core::text_util::truncate_chars(
            &status,
            MAX_STATUS_CHARS,
            "\n... (truncated because it exceeds 2k characters. \
             If you need more information, run \"git status\" using Bash)",
        )
    } else {
        status
    };

    let mut lines = vec![
        "This is the git status at the start of the conversation. \
         Note that this status is a snapshot in time, and will not update \
         during the conversation."
            .to_string(),
        format!("Current branch: {}", branch),
    ];

    if let Some(main) = main_branch {
        lines.push(format!(
            "Main branch (you will usually use this for PRs): {}",
            main
        ));
    }

    if let Ok(user) = user_name {
        if !user.is_empty() {
            lines.push(format!("Git user: {}", user));
        }
    }

    lines.push(format!(
        "Status:\n{}",
        if truncated_status.is_empty() {
            "(clean)".to_string()
        } else {
            truncated_status
        }
    ));
    lines.push(format!("Recent commits:\n{}", log));

    Some(lines.join("\n\n"))
}

async fn resolve_main_branch(cwd: &Path) -> Option<String> {
    for name in &["main", "master"] {
        let check = git_output(cwd, &["rev-parse", "--verify", name]).await;
        if check.is_ok() {
            return Some((*name).to_string());
        }
    }
    None
}

async fn git_output(cwd: &Path, args: &[&str]) -> Result<String, ()> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .map_err(|_| ())?;

    if !output.status.success() {
        return Err(());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Wrap context as a `<system-reminder>` user message, matching the TS format.
pub fn format_context_message(context: &str) -> String {
    format!(
        "<system-reminder>\n\
         As you answer the user's questions, you can use the following context:\n\
         {}\n\n\
         IMPORTANT: this context may or may not be relevant to your tasks. \
         You should not respond to this context unless it is highly relevant to your task.\n\
         </system-reminder>",
        context
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_context_message() {
        let msg = format_context_message("currentDate\nToday's date is 2026/04/30.");
        assert!(msg.starts_with("<system-reminder>"));
        assert!(msg.contains("currentDate"));
        assert!(msg.contains("2026/04/30"));
        assert!(msg.contains("IMPORTANT"));
        assert!(msg.ends_with("</system-reminder>"));
    }

    #[test]
    fn test_local_date_string() {
        let date = local_date_string().unwrap();
        assert!(date.starts_with("Today's date is"));
        assert!(date.contains("202")); // Year starts with 202x
    }
}
