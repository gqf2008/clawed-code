//! E2E CLI tests — spawn the `clawed` binary and verify command behavior.
//!
//! These tests cover all CLI subcommands and flags that do NOT require an API key.
//! Commands requiring API access (e.g. non-interactive prompt mode, --resume)
//! are out of scope for this test suite.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

fn clawed() -> Command {
    Command::cargo_bin("clawed").expect("clawed binary not found")
}

/// Run clawed with a temp home directory so session lists are isolated.
fn clawed_with_temp_home() -> (Command, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("create temp dir for home");
    let mut cmd = clawed();
    cmd.env("HOME", tmp.path());
    (cmd, tmp)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group A: Help & Version
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_help_shows_usage() {
    let mut cmd = clawed();
    cmd.arg("--help");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Clawed Code"))
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("Options:"));
}

#[test]
fn e2e_help_short_flag() {
    let mut cmd = clawed();
    cmd.arg("-h");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Clawed Code"));
}

#[test]
fn e2e_version_shows_semver() {
    let mut cmd = clawed();
    cmd.arg("--version");
    cmd.assert()
        .success()
        .stdout(predicate::str::is_match(r"clawed \d+\.\d+\.\d+").unwrap());
}

#[test]
fn e2e_version_short_flag() {
    let mut cmd = clawed();
    cmd.arg("-V");
    cmd.assert()
        .success()
        .stdout(predicate::str::is_match(r"clawed \d+\.\d+\.\d+").unwrap());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group B: Shell Completions
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_completions_bash() {
    let mut cmd = clawed();
    cmd.args(["--completions", "bash"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("_claude()"));
}

#[test]
fn e2e_completions_zsh() {
    let mut cmd = clawed();
    cmd.args(["--completions", "zsh"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("#compdef claude"));
}

#[test]
fn e2e_completions_fish() {
    let mut cmd = clawed();
    cmd.args(["--completions", "fish"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("complete -c claude"));
}

#[test]
fn e2e_completions_powershell() {
    let mut cmd = clawed();
    cmd.args(["--completions", "powershell"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Register-ArgumentCompleter"));
}

#[test]
fn e2e_completions_elvish() {
    let mut cmd = clawed();
    cmd.args(["--completions", "elvish"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("edit:completion:arg-completer"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group C: Session Management (no API key needed)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_list_sessions_empty() {
    let (mut cmd, _tmp) = clawed_with_temp_home();
    cmd.arg("--list-sessions");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("No saved sessions."));
}

#[test]
fn e2e_search_sessions_no_match() {
    let (mut cmd, _tmp) = clawed_with_temp_home();
    cmd.args(["--search-sessions", "nonexistent-query-xyz"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("No sessions matching"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group D: Project Initialization
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_init_creates_claude_md_and_dirs() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let mut cmd = clawed();
    cmd.args(["--init", "--cwd", tmp.path().to_str().unwrap()]);
    cmd.assert().success();

    let claude_md = tmp.path().join("CLAUDE.md");
    let skills_dir = tmp.path().join(".claude").join("skills");
    let rules_dir = tmp.path().join(".claude").join("rules");

    assert!(
        claude_md.exists(),
        "CLAUDE.md should be created in project root"
    );
    assert!(skills_dir.exists(), ".claude/skills should be created");
    assert!(rules_dir.exists(), ".claude/rules should be created");

    let md_content = fs::read_to_string(&claude_md).expect("read CLAUDE.md");
    assert!(
        md_content.contains("# CLAUDE.md"),
        "CLAUDE.md should contain header"
    );
}

#[test]
fn e2e_init_skips_existing_claude_md() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    fs::write(tmp.path().join("CLAUDE.md"), "# existing").expect("write existing CLAUDE.md");

    let mut cmd = clawed();
    cmd.args(["--init", "--cwd", tmp.path().to_str().unwrap()]);
    // Init succeeds even if CLAUDE.md exists — it skips the file and creates dirs
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("already exists"));

    // Existing CLAUDE.md should NOT be overwritten
    let content = fs::read_to_string(tmp.path().join("CLAUDE.md")).expect("read CLAUDE.md");
    assert_eq!(content, "# existing", "existing CLAUDE.md should not be overwritten");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group E: Argument Validation & Conflicts
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_repl_and_tui_conflict() {
    let mut cmd = clawed();
    cmd.args(["--repl", "--tui"]);
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

// NOTE: Invalid permission mode warning is tested at unit level in
// config.rs::test_parse_default_fallback. E2E testing is impractical because
// early-exit commands (--list-sessions, --completions, etc.) bypass the
// permission-mode parsing path entirely.

#[test]
fn e2e_invalid_model_without_base_url() {
    let (mut cmd, _tmp) = clawed_with_temp_home();
    cmd.args(["--model", "totally-fake-model", "--list-sessions"]);
    // --list-sessions exits before model validation, so this should succeed
    cmd.assert().success();
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group F: Flag Combinations (smoke tests)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_combinations_with_list_sessions() {
    // Various flags combined with --list-sessions (early-exit command)
    let cases = vec![
        vec!["--list-sessions", "--verbose"],
        vec!["--list-sessions", "--model", "claude-sonnet-4-20250514"],
        vec!["--list-sessions", "--permission-mode", "bypass"],
        vec!["--list-sessions", "--no-claude-md"],
        vec!["--list-sessions", "--max-turns", "10"],
        vec!["--list-sessions", "--output-format", "text"],
    ];

    for args in cases {
        let (mut cmd, _tmp) = clawed_with_temp_home();
        cmd.args(&args);
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("No saved sessions."));
    }
}

#[test]
fn e2e_combinations_with_completions() {
    let cases = vec![
        vec!["--completions", "bash", "--verbose"],
        vec!["--completions", "zsh", "--model", "claude-opus-4-20250514"],
    ];

    for args in cases {
        let mut cmd = clawed();
        cmd.args(&args);
        cmd.assert().success();
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group G: Output Format Flags (with early-exit commands)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_output_format_text() {
    let (mut cmd, _tmp) = clawed_with_temp_home();
    cmd.args(["--list-sessions", "--output-format", "text"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("No saved sessions."));
}

#[test]
fn e2e_output_format_json() {
    let (mut cmd, _tmp) = clawed_with_temp_home();
    cmd.args(["--list-sessions", "--output-format", "json"]);
    // --list-sessions bypasses output format handling, still prints plain text
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("No saved sessions."));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group H: CWD Resolution
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_cwd_flag_changes_directory() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let mut cmd = clawed();
    cmd.args([
        "--init",
        "--cwd",
        tmp.path().to_str().unwrap(),
    ]);
    cmd.assert().success();

    assert!(tmp.path().join("CLAUDE.md").exists());
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group I: Edge Cases
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_no_args_starts_interactive_but_aborts_without_tty() {
    // Running without args in non-TTY should fail gracefully or exit
    let mut cmd = clawed();
    // In CI/non-TTY, this may either fail or wait for input.
    // We use a timeout to avoid hanging.
    cmd.timeout(std::time::Duration::from_secs(2));
    let output = cmd.output().expect("run clawed with no args");
    // Should exit (either success or failure) within timeout
    assert!(
        output.status.code().is_some(),
        "clawed without args should exit, not hang forever"
    );
}

#[test]
fn e2e_empty_prompt_arg() {
    let mut cmd = clawed();
    cmd.arg("");
    cmd.timeout(std::time::Duration::from_secs(2));
    let output = cmd.output().expect("run clawed with empty prompt");
    assert!(
        output.status.code().is_some(),
        "clawed with empty prompt should exit"
    );
}
