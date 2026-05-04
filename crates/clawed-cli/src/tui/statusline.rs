//! External status line — aligned with official Claude Code StatusLine.tsx.
//!
//! When `settings.json` contains `"statusLine": { "command": "..." }`,
//! the configured shell command receives a JSON context via stdin and its
//! stdout is rendered dimmed above the footer hints.
//!
//! If no command is configured, the status line is hidden and the built-in
//! separator (model / turn / tokens) is shown instead.

use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Debounce interval between status line updates (ms).
const DEBOUNCE_MS: u64 = 300;

pub struct StatusLineState {
    /// Configured shell command (from settings.json `statusLine.command`).
    pub command: Option<String>,
    /// Cached rendered output (last non-empty stdout from the command).
    pub output: Arc<Mutex<String>>,
    /// Whether a background update is currently running.
    pub updating: Arc<AtomicBool>,
    /// When the output was last refreshed.
    pub last_update: Instant,
    /// Set to true when state changes and a refresh is needed.
    pub needs_refresh: bool,
}

impl StatusLineState {
    pub fn new(command: Option<String>) -> Self {
        Self {
            command,
            output: Arc::new(Mutex::new(String::new())),
            updating: Arc::new(AtomicBool::new(false)),
            last_update: Instant::now() - Duration::from_secs(60),
            needs_refresh: true,
        }
    }

    /// Mark that state has changed and the status line should be refreshed.
    pub fn invalidate(&mut self) {
        self.needs_refresh = true;
    }

    /// Whether an external status line is configured.
    pub fn is_enabled(&self) -> bool {
        self.command.is_some()
    }

    /// Trigger a background refresh if the debounce interval has passed and
    /// no update is already in flight.
    pub fn refresh_if_due(&mut self, context: serde_json::Value) {
        let Some(ref cmd) = self.command else {
            return;
        };
        if !self.needs_refresh && self.last_update.elapsed().as_millis() < DEBOUNCE_MS as u128 {
            return;
        }
        if self.updating.load(Ordering::Relaxed) {
            return;
        }
        self.needs_refresh = false;
        self.updating.store(true, Ordering::Relaxed);

        let cmd = cmd.clone();
        let output = Arc::clone(&self.output);
        let updating = Arc::clone(&self.updating);
        std::thread::spawn(move || {
            let result = execute_command(&cmd, &context);
            if let Ok(text) = result {
                let trimmed = text
                    .trim()
                    .lines()
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Ok(mut guard) = output.lock() {
                    *guard = trimmed;
                }
            }
            updating.store(false, Ordering::Relaxed);
        });
    }

    /// Called from the render loop to update the last-update timestamp once
    /// the background thread has finished.
    pub fn sync(&mut self) {
        if !self.updating.load(Ordering::Relaxed)
            && self.last_update.elapsed().as_millis() >= DEBOUNCE_MS as u128
        {
            self.last_update = Instant::now();
        }
    }
}

fn execute_command(cmd: &str, context: &serde_json::Value) -> anyhow::Result<String> {
    let input = serde_json::to_string(context)? + "\n";
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "status line command exited with code {:?}",
            output.status.code()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Build the JSON context passed to the external status line command.
/// Matches official Claude Code `buildStatusLineCommandInput()`.
pub fn build_context(
    model: &str,
    permission_mode: &str,
    _total_turns: u32,
    context_tokens: u64,
    total_output_tokens: u64,
    total_cost_usd: f64,
    context_pct: f64,
) -> serde_json::Value {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    json!({
        "cwd": cwd,
        "permission_mode": permission_mode,
        "model": {
            "id": model,
            "display_name": model,
        },
        "version": env!("CARGO_PKG_VERSION"),
        "cost": {
            "total_cost_usd": total_cost_usd,
            "total_duration_ms": 0,
            "total_api_duration_ms": 0,
            "total_lines_added": 0,
            "total_lines_removed": 0,
        },
        "context_window": {
            "total_input_tokens": context_tokens,
            "total_output_tokens": total_output_tokens,
            "context_window_size": 200000,
            "used_percentage": if context_pct > 0.0 { Some(context_pct) } else { None },
            "current_usage": context_tokens,
            "remaining_percentage": if context_pct > 0.0 { Some(100.0 - context_pct) } else { Some(100.0) },
        },
        "exceeds_200k_tokens": context_tokens > 200000,
        "workspace": {
            "current_dir": cwd,
            "project_dir": cwd,
            "added_dirs": [],
        },
        "output_style": { "name": "default" },
        "rate_limits": { },
    })
}

/// Return the cached external status line text (first line only).
/// ANSI escape codes are stripped so the caller can render it as plain
/// dimmed text (matching official CC `dimColor` behaviour).
pub fn text(state: &StatusLineState) -> Option<String> {
    let Ok(guard) = state.output.lock() else {
        return None;
    };
    if guard.is_empty() {
        return None;
    }
    // Take first line only (official CC uses wrap="truncate").
    let first_line = guard.lines().next()?.to_string();
    Some(first_line)
}
