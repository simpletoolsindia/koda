#!/usr/bin/env python3
"""Drive the interactive TUI through a pseudo-terminal.

A TUI only writes the cells that changed, so the raw pty byte stream is not
readable text. This harness reconstructs the screen with a small VT100
interpreter and asserts against the rendered grid, which is what a user sees.

Covers: startup, streaming, tool cards, the approval modal, interrupting a
running turn, and slash commands.
"""

import fcntl
import os
import pty
import re
import struct
import subprocess
import sys
import tempfile
import termios
import time

BIN = os.environ.get("BIN", "./target/release/koda")
URL = os.environ.get("URL", "http://127.0.0.1:8123/v1")
SLOW_URL = os.environ.get("SLOW_URL", "http://127.0.0.1:8124/v1")
SHOW_URL = os.environ.get("SHOW_URL", "http://127.0.0.1:8130/v1")
ROWS, COLS = 40, 100

passed, failed = 0, 0


def check(name, cond, tui=None):
    global passed, failed
    if cond:
        print(f"  ok   {name}")
        passed += 1
    else:
        print(f"  FAIL {name}")
        failed += 1
        if tui is not None:
            print("       ---- last screen ----")
            for line in tui.screen().splitlines():
                if line.strip():
                    print("       |" + line.rstrip())


