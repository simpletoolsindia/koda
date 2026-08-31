#!/usr/bin/env python3
# Verify the system-prompt editor opens a textarea pre-populated with the
# built-in prompt (so the user can see & edit it, not type blind).
import os, pty, subprocess, time, fcntl, termios, struct, tempfile

ROWS, COLS = 40, 100
BIN = "./target/release/koda"
PORT = "8123"

def run():
    master, slave = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    ws = tempfile.mkdtemp()
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
                    r = max(0,(int(p[0])-1) if p and p[0] else 0)
                    c = max(0,(int(p[1])-1) if len(p)>1 and p[1] else 0)
                elif cmd == "J":
                    for y in range(ROWS): grid[y]=[" "]*COLS
                    r=c=0
                i = k+1; continue
            if ch == "\n": r=min(ROWS-1,r+1); c=0
            elif ch == "\r": c=0
            elif ch >= " ":
                if r<ROWS and c<COLS: grid[r][c]=ch
                c+=1
            i+=1
    def read(sec):
        end=time.time()+sec; os.set_blocking(master,False)
        while time.time()<end:
            try:
                d=os.read(master,65536)
                if d: feed(d.decode("utf-8","replace"))
            except BlockingIOError: time.sleep(0.03)
    def snap(): return "\n".join("".join(row) for row in grid)

    read(1.5)
    os.write(master, b"/settings\r")
    read(1.0)
    # Navigate down to "system prompt" row (near the bottom) — press Down many times.
    for _ in range(19):
        os.write(master, b"\x1b[B"); read(0.05)
    read(0.3)
    os.write(master, b"\r")   # Enter -> open the textarea
    read(0.8)
    s = snap()
    ok_title = "Edit system prompt" in s
    ok_body = "You are koda" in s
    print("VERDICT title:", "OK" if ok_title else "MISSING", "| built-in text:", "OK" if ok_body else "MISSING")
    if not (ok_title and ok_body):
        for y,line in enumerate(s.split("\n")):
            if line.strip(): print(f"{y:2}|{line.rstrip()[:96]}")
    os.write(master, b"\x1b"); read(0.3)  # esc
    os.write(master, b"\x1b"); read(0.3)  # esc settings
    proc.terminate()
    try: proc.wait(3)
    except Exception: proc.kill()

if __name__ == "__main__":
    run()
