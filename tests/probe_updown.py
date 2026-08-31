#!/usr/bin/env python3
# Verify the up/down remap:
#   * plain Up on empty input recalls the PREVIOUS message the user typed
#     (input history) — it does NOT scroll the transcript.
#   * Ctrl+Up scrolls the agent response window (transcript).
# Small terminal so the transcript overflows and scrolling is observable.
import os, pty, subprocess, time, fcntl, termios, struct, tempfile, sys

ROWS, COLS = 16, 90
BIN = os.environ.get("BIN", "./target/release/koda")
PORT = os.environ.get("PORT", "8123")
URL = f"http://127.0.0.1:{PORT}/v1"

passed = failed = 0
def check(name, cond, extra=""):
    global passed, failed
    if cond: print(f"  ok   {name}"); passed += 1
    else: print(f"  FAIL {name}  {extra}"); failed += 1

def run():
    master, slave = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    ws = tempfile.mkdtemp()
    open(os.path.join(ws, "demo.txt"), "w").write("hello world\nsecond line\n")
    env = dict(os.environ, TERM="xterm-256color", COLUMNS=str(COLS), LINES=str(ROWS))
    proc = subprocess.Popen([BIN, "-C", ws, "-u", URL, "-m", "mock-coder", "-y"],
        stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True)
    os.close(slave)
    grid = [[" "]*COLS for _ in range(ROWS)]; r = c = 0
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
                    r = max(0,(int(p[0])-1) if p and p[0] else 0)
                    c = max(0,(int(p[1])-1) if len(p)>1 and p[1] else 0)
                elif cmd == "J":
                    for y in range(ROWS): grid[y] = [" "]*COLS
                    r = c = 0
                elif cmd == "K":
                    for x in range(c, COLS): grid[r][x] = " "
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
    # Type and submit a distinctive message so it enters input history.
    os.write(master, b"remember-this-line replace hello with goodbye in demo.txt\r")
    read(3.0)  # let the turn run and complete

    # --- plain Up on empty input recalls the typed message (history) ---
    os.write(master, b"\x1b[A")   # Up
    read(0.6)
    after_up = snap()
    rows = after_up.split("\n")
    # The recalled text lands on the composer prompt line (marked with ❯),
    # wherever that sits above the status rows.
    recalled = any("remember-this-line" in row and "\u276f" in row for row in rows) \
        or any("remember-this-line" in row for row in rows[-6:])
    check("plain Up recalls the previous typed message into the input", recalled,
          repr([r for r in rows if r.strip()][-4:]))

    # Clear the input line so it doesn't interfere with the scroll test.
    os.write(master, b"\x15")  # ctrl+u
    read(0.3)
    before_scroll = snap()

    # --- Ctrl+Up scrolls the transcript ---
    for _ in range(6):
        os.write(master, b"\x1b[1;5A")   # Ctrl+Up
        read(0.25)
    after_scroll = snap()
    check("Ctrl+Up scrolls the agent response window", before_scroll != after_scroll)

    os.write(master, b"\x03\x03")
    read(0.5)
    proc.terminate()
    try: proc.wait(3)
    except Exception: proc.kill()
    print(f"== summary ==\n  {passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)

if __name__ == "__main__":
    run()
