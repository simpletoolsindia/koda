#!/usr/bin/env python3
"""Mock OpenAI-compatible server used by tests/e2e.sh.

Modes (env MOCK_MODE):
  native  - stream native tool_calls deltas (arguments split across chunks)
  text    - stream <tool_call> blocks instead of native tool calls
  reject  - reject any request carrying `tools` with HTTP 400, then behave
            like `text`; exercises koda's automatic protocol fallback
  slow    - stream a long reply slowly, for interrupt tests
  empty   - HTTP 200 with an empty stream (a broken chat template looks like this)
  thinky  - stream only `reasoning_content`, never any content
  deleg   - delegate to a subagent, then answer using its report
  browse  - drive a real Chromium at a real page, then answer from it
  chat    - answer in one short paragraph, no tools (an install smoke test)
  verbose - long answers, fast: fills a context window so /compact has work
  searched- web_search with the tool ENABLED, and an answer that used it
            (`websearch` is the refused-because-disabled case the e2e asserts)
  custom  - call a custom tool declared in the workspace's koda.toml
  skill   - load a project skill, then answer from it
  watch   - answer a file-trigger turn with one small edit
  debugger- a real debugpy session: launch, breakpoint, continue, evaluate

Scripted conversation, driven by how many tool results the request contains:
  0 -> call read_file(demo.txt)
  1 -> call edit_file(demo.txt, hello -> goodbye)
  2 -> final assistant text
"""

import json
import os
import time
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODE = os.environ.get("MOCK_MODE", "native")
MODEL = "mock-coder"


def sse(obj):
    return f"data: {json.dumps(obj)}\n\n".encode()


def delta(d, finish=None):
    choice = {"index": 0, "delta": d}
    if finish:
        choice["finish_reason"] = finish
    return sse({"object": "chat.completion.chunk", "model": MODEL, "choices": [choice]})


def tool_call_frames(call_id, name, args_json):
    """Native tool call, deliberately fragmented to test accumulation."""
    frames = [
        delta({
            "tool_calls": [{
                "index": 0,
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": ""},
            }]
        })
    ]
    blob = json.dumps(args_json)
    for i in range(0, len(blob), 7):
        frames.append(delta({
            "tool_calls": [{"index": 0, "function": {"arguments": blob[i:i + 7]}}]
        }))
    frames.append(delta({}, finish="tool_calls"))
    return frames


DELAY = 0.25 if MODE == "slow" else 0.0


def text_frames(text, finish="stop"):
    frames = []
    for i in range(0, len(text), 5):
        frames.append(delta({"content": text[i:i + 5]}))
    frames.append(delta({}, finish=finish))
    return frames


