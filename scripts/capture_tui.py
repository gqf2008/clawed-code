#!/usr/bin/env python3
"""Capture clawed TUI screenshots via PTY using pexpect."""

import os, sys, time, re
from pathlib import Path
from datetime import datetime

PROJECT_DIR = Path(__file__).resolve().parent.parent
SCREENSHOT_DIR = PROJECT_DIR / "scripts" / "tui_screenshots"
SCREENSHOT_DIR.mkdir(exist_ok=True)

def capture():
    import pexpect

    # Spawn clawed TUI with proper terminal size
    child = pexpect.spawn(
        "cargo run --",
        cwd=str(PROJECT_DIR),
        env={**os.environ, "TERM": "xterm-256color"},
        dimensions=(40, 120),
        timeout=10,
        encoding="utf-8",
        codec_errors="replace",
    )

    screens = {}

    try:
        # Wait for initial TUI render (build + startup)
        idx = child.expect([r"> ", r"clawed", pexpect.TIMEOUT], timeout=60)
        time.sleep(0.5)
        screens["01_initial"] = child.before + child.after

        # The TUI should now be showing the input area with "> " prompt
        # Type a slash command
        child.send("/help")
        time.sleep(0.5)
        screens["02_slash_help"] = child.before + child.after

        # Clear with Esc
        child.sendcontrol("c")  # Escape/cancel
        time.sleep(0.3)
        # Start fresh
        child.send("test message")
        time.sleep(0.3)
        screens["03_input"] = child.before + child.after

        # Clear
        for _ in range(20):
            child.send("\b")
        time.sleep(0.2)

    except Exception as e:
        print(f"  Error: {e}")
    finally:
        try:
            child.sendcontrol("c")
            child.sendcontrol("c")
        except:
            pass
        child.terminate(force=True)
        time.sleep(0.5)

    # Save screenshots
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    for name, data in screens.items():
        # Raw ANSI
        raw = data.encode("utf-8", errors="replace") if isinstance(data, str) else data
        (SCREENSHOT_DIR / f"{name}_{ts}.ansi").write_bytes(raw)

        # Stripped text
        ansi_re = re.compile(r'\x1b\[[0-9;]*[a-zA-Z]|\x1b\].*?\x07')
        clean = ansi_re.sub('', str(data))
        (SCREENSHOT_DIR / f"{name}_{ts}.txt").write_text(clean, errors="replace")

    print(f"Saved {len(screens)} screenshots to {SCREENSHOT_DIR}/")
    for name in screens:
        f = SCREENSHOT_DIR / f"{name}_{ts}.txt"
        content = f.read_text()[:200] if f.exists() else "(empty)"
        print(f"  {name}: {len(content)} chars")


def capture_simple():
    """Simpler: just capture the initial TUI state."""
    import pexpect

    child = pexpect.spawn(
        "cargo run --",
        cwd=str(PROJECT_DIR),
        env={**os.environ, "TERM": "xterm-256color"},
        dimensions=(40, 120),
        timeout=120,
    )

    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    all_output = ""

    try:
        # Wait for build to finish and TUI to start
        idx = child.expect([r"> ", "clawed", pexpect.TIMEOUT], timeout=90)
        all_output = child.before + child.after
        time.sleep(1)

        # Read whatever is on screen
        try:
            more = child.read_nonblocking(size=5000, timeout=0.5)
            all_output += more
        except:
            pass

        print(f"Captured {len(all_output)} chars of TUI output")

    except pexpect.TIMEOUT:
        print("Timeout - TUI may not have started. Partial output:")
        all_output = child.before or ""
        print(f"  Got {len(all_output)} chars")
    except Exception as e:
        print(f"Error: {e}")
        all_output = str(child.before or "")
    finally:
        child.terminate(force=True)

    # Save
    raw = all_output.encode("utf-8", errors="replace") if isinstance(all_output, str) else all_output
    ansi_re = re.compile(r'\x1b\[[0-9;]*[a-zA-Z]|\x1b\].*?\x07')

    (SCREENSHOT_DIR / f"tui_raw_{ts}.ansi").write_bytes(raw)
    clean = ansi_re.sub('', str(all_output))
    (SCREENSHOT_DIR / f"tui_clean_{ts}.txt").write_text(clean, errors="replace")

    # Show the clean output
    lines = [l for l in clean.split('\n') if l.strip()]
    print(f"\nTUI output ({len(lines)} non-empty lines):")
    for l in lines[:30]:
        print(f"  {l}")

    return child


if __name__ == "__main__":
    print("Capturing clawed TUI via PTY...")
    capture_simple()
