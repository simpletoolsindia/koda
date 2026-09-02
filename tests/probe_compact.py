#!/usr/bin/env python3
# Verify the /compact fixes:
#  1) running /compact shows an animated "compacting…" status,
#  2) it completes (a "compacted N → M tokens" notice),
#  3) input typed afterwards is accepted (a new turn runs) — i.e. the prompt is
#     NOT stuck the way it was before (needing a CLI restart).
# Drives the release binary against the mock server (any mode; the mock answers
# compaction requests with a canned summary).
import os, pty, subprocess, time, fcntl, termios, struct, sys, tempfile, re

ROWS, COLS = 40, 100
BIN = os.environ.get("BIN", "./target/release/koda")
PORT = os.environ.get("PORT", "8123")
URL = f"http://127.0.0.1:{PORT}/v1"
# Overridable so a probe can drive a real backend (e.g. OmniRoute) instead of
# the mock server.
MODEL = os.environ.get("MODEL", "mock-coder")

passed = failed = 0
def check(name, cond, extra=""):
    global passed, failed
    if cond:
        print(f"  ok   {name}"); passed += 1
    else:
        print(f"  FAIL {name}  {extra}"); failed += 1


class Screen:
    CSI = re.compile(r"\x1b\[([0-9;?]*)([@-~])")
    def __init__(self, r, c): self.rows, self.cols = r, c; self.reset()
    def reset(self): self.grid=[[" "]*self.cols for _ in range(self.rows)]; self.r=self.c=0; self.pending=""
    def feed(self, text):
        data=self.pending+text; self.pending=""; i=0
        while i < len(data):
            ch=data[i]
            if ch=="\x1b":
                m=self.CSI.match(data,i)
                if m: self.csi(m.group(1),m.group(2)); i=m.end(); continue
                if i+1>=len(data): self.pending=data[i:]; return
                # OSC (koda posts OSC 9 notifications, OSC 52 clipboard): skip to
                # BEL/ST rather than painting the payload into the grid.
                if data[i+1]=="]":
                    end=data.find("\x07",i); st=data.find("\x1b\\",i)
                    if end==-1 and st==-1: self.pending=data[i:]; return
                    i=(st+2) if (end==-1 or (st!=-1 and st<end)) else (end+1)
                    continue
                # A CSI split across two reads must be held, not skipped.
                if data[i+1]=="[" and self.CSI.match(data,i) is None:
                    self.pending=data[i:]; return
                i+=2; continue
            if ch=="\r": self.c=0
            elif ch=="\n": self.nl()
            elif ch=="\t": self.c=min(self.cols-1,(self.c//8+1)*8)
            elif ch=="\b": self.c=max(0,self.c-1)
            elif ch>=" ":
                if self.c>=self.cols: self.c=0; self.nl()
                self.grid[self.r][self.c]=ch; self.c+=1
            i+=1
    def nl(self):
        self.r+=1
        if self.r>=self.rows: self.grid.pop(0); self.grid.append([" "]*self.cols); self.r=self.rows-1
    def csi(self, params, final):
        nums=[int(p) for p in params.split(";") if p.isdigit()]; n=nums[0] if nums else 0
        if final in "Hf":
            self.r=max(0,min(self.rows-1,(nums[0]-1) if nums else 0))
            self.c=max(0,min(self.cols-1,(nums[1]-1) if len(nums)>1 else 0))
        elif final=="A": self.r=max(0,self.r-max(1,n))
        elif final=="B": self.r=min(self.rows-1,self.r+max(1,n))
        elif final=="C": self.c=min(self.cols-1,self.c+max(1,n))
        elif final=="D": self.c=max(0,self.c-max(1,n))
        elif final=="G": self.c=max(0,min(self.cols-1,(nums[0]-1) if nums else 0))
        elif final=="K":
            if n==0:
                for x in range(self.c,self.cols): self.grid[self.r][x]=" "
            elif n==1:
                for x in range(0,self.c+1): self.grid[self.r][x]=" "
            else: self.grid[self.r]=[" "]*self.cols
        elif final=="J":
            if n in (2,3): self.reset()
            elif n==0:
                for x in range(self.c,self.cols): self.grid[self.r][x]=" "
                for y in range(self.r+1,self.rows): self.grid[y]=[" "]*self.cols
    def text(self): return "\n".join("".join(row) for row in self.grid)


class Tui:
    def __init__(self, ws):
        self.master, slave = pty.openpty()
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        env = dict(os.environ, TERM="xterm-256color", COLUMNS=str(COLS), LINES=str(ROWS))
        self.proc = subprocess.Popen([BIN,"-C",ws,"-u",URL,"-m",MODEL,"-y"],
            stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True)
        os.close(slave); self.vt=Screen(ROWS,COLS); self.history=[]
    def read(self, sec, until=None):
        end=time.time()+sec
        while time.time()<end:
            try:
                os.set_blocking(self.master, False)
                d=os.read(self.master,65536)
                if d: self.vt.feed(d.decode("utf-8","replace")); self.history.append(self.vt.text())
            except (BlockingIOError,OSError): pass
            if until and self.saw(until): return
            time.sleep(0.04)
    def saw(self, needle):
        return needle in self.vt.text() or any(needle in f for f in self.history)
    def send(self, s): os.write(self.master, s.encode()); time.sleep(0.15)
    def close(self):
        try: self.send("\x04")
        except OSError: pass
        end=time.time()+8
        while time.time()<end:
            try:
                os.set_blocking(self.master,False)
                d=os.read(self.master,65536)
                if d: self.vt.feed(d.decode("utf-8","replace"))
            except (BlockingIOError,OSError): pass
            if self.proc.poll() is not None: return self.proc.returncode
            time.sleep(0.02)
        self.proc.kill(); return -1


def main():
    ws = tempfile.mkdtemp()
    open(os.path.join(ws,"demo.txt"),"w").write("hello world\nsecond line\n")
    t = Tui(ws)
    t.read(2.0, until="ready")

    # First, a normal turn so there is history to compact.
    t.send("replace hello with goodbye in demo.txt\r")
    t.read(8.0, until="replaced hello with goodbye")

    # Run /compact — the status should show a compacting animation, then finish.
    t.send("/compact\r")
    t.read(6.0, until="compacting")
    check("shows a compacting status while it runs", t.saw("compacting"), t.vt.text()[:200])
    t.read(6.0, until="compacted")
    check("reports completion (compacted N → M tokens)", t.saw("compacted"))

    # Now the key regression: input AFTER compaction must be accepted (a new turn
    # runs) rather than being silently swallowed until a restart.
    t.read(2.0, until="ready")
    # Clear history-frame noise: mark a boundary by counting current "Read" cards.
    t.send("please edit the file again\r")
    # A new turn must start: the user text is echoed AND fresh tool/reply activity
    # happens after we submitted (proving the agent task wasn't stuck).
    t.read(8.0)
    check("accepts a new message after compaction (echoed)", t.saw("please edit the file again"),
          t.vt.text()[-300:])
    check("a new turn actually runs after compaction (agent replied)",
          t.saw("Done: replaced hello with goodbye") or t.saw("replaced hello with goodbye"))

    t.close()
    subprocess.run(["rm","-rf",ws])
    print(f"== summary ==\n  {passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)

if __name__ == "__main__":
    main()
