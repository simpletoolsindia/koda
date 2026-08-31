#!/usr/bin/env python3
# Focused runtime check for the approval-popup vertical position (bug 2).
# Drives the release binary against the mock (native mode = read then edit,
# which triggers a write approval), captures the screen, and reports the row
# range the modal occupies so we can see it is centered, not bottom-docked.
import os, pty, subprocess, time, fcntl, termios, struct, sys, tempfile

ROWS, COLS = 30, 100
BIN = "./target/release/koda"
PORT = "8123"

def run():
    master, slave = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    ws = tempfile.mkdtemp()
    open(os.path.join(ws, "demo.txt"), "w").write("hello world\n")
    env = dict(os.environ, TERM="xterm-256color", COLUMNS=str(COLS), LINES=str(ROWS))
    proc = subprocess.Popen(
        [BIN, "-C", ws, "-u", f"http://127.0.0.1:{PORT}/v1", "-m", "mock-coder"],
        stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True,
    )
    os.close(slave)
    grid = [[" "] * COLS for _ in range(ROWS)]
    r = c = 0
    # crude VT: track cursor for CUP, else write chars; enough to locate the modal
    def feed(data):
        nonlocal r, c
        i = 0
        while i < len(data):
            ch = data[i]
            if ch == "\x1b":
                # parse CSI
                j = i + 1
                if j < len(data) and data[j] == "[":
                    k = j + 1
                    while k < len(data) and not data[k].isalpha():
                        k += 1
                    params = data[j+1:k]
                    cmd = data[k] if k < len(data) else ""
                    if cmd == "H":
                        parts = params.split(";")
                        r = max(0, (int(parts[0]) - 1) if parts and parts[0] else 0)
                        c = max(0, (int(parts[1]) - 1) if len(parts) > 1 and parts[1] else 0)
                    elif cmd == "J":
                        for y in range(ROWS): grid[y] = [" "]*COLS
                        r = c = 0
                    i = k + 1
                    continue
                i += 1
                continue
            if ch == "\n":
                r = min(ROWS-1, r+1); c = 0
            elif ch == "\r":
                c = 0
            elif ch >= " ":
                if r < ROWS and c < COLS:
                    grid[r][c] = ch
                c += 1
            i += 1
    def read(sec):
        end = time.time() + sec
        os.set_blocking(master, False)
        while time.time() < end:
            try:
                d = os.read(master, 65536)
                if d: feed(d.decode("utf-8", "replace"))
            except BlockingIOError:
                time.sleep(0.05)
    read(1.5)
    os.write(master, b"replace hello with goodbye in demo.txt\r")
    read(3.0)  # let it read + reach the edit approval
    # Find rows containing the modal border/title.
    text_rows = ["".join(row) for row in grid]
    modal_rows = [y for y,line in enumerate(text_rows) if ("APPROVE" in line or "EDIT FILE" in line or "allow once" in line)]
    print("screen rows with modal markers:", modal_rows)
    mid = ROWS // 2
    if modal_rows:
        top = min(modal_rows)
        print(f"modal top row = {top}, screen mid = {mid}, bottom = {ROWS}")
        print("VERDICT:", "CENTERED" if top < mid else "BOTTOM-DOCKED")
    else:
        print("no modal detected — dumping screen:")
        for y,line in enumerate(text_rows):
            if line.strip(): print(f"{y:2} |{line.rstrip()}")
    os.write(master, b"n")  # decline
    read(0.5)
    proc.terminate()
    try: proc.wait(3)
    except Exception: proc.kill()

if __name__ == "__main__":
    run()
