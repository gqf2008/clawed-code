#!/usr/bin/env python3
"""PTY-based TUI screenshot capture for clawed.

Spawns clawed in a pseudo-terminal (without --print mode),
captures actual terminal output, and saves screenshots.
"""

import os
import sys
import time
import pty
import tty
import struct
import fcntl
import termios
import subprocess
import select
import json
from pathlib import Path
from datetime import datetime

PROJECT_DIR = Path(__file__).resolve().parent.parent
SCREENSHOT_DIR = PROJECT_DIR / "scripts" / "screenshots"
SCREENSHOT_DIR.mkdir(exist_ok=True)


def spawn_tui(width=120, height=40):
    """Spawn clawed TUI in a PTY with given terminal size.
    Returns (master_fd, slave_fd, process).
    """
    master_fd, slave_fd = pty.openpty()

    # Set window size via TIOCSWINSZ
    winsize = struct.pack("HHHH", height, width, 0, 0)
    fcntl.ioctl(slave_fd, termios.TIOCSWINSZ, winsize)

    proc = subprocess.Popen(
        ["cargo", "run", "--"],
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        cwd=str(PROJECT_DIR),
        env={
            **os.environ,
            "TERM": "xterm-256color",
            "COLUMNS": str(width),
            "LINES": str(height),
        },
        close_fds=True,
    )
    os.close(slave_fd)
    return master_fd, proc


def capture_screen(fd, timeout=0.5) -> bytes:
    """Read all available terminal output."""
    data = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.05)
        if ready:
            try:
                chunk = os.read(fd, 4096)
                if not chunk:
                    break
                data += chunk
            except (BlockingIOError, OSError):
                if data:
                    break
        else:
            if data:
                break
    return data


def send_and_capture(fd, text: str, wait=0.5) -> bytes:
    """Send text to PTY and capture response."""
    for ch in text:
        os.write(fd, ch.encode())
        time.sleep(0.01)
    time.sleep(wait)
    return capture_screen(fd)


def main():
    print("Starting clawed TUI in PTY...")
    fd, proc = spawn_tui(120, 40)

    try:
        # Wait for initial render
        time.sleep(2.0)
        initial = capture_screen(fd, timeout=1.0)
        screenshot("01_initial", initial)

        # Type a simple prompt
        send_and_capture(fd, "hello", wait=0.5)
        pre_submit = capture_screen(fd, timeout=0.5)
        screenshot("02_input_hello", pre_submit)

        # Type / key to trigger completion
        send_and_capture(fd, "\b\b\b\b\b/", wait=0.5)
        after_slash = capture_screen(fd, timeout=0.5)
        screenshot("03_slash_completion", after_slash)

        # Clear with Esc
        send_and_capture(fd, "\x1b", wait=0.3)
        cleared = capture_screen(fd, timeout=0.3)
        screenshot("04_esc_cleared", cleared)

        # Type /model to test overlay
        send_and_capture(fd, "/model", wait=0.5)
        model_shown = capture_screen(fd, timeout=0.5)
        screenshot("05_model_overlay", model_shown)

        print(f"\nScreenshots saved to {SCREENSHOT_DIR}/")
        print("Files:", sorted(os.listdir(str(SCREENSHOT_DIR))))

    finally:
        proc.terminate()
        os.close(fd)


def screenshot(name: str, data: bytes):
    """Save raw terminal data with timestamp."""
    ts = datetime.now().strftime("%Y%m%d_%H%M%S")

    import re
    text = data.decode("utf-8", errors="replace")

    # Raw ANSI
    (SCREENSHOT_DIR / f"{name}_{ts}.ansi").write_bytes(data)

    # Stripped text
    ansi = re.compile(r'\x1b\[[0-9;]*[a-zA-Z]|\x1b\].*?\x07')
    clean = ansi.sub('', text)

    # Dump visible characters only
    lines = []
    for line in clean.split('\n'):
        stripped = ''.join(c if c.isprintable() or c in '\t ' else ' ' for c in line)
        if stripped.strip():
            lines.append(stripped)
    (SCREENSHOT_DIR / f"{name}_{ts}.txt").write_text('\n'.join(lines), errors="replace")


if __name__ == "__main__":
    main()
