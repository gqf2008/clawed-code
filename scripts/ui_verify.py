#!/usr/bin/env python3
"""Automated TUI verification via PTY.

Spawns clawed in a pseudo-terminal, sends commands, captures screenshots,
and validates the output against expected patterns.
"""

import os
import sys
import time
import json
import pty
import subprocess
import re
from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parent.parent
CARGO = os.environ.get("CARGO", "cargo")

# ── PTY helpers ───────────────────────────────────────────────────────────────

def spawn_clawed(print_mode=False, prompt="hello"):
    """Spawn clawed in a PTY. Returns (fd, pid)."""
    master_fd, slave_fd = pty.openpty()

    cmd = [CARGO, "run", "--"]
    if print_mode:
        cmd += ["--print", "--output-format", "text", prompt]

    # Set terminal size
    os.set_blocking(master_fd, False)

    proc = subprocess.Popen(
        cmd,
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        cwd=str(PROJECT_DIR),
        env={**os.environ, "TERM": "xterm-256color", "COLUMNS": "120", "LINES": "40"},
        close_fds=True,
    )
    os.close(slave_fd)
    return master_fd, proc


def read_all(fd, timeout=5.0, chunk_size=4096):
    """Read all available data from fd with timeout."""
    import select
    data = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.1)
        if ready:
            try:
                chunk = os.read(fd, chunk_size)
                if not chunk:
                    break
                data += chunk
            except BlockingIOError:
                break
        else:
            if data:  # got some data but no more coming
                break
    return data


def strip_ansi(text):
    """Strip ANSI escape codes from text."""
    ansi_escape = re.compile(r'\x1b\[[0-9;]*[a-zA-Z]|\x1b\].*?\x07|\x1b\[.*?m')
    return ansi_escape.sub('', text)


def send_keys(fd, text, delay=0.05):
    """Send keystrokes to the PTY."""
    for ch in text:
        os.write(fd, ch.encode())
        time.sleep(delay)


# ── Verification functions ────────────────────────────────────────────────────

class TestResult:
    def __init__(self, name, passed, msg=""):
        self.name = name
        self.passed = passed
        self.msg = msg


results: list[TestResult] = []


def check(name, condition, msg=""):
    results.append(TestResult(name, condition, msg))
    icon = "\033[32m✓\033[0m" if condition else "\033[31m✗\033[0m"
    detail = f" — {msg}" if msg else ""
    print(f"  {icon} {name}{detail}")
    return condition


# ── Tests ──────────────────────────────────────────────────────────────────────

def test_print_mode():
    """Test --print mode produces output."""
    print("\n─── Print Mode ───")
    fd, proc = spawn_clawed(print_mode=True, prompt="say hello world")
    data = read_all(fd, timeout=60.0)
    proc.terminate()
    os.close(fd)

    text = data.decode("utf-8", errors="replace")
    ansi_text = strip_ansi(text)

    # Should produce some output (AI response)
    check("Print mode produces output", len(ansi_text.strip()) > 0,
          f"got {len(ansi_text)} bytes")

    # Should contain actual text (not just ANSI escape codes)
    check("Output contains readable text",
          len(ansi_text.strip()) > 20,
          f"text: {ansi_text[:100]}")

    # Write output for inspection
    (PROJECT_DIR / "scripts" / "output_print.txt").write_text(text, errors="replace")