def script(step, is_subagent=False, last_user=""):
    """Frames for the given conversation step.

    Parent and subagent are told apart by their system prompt, because each has
    its own fresh history and so both start at step 0. `last_user` is the most
    recent user message, for the few modes whose second turn differs.
    """
    if MODE == "undo":
        if step == 0:
            return tool_call_frames("u1", "edit_file",
                {"path": "demo.txt", "old": "hello", "new": "wrecked"})
        return text_frames("Changed it.")
    if MODE == "learn":
        if step == 0:
            return tool_call_frames("l1", "run_command", {"command": "echo built"})
        if step == 1:
            return tool_call_frames("l2", "remember",
                {"note": "the build command here is echo built"})
        return text_frames("Noted the build command.")
    if MODE == "askopts":
        # Ask the user with a dropdown of options.
        if step == 0:
            return tool_call_frames("a1", "ask_user", {
                "question": "Which database should I use?",
                "options": ["PostgreSQL", "SQLite", "MySQL"],
            })
        return text_frames("Got it.")
    if MODE == "correct_write":
        # koda WRITES svc.py using `logging` (populates its last_writes record).
        if step == 0:
            return tool_call_frames("c1", "write_file",
                {"path": "svc.py", "content": "import logging\nx = logging\n"})
        return text_frames("Wrote svc.py.")
    if MODE == "correct_read":
        # koda READS svc.py — by now the user swapped logging->log.audit on
        # disk, so koda detects the correction against what it last wrote.
        if step == 0:
            return tool_call_frames("c1", "read_file", {"path": "svc.py"})
        return text_frames("Read svc.py.")
    if MODE == "websearch":
        if step == 0:
            return tool_call_frames("w1", "web_search", {"query": "ratatui docs"})
        return text_frames("I could not search; web search is off.")
    if MODE == "graph":
        if step == 0:
            return tool_call_frames("g1", "codegraph", {"query": "overview"})
        if step == 1:
            return tool_call_frames("g2", "codegraph",
                                    {"query": "symbol", "name": "build_widget"})
        return text_frames("build_widget is defined in src/lib.rs and used by src/main.rs.")
    if MODE == "showcase":
        if step == 0:
            return tool_call_frames("t1", "todo", {"items": [
                {"text": "read calc.py to find the bug", "status": "in_progress"},
                {"text": "fix the operator", "status": "pending"},
                {"text": "run the tests", "status": "pending"}]})
        if step == 1:
            return tool_call_frames("t2", "read_file", {"path": "calc.py"})
        if step == 2:
            return tool_call_frames("t3", "edit_file",
                {"path": "calc.py", "old": "return a - b", "new": "return a + b"})
        if step == 3:
            return tool_call_frames("t4", "todo", {"items": [
                {"text": "read calc.py to find the bug", "status": "done"},
                {"text": "fix the operator", "status": "done"},
                {"text": "run the tests", "status": "done"}]})
        return text_frames(
            "Fixed `add()` — it was subtracting.\n\n"
            "| check | before | after |\n|---|---|---|\n"
            "| add(2,3) | -1 | 5 |\n| tests | 1 failed | 3 passed |\n\n"
            "- [x] operator corrected\n- [x] suite green\n")
    if MODE == "searched":
        if step == 0:
            return tool_call_frames("s1", "web_search",
                {"query": "ratatui layout documentation"})
        return text_frames(
            "Layouts are built with `Layout::default()`, split by `Constraint`s "
            "(`Length`, `Percentage`, `Min`, `Max`) into the rects you draw into. "
            "The tutorials page is the fastest way in — these are search snippets, "
            "so worth opening the real page before relying on them."
        )
    if MODE == "browse":
        if step == 0:
            return tool_call_frames("b1", "browse", {
                "action": "navigate",
                "url": "https://simpletoolsindia.github.io/koda/demos/",
            })
        return text_frames(
            "That page is koda's own demo gallery — every recording on it is the "
            "real binary driven through a pty against a scripted model."
        )
    if MODE == "verbose":
        # Deliberately long, and streamed with no delay: some screens (the
        # context gauge, /compact) only mean anything once a conversation has
        # real weight behind it.
        return text_frames(
            "Here is the walkthrough you asked for.\n\nThe billing package splits into three layers. `calc.py` holds the pure arithmetic and knows nothing about orders, customers or currency; every function in it takes numbers and returns numbers, which is why it is the only module with exhaustive unit tests. `orders.py` sits on top and turns a cart into a sequence of those calls, applying the per-line discount before tax rather than after, because the tax authority requires the discounted price to be the taxable base. `invoice.py` renders the result and is the only place that formats money as a string.\n\nThe failure you saw came from the middle layer. `apply_discount` was being handed a percentage where it expected a fraction, so a ten percent discount multiplied the line by ten instead of by nine tenths. The unit tests did not catch it because they exercise `calc.py` directly with fractions, and the integration test that would have caught it was skipped when the fixture data moved.\n\nWhat I would change, in order: normalise at the boundary so a percentage never reaches the arithmetic layer; give `apply_discount` an assertion that its argument is between zero and one; un-skip the integration test and point it at the new fixture; and add a property test that a discount never increases a total. The first two are five minutes each. The last one is the one that keeps this from coming back.\n"
        )
    if MODE == "chat":
        return text_frames(
            "Yes — I am running against your local server, and I can see this "
            "project. Ask me for a change and I will show you the diff first."
        )
    if MODE == "debugger":
        # A real DAP session against debugpy; only the model's choices are
        # scripted. The line is inside the loop, so `running` is partial.
        if step == 0:
            return tool_call_frames("d1", "debug",
                {"action": "launch", "program": "cart.py"})
        if step == 1:
            return tool_call_frames("d2", "debug",
                {"action": "set_breakpoint", "file": "cart.py", "line": 9})
        if step == 2:
            return tool_call_frames("d3", "debug", {"action": "continue"})
        if step == 3:
            return tool_call_frames("d4", "debug",
                {"action": "evaluate", "expression": "running, price, DISCOUNT"})
        # The session stays open after the answer, so the next turn can close
        # it — which is also how a person uses it.
        if "done" in last_user.lower() or "terminate" in last_user.lower():
            if step == 5:
                return text_frames("Session closed.")
            return tool_call_frames("d5", "debug", {"action": "terminate"})
        return text_frames(
            "Stopped inside the loop on the first item: `running` is 0.0 before "
            "the add, `price` is 1200 and `DISCOUNT` is 0.9 — so the discount is "
            "applied per item, not to the order."
        )
    if MODE == "custom":
        # A shell command declared in koda.toml, called like a built-in.
        if step == 0:
            return tool_call_frames("x1", "check", {})
        return text_frames("The project gate passes: fmt, clippy and tests are clean.")
    if MODE == "skill":
        if step == 0:
            return tool_call_frames("s1", "skill", {"name": "migrations"})
        return text_frames(
            "Following the project's `migrations` skill: reversible, one change "
            "per file, and named with today's date."
        )
    if MODE == "watch":
        if step == 0:
            return tool_call_frames("w1", "edit_file", {
                "path": "calc.py",
                "old": "def mul(a, b):\n    return a * b",
                "new": "def mul(a, b):\n    \"\"\"Multiply two numbers.\"\"\"\n    return a * b",
            })
        return text_frames("Added the docstring the AI! comment asked for.")
    if MODE == "deleg":
        if is_subagent:
            if step == 0:
                return tool_call_frames("call_r", "read_file", {"path": "demo.txt"})
            return text_frames("SUBREPORT: demo.txt line 1 holds `hello world`.")
        if step == 0:
            return tool_call_frames("call_d", "delegate",
                                    {"task": "find where the greeting lives"})
        return text_frames("The greeting is in demo.txt line 1, per the subagent.")

    if MODE == "docread":
        # Read a document fixture (path from DOC_PATH), then echo a short reply.
        # Used by the doc-parsing e2e to prove read_file extracts DOCX/XLSX/PDF.
        if step == 0:
            return tool_call_frames("d1", "read_file",
                {"path": os.environ.get("DOC_PATH", "tiny.csv")})
        return text_frames("Read the document.")

    if MODE == "cut":
        # Stream a few content frames, then drop the connection mid-stream
        # WITHOUT sending [DONE] or the terminating chunk — reproduces the
        # "unexpected EOF during chunk" failure. koda should keep the partial
        # reply rather than failing the whole turn.
        return [
            delta({"content": "Here is the first part of the answer"}),
            delta({"content": " that streamed fine before the drop."}),
            "__CUT__",  # sentinel: the send loop closes the socket here
        ]

    if MODE == "empty":
        return []
    if MODE == "thinky":
        return [
            delta({"reasoning_content": "Let me think about this. "}),
            delta({"reasoning_content": "Still thinking. "}),
            delta({}, finish="stop"),
        ]
    if MODE == "slow":
        return text_frames("counting: " + " ".join(str(i) for i in range(1, 200)))
    if step == 0:
        if MODE == "native":
            return tool_call_frames("call_a", "read_file", {"path": "demo.txt"})
        return text_frames(
            'Reading the file first.\n<tool_call>\n'
            '{"name": "read_file", "arguments": {"path": "demo.txt"}}\n</tool_call>'
        )
    if step == 1:
        args = {"path": "demo.txt", "old": "hello", "new": "goodbye"}
        if MODE == "native":
            return tool_call_frames("call_b", "edit_file", args)
        return text_frames(
            'Applying the edit.\n<tool_call>\n'
            + json.dumps({"name": "edit_file", "arguments": args})
            + "\n</tool_call>"
        )
    return text_frames("Done: replaced hello with goodbye in `demo.txt`.")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            body = json.dumps({
                "object": "list",
                "data": [{"id": MODEL, "object": "model"}],
            }).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        req = json.loads(self.rfile.read(length) or b"{}")

        if MODE == "reject" and req.get("tools"):
            body = json.dumps({
                "error": {"message": "tools are not supported by this model"}
            }).encode()
            self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        messages = req.get("messages", [])
        # A compaction request (no tools, "Summarize the conversation" system
        # prompt) gets a canned summary so /compact completes deterministically
        # in tests, regardless of MODE.
        is_compaction = any(
            m.get("role") == "system"
            and "Summarize the conversation" in str(m.get("content", ""))
            for m in messages
        )
        if is_compaction:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            try:
                for frame in text_frames("SUMMARY: earlier work condensed for the record."):
                    self.write_chunk(frame)
                self.write_chunk(b"data: [DONE]\n\n")
                self.wfile.write(b"0\r\n\r\n")
                self.wfile.flush()
            except BrokenPipeError:
                pass
            return
        # Count tool results, in either protocol.
        step = sum(
            1
            for m in messages
            if m.get("role") == "tool"
            or (m.get("role") == "user" and str(m.get("content", "")).startswith("Tool result"))
        )

        last_user = ""
        for m in messages:
            if m.get("role") == "user":
                last_user = str(m.get("content", ""))

        is_sub = any(
            m.get("role") == "system" and "research subagent" in str(m.get("content", ""))
            for m in messages
        )

        # In docread mode, dump any tool-result content we receive so the e2e
        # can assert on the exact text read_file extracted from the document.
        if MODE == "docread":
            cap = os.environ.get("DOC_CAPTURE")
            if cap:
                with open(cap, "a") as fh:
                    for m in messages:
                        if m.get("role") == "tool" or (
                            m.get("role") == "user"
                            and str(m.get("content", "")).startswith("Tool result")
                        ):
                            fh.write(str(m.get("content", "")) + "\n")

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        try:
            for frame in script(step, is_sub, last_user):
                if frame == "__CUT__":
                    # Abruptly drop the connection mid-stream: no [DONE], no
                    # terminating 0-length chunk. The client sees EOF while it
                    # still expects more chunked data.
                    try:
                        self.wfile.flush()
                        self.connection.close()
                    except Exception:
                        pass
                    return
                self.write_chunk(frame)
                if DELAY:
                    time.sleep(DELAY)
            self.write_chunk(b"data: [DONE]\n\n")
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        except BrokenPipeError:
            pass

    def write_chunk(self, payload):
        self.wfile.write(f"{len(payload):X}\r\n".encode() + payload + b"\r\n")
        self.wfile.flush()


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8123
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"mock server on 127.0.0.1:{port} mode={MODE}", flush=True)
    srv.serve_forever()
