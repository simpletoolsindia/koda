#!/usr/bin/env python3
"""Drive koda through a pty and relay its screen, so asciinema can record it.

`asciinema rec --command` gives the command a pty, but koda needs *keystrokes*
— a seeded prompt only fills the composer, it does not send. So this opens a
second, inner pty for koda, types into it on a schedule, and copies everything
koda paints to its own stdout, which is what asciinema captures.

Usage:
    drive.py <workspace> <url> <step>...

Each step is one of:
    type:TEXT     type it (no Enter)
    key:NAME      enter | esc | ctrl-c | ctrl-p | tab | up | down
    wait:SECONDS  let the screen settle, and be watched
"""
import fcntl
import os
import pty
import struct
import subprocess
import sys
import termios
import time

ROWS, COLS = 30, 100

KEYS = {
    "enter": "\r",
    "esc": "\x1b",
    "ctrl-c": "\x03",
    "ctrl-p": "\x10",
    "ctrl-t": "\x14",
    "tab": "\t",
    "up": "\x1b[A",
    "down": "\x1b[B",
}


def main() -> int:
    workspace, url, *steps = sys.argv[1:]
    binary = os.environ.get("KODA_BIN", "./target/release/koda")

    master, slave = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    env = dict(
        os.environ,
        TERM="xterm-256color",
        COLUMNS=str(COLS),
        LINES=str(ROWS),
        # Never record the recorder's own provider, theme or autonomy tier.
        XDG_CONFIG_HOME=os.path.join(workspace, ".config"),
    )
    proc = subprocess.Popen(
        [binary, "-C", workspace, "-u", url, "-m", "mock-coder", "-y"],
        stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True,
    )
    os.close(slave)
    os.set_blocking(master, False)

    def pump(seconds: float) -> None:
        """Copy koda's output through for `seconds`, so the frame is recorded."""
        deadline = time.time() + seconds
        while time.time() < deadline:
            try:
                chunk = os.read(master, 65536)
            except (BlockingIOError, OSError):
                time.sleep(0.01)
                continue
            if not chunk:
                break
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()

    # Let the intro animation play; it is part of what the demo shows.
    pump(2.5)

    for step in steps:
        kind, _, arg = step.partition(":")
        if kind == "type":
            # A character at a time, so the viewer sees it typed rather than
            # pasted. 28ms is close to a fast human and reads as deliberate.
            for ch in arg:
                os.write(master, ch.encode())
                pump(0.028)
        elif kind == "key":
            os.write(master, KEYS[arg].encode())
            pump(0.2)
        elif kind == "wait":
            pump(float(arg))

    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
