#!/usr/bin/env python3
"""Capture clawed TUI screenshots using pyte terminal emulator + pexpect PTY.

pyte interprets ANSI escape sequences and builds an actual screen buffer.
This gives us proper TUI screenshots, character by character.
"""

import os, sys, time, re, io
from pathlib import Path
from datetime import datetime
import pyte
import pexpect

PROJECT_DIR = Path(__file__).resolve().parent.parent
SCREENSHOT_DIR = PROJECT_DIR / "scripts" / "tui_screenshots"
SCREENSHOT_DIR.mkdir(exist_ok=True)

W, H = 120, 40


def capture_screenshot(screen, name: str):
    """Dump current pyte screen buffer to text file."""
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    lines = []
    for row in range(H):
        line = screen.buffer[row]
        text = ""
        for col in range(W):
            char = line[col]
            text += char.data if char.data != " " else char.data
        stripped = text.rstrip()
        if stripped or any(line[c].reverse or line[c].bold or line[c].fg != "default"
                          for c in range(W) if hasattr(line, '__getitem__')):
            lines.append(stripped)
    content = "\n".join(lines)
    f = SCREENSHOT_DIR / f"{name}_{ts}.txt"
    f.write_text(content)
    return content


def main():
    # Build first
    print("Building clawed...")
    result = os.system(f"cd {PROJECT_DIR} && cargo build 2>/dev/null")
    if result != 0:
        print("Build failed"); return

    # Create screen
    screen = pyte.Screen(W, H)
    stream = pyte.Stream(screen)

    # Spawn TUI in PTY
    print("Spawning clawed TUI...")
    child = pexpect.spawn(
        str(PROJECT_DIR / "target" / "debug" / "clawed"),
        dimensions=(H, W),
        env={**os.environ, "TERM": "xterm-256color"},
        timeout=30,
        maxread=65536,
    )

    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    screenshots = {}
    output_log = []

    def feed_and_capture(name, wait=1.0):
        time.sleep(wait)
        try:
            data = child.read_nonblocking(size=65536, timeout=0.5)
            if data:
                output_log.append(data)
                stream.feed(data)
        except pexpect.TIMEOUT:
            pass
        except Exception:
            pass
        text = capture_screenshot(screen, name)
        screenshots[name] = text
        return text

    try:
        # Wait for TUI startup
        time.sleep(3.0)
        try:
            data = child.read_nonblocking(size=65536, timeout=1.0)
            if data:
                output_log.append(data)
                stream.feed(data)
        except:
            pass

        text = capture_screenshot(screen, "01_startup")
        print(f"Startup: {len(text)} chars")
        if text:
            for l in text.split("\n")[:10]:
                print(f"  {l[:100]}")

        # Type some text into the TUI
        child.send("hello")
        time.sleep(0.5)
        try:
            data = child.read_nonblocking(size=65536, timeout=0.5)
            if data:
                output_log.append(data)
                stream.feed(data)
        except:
            pass
        text = capture_screenshot(screen, "02_typed_hello")
        print(f"\nTyped 'hello': {len(text)} chars")
        if text:
            for l in text.split("\n")[:10]:
                print(f"  {l[:100]}")

        # Press / for slash commands
        child.send("\b\b\b\b\b/")
        time.sleep(0.5)
        try:
            data = child.read_nonblocking(size=65536, timeout=0.5)
            if data:
                output_log.append(data)
                stream.feed(data)
        except:
            pass
        text = capture_screenshot(screen, "03_slash")
        print(f"\nSlash: {len(text)} chars")
        if text:
            for l in text.split("\n")[:10]:
                print(f"  {l[:100]}")

    except Exception as e:
        print(f"Error: {e}")
    finally:
        child.terminate(force=True)

    # Save raw output log
    raw_path = SCREENSHOT_DIR / f"raw_output_{ts}.bin"
    raw_path.write_bytes(b"".join(output_log))

    print(f"\nScreenshots saved to {SCREENSHOT_DIR}/")
    for name, text in screenshots.items():
        print(f"  {name}: {len(text)} chars, {len(text.splitlines())} lines")

    # Show final state
    if screenshots:
        last_name = list(screenshots.keys())[-1]
        print(f"\n=== {last_name} ===")
        print(screenshots[last_name][:500] or "(empty)")


if __name__ == "__main__":
    main()
