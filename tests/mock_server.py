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


def script(step, is_subagent=False):
    """Frames for the given conversation step.

    Parent and subagent are told apart by their system prompt, because each has
    its own fresh history and so both start at step 0.
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
        # Count tool results, in either protocol.
        step = sum(
            1
            for m in messages
            if m.get("role") == "tool"
            or (m.get("role") == "user" and str(m.get("content", "")).startswith("Tool result"))
        )

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
            for frame in script(step, is_sub):
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
