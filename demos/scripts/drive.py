#!/usr/bin/env python3
"""Drive koda through a pty and relay its screen, so asciinema can record it.

`asciinema rec --command` gives the command a pty, but koda needs *keystrokes*
— a seeded prompt only fills the composer, it does not send. So this opens a
second, inner pty for koda, types into it on a schedule, and copies everything
koda paints to its own stdout, which is what asciinema captures.

Usage:
    drive.py <spec.json>

The spec is one demo out of `demos/manifest.json`, already resolved by
`record.py`: workspace, url, koda flags, terminal size and the step list.

Each step is one of:
    type:TEXT      type it, a character at a time (no Enter)
    key:NAME       enter | esc | tab | ctrl-c | ctrl-p | up | down | … (see KEYS)
    wait:SECONDS   let the screen settle, and be watched
    sh:COMMAND     run a shell command in the workspace, off-screen; used to
                   poke the world koda is watching (a file a trigger fires on)

With `"driver": "shell"` the pty gets a bare shell instead of the TUI, and the
steps type into that — which is how the headless (`koda -p`) demos are made.
`koda` is on its PATH, and the endpoint comes from the demo's config, so the
command line on screen is the one a reader would actually type.
"""
import fcntl
import json
import os
import pty
import struct
import subprocess
import sys
import termios
import time

KEYS = {
    "enter": "\r",
    "esc": "\x1b",
    "tab": "\t",
    "backspace": "\x7f",
    "space": " ",
    "ctrl-a": "\x01",
    "ctrl-c": "\x03",
    "ctrl-d": "\x04",
    "ctrl-j": "\n",
    "ctrl-k": "\x0b",
    "ctrl-o": "\x0f",
    "ctrl-p": "\x10",
    "ctrl-r": "\x12",
    "ctrl-t": "\x14",
    "ctrl-u": "\x15",
    "ctrl-w": "\x17",
    "up": "\x1b[A",
    "down": "\x1b[B",
    "right": "\x1b[C",
    "left": "\x1b[D",
    "pgup": "\x1b[5~",
    "pgdn": "\x1b[6~",
    "shift-up": "\x1b[1;2A",
    "shift-down": "\x1b[1;2B",
}


def main() -> int:
    spec = json.load(open(sys.argv[1]))
    workspace = spec["workspace"]
    cols, rows = spec["cols"], spec["rows"]
    binary = os.environ.get("KODA_BIN", "./target/release/koda")

    master, slave = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    env = dict(
        os.environ,
        TERM="xterm-256color",
        COLUMNS=str(cols),
        LINES=str(rows),
        # Never record the recorder's own provider, theme or autonomy tier.
        XDG_CONFIG_HOME=os.path.join(workspace, ".config"),
        # Deterministic prompt-side state: no git author noise in the status bar.
        GIT_AUTHOR_NAME="koda demo",
        GIT_AUTHOR_EMAIL="demo@example.com",
        # A sandboxed home, so sessions, the index and the learning store are
        # this demo's own — and so no recording ever publishes the path to
        # whoever's laptop it was made on.
        HOME=os.path.dirname(workspace),
        XDG_DATA_HOME=os.path.join(os.path.dirname(workspace), "data"),
    )
    if spec.get("home") == "real":
        # One exception: the browse demo needs the agent-browser koda installed
        # into the real data directory, so it keeps its own home.
        for key in ("HOME", "XDG_DATA_HOME"):
            env[key] = os.environ.get(key, env[key])
    env.update(spec.get("env", {}))
    if spec.get("driver") == "shell":
        # A bare, quiet shell: no rc files, a fixed prompt, and koda on PATH.
        env["PS1"] = spec.get("ps1", "\\[\\]$ ")
        env["PATH"] = os.path.join(os.path.dirname(workspace), "bin") + os.pathsep + env["PATH"]
        argv = ["bash", "--noprofile", "--norc", "-i"]
    else:
        argv = [binary, "-C", workspace, "-u", spec["url"], "-m", "mock-coder"]
        argv += spec.get("flags", ["-y"])
    proc = subprocess.Popen(
        argv, cwd=workspace,
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

    if spec.get("driver") == "shell":
        # Swallow the shell's own greeting (macOS bash announces itself, and
        # "no job control in this shell" is an artefact of the pty, not of
        # koda) before anything is recorded.
        os.write(master, b"clear\r")
        deadline = time.time() + 0.8
        while time.time() < deadline:
            try:
                os.read(master, 65536)
            except (BlockingIOError, OSError):
                time.sleep(0.01)
        # An empty line, so the recording opens on a prompt rather than on a
        # command with nothing in front of it.
        os.write(master, b"\r")

    # Let the intro animation play; it is part of what the demo shows.
    pump(spec.get("intro", 2.5))

    for step in spec["steps"]:
        kind, _, arg = step.partition(":")
        if kind == "type":
            # A character at a time, so the viewer sees it typed rather than
            # pasted. 28ms is close to a fast human and reads as deliberate.
            for ch in arg:
                os.write(master, ch.encode())
                pump(0.028)
        elif kind == "key":
            os.write(master, KEYS[arg].encode())
            pump(0.25)
        elif kind == "wait":
            pump(float(arg))
        elif kind == "sh":
            # Off-screen: koda is meant to notice the effect, not the command.
            subprocess.run(arg, shell=True, cwd=workspace, env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            pump(0.3)
        else:
            raise SystemExit(f"unknown step: {step}")

    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
