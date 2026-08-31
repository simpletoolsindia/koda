#!/usr/bin/env python3
# Verify transcript scrolling: Ctrl+Up scrolls the agent response window so the
# user can read back agent responses. (Plain Up/Down recall input history;
# Ctrl+Up/Down and PageUp/PageDown scroll.) Uses a tiny terminal so content
# overflows and scrolling is observable.
import os, pty, subprocess, time, fcntl, termios, struct, tempfile

ROWS, COLS = 14, 80   # small height so the transcript overflows
BIN = "./target/release/koda"
PORT = "8123"

def run():
    master, slave = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    ws = tempfile.mkdtemp()
    open(os.path.join(ws, "demo.txt"), "w").write("hello world\n")
    env = dict(os.environ, TERM="xterm-256color", COLUMNS=str(COLS), LINES=str(ROWS))
    proc = subprocess.Popen([BIN, "-C", ws, "-u", f"http://127.0.0.1:{PORT}/v1",
                             "-m", "mock-coder"], stdin=slave, stdout=slave,
                            stderr=slave, env=env, close_fds=True)
    os.close(slave)
    grid = [[" "]*COLS for _ in range(ROWS)]
    r = c = 0
    def feed(data):
        nonlocal r, c
        i = 0
        while i < len(data):
            ch = data[i]
            if ch == "\x1b" and i+1 < len(data) and data[i+1] == "[":
                k = i+2
                while k < len(data) and not data[k].isalpha(): k += 1
                params, cmd = data[i+2:k], (data[k] if k < len(data) else "")
                if cmd == "H":
                    p = params.split(";")
                    r = max(0, (int(p[0])-1) if p and p[0] else 0)
                    c = max(0, (int(p[1])-1) if len(p)>1 and p[1] else 0)
                elif cmd == "J":
                    for y in range(ROWS): grid[y] = [" "]*COLS
                    r = c = 0
                i = k+1; continue
            if ch == "\n": r = min(ROWS-1, r+1); c = 0
            elif ch == "\r": c = 0
            elif ch >= " ":
                if r < ROWS and c < COLS: grid[r][c] = ch
                c += 1
            i += 1
    def read(sec):
        end = time.time()+sec; os.set_blocking(master, False)
        while time.time() < end:
            try:
                d = os.read(master, 65536)
                if d: feed(d.decode("utf-8","replace"))
            except BlockingIOError: time.sleep(0.03)
    def snap(): return "\n".join("".join(row) for row in grid)

    read(1.5)
    os.write(master, b"replace hello with goodbye in demo.txt\r")
    read(3.0)                      # let the turn complete (multiple tool blocks + reply)
    top_before = grid[0][:]        # first visible row while following the tail
    before = snap()
    # Empty input — press Ctrl+Up several times; the transcript should scroll up.
    # (Plain Up now recalls input history; Ctrl+Up is the scroll key.)
    for _ in range(6):
        os.write(master, b"\x1b[1;5A")  # Ctrl+Up
        read(0.3)
    after = snap()
    top_after = "".join(grid[0])
    changed = before != after
    print("=== BEFORE (tail) top row ===")
    print("|" + "".join(top_before).rstrip())
    print("=== AFTER 6x Ctrl+Up, top row ===")
    print("|" + top_after.rstrip())
    print("VERDICT:", "SCROLLS (ctrl+up moved the transcript)" if changed else "BROKEN (no scroll)")
    os.write(master, b"\x03\x03")  # ctrl+c twice to quit
    read(0.5)
    proc.terminate()
    try: proc.wait(3)
    except Exception: proc.kill()

if __name__ == "__main__":
    run()
