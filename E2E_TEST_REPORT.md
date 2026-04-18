# E2E Functional Test Report — clawed CLI

**Date:** 2026-04-18
**Branch:** ux/claude-inspired-improvements
**Test Suite:** `crates/clawed-cli/tests/e2e_cli.rs`
**Framework:** `assert_cmd` + `predicates`

---

## Summary

| Metric | Value |
|--------|-------|
| E2E Tests | **22** |
| Passed | **22** |
| Failed | **0** |
| Unit Tests (clawed-cli) | **615** |
| Total Tests (clawed-cli) | **637** |

All E2E tests execute the compiled `clawed` binary in a subprocess with temporary directories and isolated `$HOME` to avoid side effects on developer environments.

---

## Scope

Commands that **require an API key** (prompt mode, `--resume`, `--session-id`) are excluded because they need network access and authentication. All other CLI subcommands and flags are covered.

---

## Test Groups

### Group A: Help & Version (4 tests)

| Test | Command | Assertion |
|------|---------|-----------|
| `e2e_help_shows_usage` | `--help` | stdout contains "Clawed Code", "Usage:", "Options:" |
| `e2e_help_short_flag` | `-h` | stdout contains "Clawed Code" |
| `e2e_version_shows_semver` | `--version` | stdout matches `clawed \d+\.\d+\.\d+` |
| `e2e_version_short_flag` | `-V` | stdout matches semver pattern |

**Result:** PASS

---

### Group B: Shell Completions (5 tests)

| Test | Command | Assertion |
|------|---------|-----------|
| `e2e_completions_bash` | `--completions bash` | stdout contains `_claude()` |
| `e2e_completions_zsh` | `--completions zsh` | stdout contains `#compdef claude` |
| `e2e_completions_fish` | `--completions fish` | stdout contains `complete -c claude` |
| `e2e_completions_powershell` | `--completions powershell` | stdout contains `Register-ArgumentCompleter` |
| `e2e_completions_elvish` | `--completions elvish` | stdout contains `edit:completion:arg-completer` |

**Result:** PASS

---

### Group C: Session Management (2 tests)

Uses a temporary `$HOME` directory so session lists are isolated.

| Test | Command | Assertion |
|------|---------|-----------|
| `e2e_list_sessions_empty` | `--list-sessions` | stdout contains "No saved sessions." |
| `e2e_search_sessions_no_match` | `--search-sessions nonexistent-query-xyz` | stdout contains "No sessions matching" |

**Result:** PASS

---

### Group D: Project Initialization (2 tests)

| Test | Command | Assertion |
|------|---------|-----------|
| `e2e_init_creates_claude_md_and_dirs` | `--init --cwd <tmp>` | Creates `CLAUDE.md`, `.claude/skills/`, `.claude/rules/` |
| `e2e_init_skips_existing_claude_md` | `--init --cwd <tmp>` (with existing file) | Skips overwrite, preserves existing content |

**Result:** PASS

---

### Group E: Argument Validation & Conflicts (2 tests)

| Test | Command | Assertion |
|------|---------|-----------|
| `e2e_repl_and_tui_conflict` | `--repl --tui` | exits with failure, stderr contains "cannot be used with" |
| `e2e_invalid_model_without_base_url` | `--model totally-fake-model --list-sessions` | exits successfully (early-exit bypasses validation) |

**Result:** PASS

---

### Group F: Flag Combinations — Smoke Tests (2 tests)

Verifies that various flags can be combined with early-exit commands without panics.

| Test | Variations |
|------|-----------|
| `e2e_combinations_with_list_sessions` | `--verbose`, `--model`, `--permission-mode bypass`, `--no-claude-md`, `--max-turns 10`, `--output-format text` |
| `e2e_combinations_with_completions` | `--verbose`, `--model` combined with `--completions bash/zsh` |

**Result:** PASS

---

### Group G: Output Format Flags (2 tests)

| Test | Command | Assertion |
|------|---------|-----------|
| `e2e_output_format_text` | `--list-sessions --output-format text` | stdout contains "No saved sessions." |
| `e2e_output_format_json` | `--list-sessions --output-format json` | stdout contains "No saved sessions." |

**Result:** PASS

---

### Group H: CWD Resolution (1 test)

| Test | Command | Assertion |
|------|---------|-----------|
| `e2e_cwd_flag_changes_directory` | `--init --cwd <tmp>` | `CLAUDE.md` created in the specified directory |

**Result:** PASS

---

### Group I: Edge Cases (2 tests)

| Test | Command | Assertion |
|------|---------|-----------|
| `e2e_no_args_starts_interactive_but_aborts_without_tty` | (no args) | exits within 2s (no hang) |
| `e2e_empty_prompt_arg` | `""` (empty string arg) | exits within 2s (no hang) |

**Result:** PASS

---

## How to Run

```bash
# All E2E tests only
cargo test -p clawed-cli --test e2e_cli

# All clawed-cli tests (unit + E2E)
cargo test -p clawed-cli

# Single E2E test
cargo test -p clawed-cli --test e2e_cli e2e_help_shows_usage
```

---

## Notes

- Invalid permission mode warnings are tested at the unit level (`config.rs`) because early-exit commands bypass the permission-mode parsing path.
- Model validation without `--base-url` is not triggered by early-exit commands; full validation is covered by integration tests that spawn the agent engine.