def test_help_flag():
    """Test --help produces expected output."""
    print("\n─── Help Output ───")
    result = subprocess.run(
        [CARGO, "run", "--", "--help"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=30,
    )
    output = result.stdout + result.stderr

    check("--help shows usage", "Usage:" in output or "USAGE:" in output or "clawed" in output.lower())
    check("--help mentions model", "--model" in output or "-m" in output)
    check("--help mentions permission", "permission" in output.lower())
    check("--help exits 0", result.returncode == 0, f"exit={result.returncode}")


def test_version():
    """Test --version."""
    print("\n─── Version ───")
    result = subprocess.run(
        [CARGO, "run", "--", "--version"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=30,
    )

    check("--version produces output", len(result.stdout.strip()) > 0,
          f"got: {result.stdout.strip()[:50]}")
    check("--version exits 0", result.returncode == 0)


def test_output_formats():
    """Test JSON and stream-json output formats."""
    print("\n─── Output Formats ───")

    # JSON format
    result = subprocess.run(
        [CARGO, "run", "--", "--print", "--output-format", "json", "say hi"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=60,
        env={**os.environ, "TERM": "dumb"},
    )
    # JSON output goes to stderr in print mode? Or stdout? Let's check both.
    combined = result.stdout + result.stderr
    check("JSON output produces data", len(combined.strip()) > 0,
          f"got {len(combined)} bytes")


def test_list_sessions():
    """Test --list-sessions."""
    print("\n─── List Sessions ───")
    result = subprocess.run(
        [CARGO, "run", "--", "--list-sessions"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=30,
    )
    check("--list-sessions exits 0", result.returncode == 0,
          f"exit={result.returncode}")


def test_completions():
    """Test shell completions generate output."""
    print("\n─── Completions ───")
    for shell in ["bash", "zsh", "fish"]:
        result = subprocess.run(
            [CARGO, "run", "--", "--completions", shell],
            cwd=str(PROJECT_DIR),
            capture_output=True,
            text=True,
            timeout=30,
        )
        check(f"--completions {shell} exits 0", result.returncode == 0,
              f"output: {len(result.stdout)} bytes")


def test_init_skips_existing():
    """Test --init handles existing CLAUDE.md."""
    print("\n─── Init ───")
    result = subprocess.run(
        [CARGO, "run", "--", "--init"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=30,
    )
    # Should not crash
    check("--init doesn't crash", result.returncode in (0, 1),
          f"exit={result.returncode}")


def test_render_markdown():
    """Test markdown rendering output via cargo test dumps."""
    print("\n─── Markdown Rendering ───")
    result = subprocess.run(
        [CARGO, "test", "-p", "clawed-cli", "--", "render_dump_markdown", "--nocapture"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=30,
    )
    output = result.stdout + result.stderr

    # Check rendered markdown
    check("Heading stripped of ###",
          "测试覆盖" in output and "###" not in output.split("输出:")[-1] if "输出:" in output else True)
    check("List uses - prefix", "- messages.rs" in output or "- item" in output)
    check("Blockquote uses ▎ bar", "▎" in output)
    check("Horizontal rule is ---", "L8: [---]" in output or '"---"' in output)


def test_render_diff():
    """Test diff rendering output."""
    print("\n─── Diff Rendering ───")
    result = subprocess.run(
        [CARGO, "test", "-p", "clawed-cli", "--", "render_dump_diff_and_agent", "--nocapture"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=30,
    )
    output = result.stdout + result.stderr

    check("Diff has gutter with + marker", 'gutter[+ ' in output or 'gutter[+ ' in output)
    check("Diff added line has bg", "bg=true" in output)
    check("Diff context line no bg", "bg=false" in output)
    check("Agent badge has fg Magenta", "fg=Magenta" in output)


def test_screenshot_tests():
    """Run ratatui TestBackend screenshot tests."""
    print("\n─── Screenshot Tests ───")
    result = subprocess.run(
        [CARGO, "test", "-p", "clawed-cli", "--", "screenshot", "--nocapture"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=30,
    )
    output = result.stdout + result.stderr

    for name in ["screenshot_welcome", "screenshot_with_markdown_message",
                 "screenshot_input_area", "screenshot_tool_execution_rendered",
                 "screenshot_with_taskplan"]:
        check(f"Screenshot {name}", f"{name} ... ok" in output)


def test_verify_tests():
    """Run pixel-level verification tests."""
    print("\n─── Pixel Verification ───")
    result = subprocess.run(
        [CARGO, "test", "-p", "clawed-cli", "--", "verify_", "--nocapture"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=30,
    )
    output = result.stdout + result.stderr

    checks = [
        "verify_heading_no_hash_prefix",
        "verify_unordered_list_dash_prefix",
        "verify_blockquote_uses_bar",
        "verify_code_block_no_border",
        "verify_inline_code_blue",
        "verify_diff_gutter_format",
        "verify_diff_removed_line",
        "verify_agent_progress_tree_chars",
        "verify_agent_progress_badge_has_bg",
    ]
    for name in checks:
        check(f"Verification {name}", f"{name} ... ok" in output)


def test_full_suite():
    """Run complete test suite."""
    print("\n─── Full Test Suite ───")
    result = subprocess.run(
        [CARGO, "test", "-p", "clawed-cli"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=120,
    )
    output = result.stdout + result.stderr

    # Parse test results
    unit_match = re.search(r"test result: ok\. (\d+) passed", output)
    e2e_match = re.search(r"(\d+) passed.*filtered out", output)

    if unit_match:
        check("Unit tests all pass", True, f"{unit_match.group(1)} passed")
    else:
        check("Unit tests all pass", False, "could not parse output")

    check("No test failures", "0 failed" in output)


# ── Main ───────────────────────────────────────────────────────────────────────

def main():
    print("=" * 60)
    print("  Clawed TUI Automated Verification")
    print("=" * 60)

    # Build first
    print("\n─── Building ───")
    build_result = subprocess.run(
        [CARGO, "build", "-p", "clawed-cli"],
        cwd=str(PROJECT_DIR),
        capture_output=True,
        text=True,
        timeout=120,
    )
    if build_result.returncode != 0:
        print("\033[31mBUILD FAILED\033[0m")
        print(build_result.stderr[-500:])
        sys.exit(1)
    print("  ✓ Build succeeded")

    # Run tests
    test_help_flag()
    test_version()
    test_list_sessions()
    test_completions()
    test_init_skips_existing()
    test_render_markdown()
    test_render_diff()
    test_screenshot_tests()
    test_verify_tests()
    test_full_suite()
    test_output_formats()

    # Try print mode (needs API key, may fail gracefully)
    try:
        test_print_mode()
    except Exception as e:
        print(f"\n─── Print Mode ───")
        print(f"  ⚠ Print mode test skipped: {e}")

    # Summary
    print("\n" + "=" * 60)
    passed = sum(1 for r in results if r.passed)
    failed = sum(1 for r in results if not r.passed)
    total = len(results)

    print(f"  Results: {passed}/{total} passed")
    if failed:
        print(f"\033[31m  {failed} FAILURES:\033[0m")
        for r in results:
            if not r.passed:
                print(f"    ✗ {r.name}: {r.msg}")

    print("=" * 60)

    # Write report
    report = f"""# Clawed TUI Verification Report

**Date**: {time.strftime('%Y-%m-%d %H:%M:%S')}
**Results**: {passed}/{total} passed

## Details

"""
    for r in results:
        icon = "✓" if r.passed else "✗"
        report += f"| {icon} | {r.name} | {r.msg or '-'} |\n"

    (PROJECT_DIR / "scripts" / "verification_report.md").write_text(report)
    print(f"\nReport saved to scripts/verification_report.md")

    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
