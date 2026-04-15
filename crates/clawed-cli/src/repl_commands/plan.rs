//! `/plan` command handler — plan mode and plan file management.

use std::path::{Path, PathBuf};

use clawed_agent::engine::QueryEngine;
use clawed_core::permissions::PermissionMode;

use crate::theme;

/// Handle `/plan [args]`.
///
/// - No args: toggle plan mode (enter/exit).
/// - `open`: open plan file in `$EDITOR`.
/// - `show`/`view`: display current plan.
/// - Other text: enable plan mode with that description as initial plan.
pub(crate) async fn handle_plan_command(args: &str, engine: &QueryEngine, cwd: &Path) {
    let args = args.trim();

    match args {
        "" => println!("{}", toggle_plan_mode(engine).await),
        "open" => match open_plan_in_editor(cwd) {
            Ok(message) => println!("{}", message),
            Err(message) => eprintln!("{}", message),
        },
        "show" | "view" => match show_plan_text(cwd) {
            Ok(Some(content)) => println!("{}", content),
            Ok(None) => println!(
                "{}No plan file found. Use /plan open to create one.{}",
                theme::DIM,
                theme::RESET
            ),
            Err(message) => eprintln!("{}", message),
        },
        description => match save_plan_description(engine, cwd, description).await {
            Ok(message) => println!("{}", message),
            Err(message) => eprintln!("{}", message),
        },
    }
}

pub(crate) async fn toggle_plan_mode(engine: &QueryEngine) -> String {
    let in_plan_mode = {
        let state = engine.state().read().await;
        state.permission_mode == PermissionMode::Plan
    };

    if in_plan_mode {
        let mut state = engine.state().write().await;
        state.exit_plan_mode();
        format!(
            "{}📋 Plan mode disabled{}\n{}  Switched back to previous permission mode.{}",
            theme::c_tool(),
            theme::RESET,
            theme::DIM,
            theme::RESET
        )
    } else {
        let mut state = engine.state().write().await;
        state.enter_plan_mode();
        format!(
            "{}📋 Plan mode enabled{}\n{}  Tools restricted to read-only. Describe your goal and the AI will create a plan.{}\n{}  Use /plan again to exit plan mode.{}",
            theme::c_tool(),
            theme::RESET,
            theme::DIM,
            theme::RESET,
            theme::DIM,
            theme::RESET
        )
    }
}

pub(crate) fn open_plan_in_editor(cwd: &Path) -> Result<String, String> {
    let plan_path = get_plan_path(cwd);
    ensure_plan_file_exists(&plan_path)?;

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "notepad".to_string()
            } else {
                "vi".to_string()
            }
        });

    match std::process::Command::new(&editor).arg(&plan_path).status() {
        Ok(status) if status.success() => Ok(format!(
            "{}✓ Plan file saved: {}{}",
            theme::c_ok(),
            plan_path.display(),
            theme::RESET
        )),
        Ok(status) => Err(format!(
            "{}Editor exited with: {}{}",
            theme::c_err(),
            status,
            theme::RESET
        )),
        Err(error) => Err(format!(
            "{}Failed to open editor '{}': {}{}\n{}  Set $EDITOR to your preferred editor.{}",
            theme::c_err(),
            editor,
            error,
            theme::RESET,
            theme::DIM,
            theme::RESET
        )),
    }
}

pub(crate) fn show_plan_text(cwd: &Path) -> Result<Option<String>, String> {
    let plan_path = get_plan_path(cwd);
    if !plan_path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&plan_path).map_err(|error| {
        format!(
            "{}Failed to read plan: {}{}",
            theme::c_err(),
            error,
            theme::RESET
        )
    })?;

    Ok(Some(format!(
        "{}Current Plan{}\n{}{}{}\n\n{}",
        theme::BOLD,
        theme::RESET,
        theme::DIM,
        plan_path.display(),
        theme::RESET,
        content
    )))
}

pub(crate) async fn save_plan_description(
    engine: &QueryEngine,
    cwd: &Path,
    description: &str,
) -> Result<String, String> {
    let in_plan_mode = {
        let state = engine.state().read().await;
        state.permission_mode == PermissionMode::Plan
    };

    if !in_plan_mode {
        let mut state = engine.state().write().await;
        state.enter_plan_mode();
    }

    let plan_path = get_plan_path(cwd);
    if let Some(parent) = plan_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "{}Failed to create plan directory: {}{}",
                theme::c_err(),
                error,
                theme::RESET
            )
        })?;
    }

    let content = format!("# Plan\n\n{}\n", description);
    std::fs::write(&plan_path, content).map_err(|error| {
        format!(
            "{}Failed to write plan: {}{}",
            theme::c_err(),
            error,
            theme::RESET
        )
    })?;

    Ok(format!(
        "{}📋 Plan mode enabled{}\n{}  Plan: {}{}\n{}  Saved to: {}{}",
        theme::c_tool(),
        theme::RESET,
        theme::DIM,
        description,
        theme::RESET,
        theme::DIM,
        plan_path.display(),
        theme::RESET
    ))
}

/// Get the plan file path for the current project.
pub(crate) fn get_plan_path(cwd: &Path) -> PathBuf {
    let base =
        clawed_core::config::Settings::claude_dir().unwrap_or_else(|| PathBuf::from(".claude"));
    let plans_dir = base.join("plans");
    let slug = cwd_to_slug(cwd);
    plans_dir.join(format!("{}.md", slug))
}

fn ensure_plan_file_exists(plan_path: &Path) -> Result<(), String> {
    if plan_path.exists() {
        return Ok(());
    }

    if let Some(parent) = plan_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "{}Failed to create plan directory: {}{}",
                theme::c_err(),
                error,
                theme::RESET
            )
        })?;
    }

    std::fs::write(plan_path, "# Plan\n\n_Describe your goals here._\n").map_err(|error| {
        format!(
            "{}Failed to create plan file: {}{}",
            theme::c_err(),
            error,
            theme::RESET
        )
    })
}

/// Convert a cwd path to a filesystem-safe slug.
fn cwd_to_slug(cwd: &Path) -> String {
    let path = cwd.to_string_lossy();
    path.chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cwd_to_slug() {
        let slug = cwd_to_slug(Path::new("/home/user/project"));
        assert!(!slug.is_empty());
        assert!(!slug.contains('/'));
        assert!(!slug.contains('\\'));
    }

    #[test]
    fn test_cwd_to_slug_windows() {
        let slug = cwd_to_slug(Path::new("C:\\Users\\gxh\\project"));
        assert!(!slug.is_empty());
        assert!(!slug.contains('\\'));
        assert!(!slug.contains(':'));
    }
}