class Screen:
    """Just enough VT100 to reconstruct what ratatui painted."""

    CSI = re.compile(r"\x1b\[([0-9;?]*)([@-~])")

    def __init__(self, rows, cols):
        self.rows, self.cols = rows, cols
        self.grid = [[" "] * cols for _ in range(rows)]
        self.r = self.c = 0
        self.pending = ""

    def feed(self, text):
        data = self.pending + text
        self.pending = ""
        i = 0
        while i < len(data):
            ch = data[i]
            if ch == "\x1b":
                m = self.CSI.match(data, i)
                if m:
                    self.csi(m.group(1), m.group(2))
                    i = m.end()
                    continue
                # Either a non-CSI escape or a sequence split across reads.
                if i + 1 >= len(data):
                    self.pending = data[i:]
                    return
                if data[i + 1] == "[":
                    if self.CSI.match(data, i) is None:
                        self.pending = data[i:]
                        return
                i += 2
                continue
            if ch == "\r":
                self.c = 0
            elif ch == "\n":
                self.newline()
            elif ch == "\t":
                self.c = min(self.cols - 1, (self.c // 8 + 1) * 8)
            elif ch == "\b":
                self.c = max(0, self.c - 1)
            elif ch >= " ":
                if self.c >= self.cols:
                    self.c = 0
                    self.newline()
                self.grid[self.r][self.c] = ch
                self.c += 1
            i += 1

    def newline(self):
        self.r += 1
        if self.r >= self.rows:
            self.grid.pop(0)
            self.grid.append([" "] * self.cols)
            self.r = self.rows - 1

    def csi(self, params, final):
        nums = [int(p) for p in params.split(";") if p.isdigit()]
        n = nums[0] if nums else 0
        if final == "H" or final == "f":
            self.r = (nums[0] - 1) if len(nums) > 0 else 0
            self.c = (nums[1] - 1) if len(nums) > 1 else 0
            self.r = max(0, min(self.rows - 1, self.r))
            self.c = max(0, min(self.cols - 1, self.c))
        elif final == "A":
            self.r = max(0, self.r - max(1, n))
        elif final == "B":
            self.r = min(self.rows - 1, self.r + max(1, n))
        elif final == "C":
            self.c = min(self.cols - 1, self.c + max(1, n))
        elif final == "D":
            self.c = max(0, self.c - max(1, n))
        elif final == "G":
            self.c = max(0, min(self.cols - 1, (nums[0] - 1) if nums else 0))
        elif final == "K":
            if n == 0:
                for x in range(self.c, self.cols):
                    self.grid[self.r][x] = " "
            elif n == 1:
                for x in range(0, self.c + 1):
                    self.grid[self.r][x] = " "
            else:
                self.grid[self.r] = [" "] * self.cols
        elif final == "J":
            if n == 2 or n == 3:
                self.grid = [[" "] * self.cols for _ in range(self.rows)]
                self.r = self.c = 0
            elif n == 0:
                for x in range(self.c, self.cols):
                    self.grid[self.r][x] = " "
                for y in range(self.r + 1, self.rows):
                    self.grid[y] = [" "] * self.cols
        # SGR, cursor visibility, alt-screen, etc. need no state here.

    def text(self):
        return "\n".join("".join(row) for row in self.grid)


class Tui:
    def __init__(self, workspace, extra=(), url=URL):
        self.master, slave = pty.openpty()
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        env = dict(os.environ, TERM="xterm-256color", COLUMNS=str(COLS), LINES=str(ROWS))
        self.proc = subprocess.Popen(
            [BIN, "-C", workspace, "-u", url, "-m", "mock-coder", *extra],
            stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True,
        )
        os.close(slave)
        self.vt = Screen(ROWS, COLS)
        # Everything the screen has ever shown, so transient states still count.
        self.history = []

    def screen(self):
        return self.vt.text()

    def read(self, seconds=1.0, until=None):
        deadline = time.time() + seconds
        while time.time() < deadline:
            try:
                os.set_blocking(self.master, False)
                data = os.read(self.master, 65536)
                if data:
                    self.vt.feed(data.decode("utf-8", "replace"))
                    self.history.append(self.vt.text())
            except (BlockingIOError, OSError):
                pass
            if until and self.saw(until):
                return
            time.sleep(0.04)

    def wait_for(self, predicate, seconds=4.0):
        """Poll until `predicate(screen)` holds. Needed for assertions about the
        current screen: a frame can be captured mid-write, so a single sample
        may catch a partially applied redraw."""
        deadline = time.time() + seconds
        while time.time() < deadline:
            self.read(0.15)
            if predicate(self.vt.text()):
                return True
        return predicate(self.vt.text())

    def saw(self, needle):
        """True if the text is on screen now or was at any earlier point."""
        if needle in self.vt.text():
            return True
        return any(needle in frame for frame in self.history)

    def send(self, text):
        os.write(self.master, text.encode())
        time.sleep(0.15)

    def close(self):
        try:
            self.send("\x04")  # ctrl+d
        except OSError:
            pass
        # Keep draining stdout while we wait. koda paints one last frame as it
        # quits; if we stop reading, the pty buffer can fill and block that
        # write, which looks like a hang. A real terminal always drains, so this
        # mirrors real conditions rather than papering over a bug.
        deadline = time.time() + 12
        while time.time() < deadline:
            try:
                os.set_blocking(self.master, False)
                data = os.read(self.master, 65536)
                if data:
                    self.vt.feed(data.decode("utf-8", "replace"))
            except (BlockingIOError, OSError):
                pass
            rc = self.proc.poll()
            if rc is not None:
                return rc
            time.sleep(0.02)
        self.proc.kill()
        return -1


def workspace():
    ws = tempfile.mkdtemp()
    with open(os.path.join(ws, "demo.txt"), "w") as f:
        f.write("hello world\nsecond line\n")
    return ws


spaces = []

print("== TUI: startup, streaming, auto-approve ==")
ws = workspace()
t = Tui(ws, ["-y"])
spaces.append(ws)
t.read(2.0, until="ready")
t.read(1.5, until="127.0.0.1:8123")
check("mode chip shown in the bottom-right status bar", t.saw("EXEC"), t)
check("workspace path shown in the status bar", t.saw(os.path.basename(ws)), t)
check("status bar renders model", t.saw("mock-coder"), t)
check("status bar shows ready state", t.saw("ready"), t)
check("powerline shows the endpoint", t.saw("127.0.0.1:8123"), t)
check("input placeholder rendered", t.saw("ask, or /help for commands"), t)
check("powerline shows model and endpoint",
      t.saw("mock-coder") and t.saw("127.0.0.1:8123") and t.saw("❯"), t)
check("full-auto flagged in the status bar", t.saw("FULL-AUTO"), t)

t.send("replace hello with goodbye in demo.txt\r")
t.read(8.0, until="replaced hello with goodbye")
check("user message echoed", t.saw("replace hello with goodbye in demo.txt"), t)
check("read view titles and names the file", t.saw("Read") and t.saw("demo.txt"), t)
check("edit view titles and names the file", t.saw("Edit") and t.saw("demo.txt"), t)
check("edit view shows diff stats", t.saw("+1") and t.saw("-1"), t)
check("assistant reply streamed", t.saw("replaced hello with goodbye"), t)
check("inline code styled in reply", t.saw("demo.txt"), t)
check("token count updated",
      t.wait_for(lambda s: re.search(r"[1-9]\d* tok", s) is not None), t)
with open(os.path.join(ws, "demo.txt")) as f:
    content = f.read()
check("file edited on disk", "goodbye world" in content, t)
check("rest of the file preserved", "second line" in content, t)
# `!cmd` runs a shell command directly, without an agent turn.
t.send("!echo koda-bang-works\r")
t.read(3.0, until="koda-bang-works")
check("!cmd runs a shell command directly", t.saw("koda-bang-works"), t)
check("!cmd shows the command echoed", t.saw("!echo koda-bang-works"), t)
rc = t.close()
check("exits cleanly on ctrl+d", rc == 0)

print("== TUI: approval modal ==")
ws2 = workspace()
spaces.append(ws2)
t2 = Tui(ws2)
t2.read(2.0, until="ready")
t2.send("replace hello with goodbye in demo.txt\r")
t2.read(8.0, until="EDIT FILE")
t2.read(0.6)
check("modal titled with the action", t2.saw("EDIT FILE"), t2)
check("modal shows the diff header", t2.saw("@@ -1,2 +1,2 @@"), t2)
check("modal shows the removed line", t2.saw("-hello world"), t2)
check("modal shows the added line", t2.saw("+goodbye world"), t2)
check("modal shows key hints", t2.saw("allow once") and t2.saw("decline"), t2)
t2.send("y")
t2.read(8.0, until="replaced hello with goodbye")
check("turn continues after approval", t2.saw("replaced hello with goodbye"), t2)
with open(os.path.join(ws2, "demo.txt")) as f:
    check("approved edit applied", "goodbye world" in f.read(), t2)
t2.close()

print("== TUI: denying a tool =="
      )
ws5 = workspace()
spaces.append(ws5)
t5 = Tui(ws5)
t5.read(2.0, until="ready")
t5.send("replace hello with goodbye in demo.txt\r")
t5.read(8.0, until="approve edit_file")
t5.send("n")
t5.read(3.0)
with open(os.path.join(ws5, "demo.txt")) as f:
    check("denied edit not applied", "hello world" in f.read(), t5)
check("returns to ready after denial", t5.saw("ready"), t5)
t5.close()

print("== TUI: interrupt a running turn ==")
ws4 = workspace()
spaces.append(ws4)
t4 = Tui(ws4, url=SLOW_URL)
t4.read(2.0, until="ready")
t4.send("count for me\r")
t4.read(4.0, until="counting: 1 2")
check("reply started streaming", t4.saw("counting: 1 2"), t4)
check("status shows interrupt hint", t4.saw("esc interrupt"), t4)
t4.send("\x03")  # ctrl+c
t4.read(3.0, until="cancelled")
check("interrupt acknowledged", t4.saw("interrupting"), t4)
check("turn reported cancelled", t4.saw("cancelled"), t4)
t4.read(0.8)
check("returns to ready state", t4.wait_for(lambda s: "ready" in s), t4)
t4.send("/cwd\r")
t4.read(2.0)
check("still accepts input after interrupt", t4.saw(os.path.basename(ws4)), t4)
t4.close()

print("== TUI: plan list, git-style diff, tables ==")
ws6 = tempfile.mkdtemp()
spaces.append(ws6)
with open(os.path.join(ws6, "calc.py"), "w") as f:
    f.write("def add(a, b):\n    return a - b\n\n\ndef mul(a, b):\n    return a * b\n")
t6 = Tui(ws6, ["-y"], url=SHOW_URL)
t6.read(2.0, until="ready")
t6.send("fix the bug in calc.py and verify\r")
t6.read(20, until="suite green")
t6.read(1.0)
check("plan block has a Tasks heading", t6.saw("Tasks"), t6)
check("plan tree marks done steps", t6.saw("[done]"), t6)
check("plan steps listed", t6.saw("fix the operator"), t6)
check("step counter in the composer", t6.saw("3/3 steps"), t6)
check("diff shown without asking", t6.saw("-    return a - b"), t6)
check("diff shows the replacement", t6.saw("+    return a + b"), t6)
check("diff has a hunk header", t6.saw("@@ -1,5 +1,5 @@"), t6)
check("diff numbers unchanged context", t6.saw("1  def add(a, b):"), t6)
check("card summary not repeated in diff",
      t6.screen().count("edited calc.py") == 0, t6)
check("table header rendered", t6.saw("check") and t6.saw("before"), t6)
check("table rule rendered", t6.saw("─────"), t6)
check("table rows aligned", t6.saw("add(2,3)") and t6.saw("1 failed"), t6)
check("task list uses checkboxes", t6.saw("☑ operator corrected"), t6)
t6.close()

print("== TUI: mode switching ==")
ws7 = tempfile.mkdtemp()
spaces.append(ws7)
t7 = Tui(ws7, url=SHOW_URL)
t7.read(2.0, until="ready")
check("execute mode is the default", t7.saw("EXEC"), t7)
t7.send("\x10")  # ctrl+p
t7.read(1.0)
check("ctrl+p reaches vibe mode", t7.saw("VIBE"), t7)
check("vibe mode is explained", t7.saw("writes a spec"), t7)
t7.send("\x10")
t7.read(1.0)
check("ctrl+p reaches plan mode", t7.saw("PLAN"), t7)
check("plan mode is explained", t7.saw("changes nothing"), t7)
t7.send("/mode execute\r")
t7.read(1.0)
check("/mode sets the mode", t7.saw("EXEC"), t7)
t7.close()

print("== TUI: file mentions, undo, sessions ==")
ws8 = tempfile.mkdtemp()
spaces.append(ws8)
os.makedirs(os.path.join(ws8, "src"))
for f in ["src/view.rs", "src/theme.rs", "README.md"]:
    with open(os.path.join(ws8, f), "w") as fh:
        fh.write("x\n")
t8 = Tui(ws8, ["-y"], url=SHOW_URL)
t8.read(2.5, until="ready")
check("welcome banner rendered", t8.saw("█"), t8)
t8.send("explain @vie")
t8.read(5.0, until="src/view.rs")
check("@ opens a fuzzy file list", t8.saw("src/view.rs"), t8)
t8.send("\t")
t8.read(1.0)
check("tab inserts the path", t8.saw("explain src/view.rs"), t8)
t8.send("\x15")  # ctrl+u clears the line
t8.read(0.5)
t8.send("/undo\r")
t8.read(1.5)
check("undo reports when there is nothing to undo",
      t8.saw("nothing to undo"), t8)
t8.send("/resume\r")
t8.read(1.5)
check("resume says so when there are no sessions",
      t8.saw("no saved sessions"), t8)
t8.send("/help\r")
t8.read(3.0, until="Examples")
check("help shows examples",
      t8.saw("Examples") and t8.saw("/mode plan"), t8)
t8.close()

print("== TUI: slash commands ==")
ws3 = workspace()
spaces.append(ws3)
t3 = Tui(ws3)
t3.read(2.0, until="ready")
t3.send("/")
t3.read(2.0, until="/models")
check("completion hint lists commands", t3.saw("/models") and t3.saw("/mode"), t3)
t3.send("\x15")  # ctrl+u: clear the slash so the hint stops overlaying
t3.send("/help\r")
t3.read(3.0, until="Examples")
check("/help shows command examples", t3.saw("Examples") and t3.saw("/mode plan"), t3)
check("/help lists a concrete example", t3.saw("/theme tokyo-night") or t3.saw("/auto"), t3)
t3.send("/tools\r")
t3.read(1.5, until="run_command")
check("/tools lists the tool suite", t3.saw("edit_file") and t3.saw("find_files"), t3)
t3.send("/auto\r")
t3.read(1.5, until="autonomy")
check("/auto cycles autonomy tier", t3.saw("autonomy:") and t3.saw("AUTO-WRITE"), t3)
t3.send("/think\r")
t3.read(1.0)
check("/think toggles reasoning display", t3.saw("reasoning hidden"), t3)
t3.send("/nope\r")
t3.read(1.0)
check("unknown command reports back", t3.saw("unknown command"), t3)
t3.close()

for d in spaces:
    subprocess.run(["rm", "-rf", d])

print(f"== summary ==\n  {passed} passed, {failed} failed")
sys.exit(1 if failed else 0)
