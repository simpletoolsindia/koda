#!/usr/bin/env python3
"""Serve the real web-ui/dist/index.html plus realistic koda API responses so
Playwright can visually exercise every navigation tab — including an in-flight
LLM debug session (to show the live "processing" view), skills/agents, and the
system prompt editor. Mirrors the shapes koda's webui.rs returns."""
import json, os, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DIST = os.path.join(ROOT, "web-ui", "dist", "index.html")

# In-memory skills store so POST/DELETE visibly change the list.
SKILLS = [
    {"name": "rust-error-handling", "when": "writing Rust that returns Result",
     "role": None, "body": "Prefer ? over unwrap. Use anyhow::Context for messages.",
     "source": "/proj/.koda/skills/rust-error-handling.md"},
    {"name": "senior-reviewer", "when": "reviewing a diff",
     "role": "reviewer", "body": "Check for edge cases, tests, and clear naming.",
     "source": "/proj/.koda/skills/senior-reviewer.md"},
]
SYSTEM_PROMPT = {"custom": ""}  # empty => built-in
BUILTIN = ("You are koda, a terminal coding agent. Be precise, edit files "
           "directly, verify with tests, and keep replies concise.")

# One completed session and one still streaming (no [DONE]) to show live view.
REQ_COMPLETE = json.dumps({
    "model": "mtplx-qwen38-27b",
    "messages": [
        {"role": "system", "content": "You are koda, a terminal coding agent."},
        {"role": "user", "content": "Fix the off-by-one in paginate()."},
    ],
    "tools": [{"function": {"name": "read_file", "description": "Read a file"}},
              {"function": {"name": "edit_file", "description": "Edit a file"}}],
})
RES_COMPLETE = (
    'data: {"choices":[{"delta":{"content":"Found it — the slice used <= instead of <. "}}]}\n\n'
    'data: {"choices":[{"delta":{"content":"Fixed and added a test."}}]}\n\n'
    'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\n'
    'data: [DONE]\n\n'
)
REQ_INFLIGHT = json.dumps({
    "model": "mtplx-qwen38-27b",
    "messages": [
        {"role": "system", "content": "You are koda, a terminal coding agent."},
        {"role": "user", "content": "Now write the changelog entry."},
    ],
    "tools": [{"function": {"name": "write_file", "description": "Write a file"}}],
})
# No [DONE], no finish_reason => the UI shows this as "processing".
RES_INFLIGHT = 'data: {"choices":[{"delta":{"content":"## Unreleased\\n- Fixed pagination"}}]}\n\n'

LOG_ENTRIES = [
    {"seq": i, "at": time.time() - (20 - i), "level": lvl, "area": area, "message": msg, "fields": fields}
    for i, (lvl, area, msg, fields) in enumerate([
        ("info", "agent", "session start", [["model", "mtplx-qwen38-27b"]]),
        ("info", "http", "stream complete", [["events", "42"], ["ms", "1180"]]),
        ("warn", "http", "request retried once", [["attempt", "2"]]),
        ("info", "tool", "edit demo.txt", [["+lines", "3"], ["-lines", "1"]]),
        ("error", "agent", "turn failed: unexpected EOF", [["detail", "connection reset"]]),
        ("info", "memory", "saved", [["file", "~/.koda/memory.md"]]),
    ])
]

GRAPH = {
    "files": 28, "languages": [["rust", 22], ["python", 4], ["toml", 2]],
    "nodes": [
        {"id": "paginate", "kind": "fn", "file": "src/view.rs", "line": 120, "refs": 5},
        {"id": "App", "kind": "struct", "file": "src/tui.rs", "line": 210, "refs": 18},
        {"id": "compact", "kind": "method", "file": "src/agent.rs", "line": 2510, "refs": 2},
        {"id": "read_document", "kind": "fn", "file": "src/tools.rs", "line": 786, "refs": 3},
    ],
    "edges": [
        {"from": "src/view.rs", "to": "paginate", "kind": "defines"},
        {"from": "src/tui.rs", "to": "App", "kind": "defines"},
        {"from": "src/agent.rs", "to": "compact", "kind": "defines"},
        {"from": "src/tools.rs", "to": "read_document", "kind": "defines"},
    ],
    "truncated": False,
}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass

    def _send(self, code, ctype, body):
        if isinstance(body, str): body = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("/", "/index.html"):
            with open(DIST, "r") as f: return self._send(200, "text/html; charset=utf-8", f.read())
        if path == "/api/logs":
            return self._send(200, "application/json", json.dumps({"version": len(LOG_ENTRIES), "entries": LOG_ENTRIES}))
        if path == "/api/events":
            return self._send(200, "text/event-stream",
                              f"event: logs\ndata: {json.dumps({'version': len(LOG_ENTRIES), 'entries': LOG_ENTRIES})}\n\n")
        if path == "/api/debug":
            return self._send(200, "application/json", json.dumps({
                "enabled": True, "dir": "~/.koda/debug",
                "sessions": [
                    {"id": "rr-session-1", "request": REQ_COMPLETE, "response": RES_COMPLETE},
                    {"id": "rr-session-2", "request": REQ_INFLIGHT, "response": RES_INFLIGHT},
                ],
            }))
        if path == "/api/codegraph":
            return self._send(200, "application/json", json.dumps(GRAPH))
        if path == "/api/skills":
            return self._send(200, "application/json", json.dumps(SKILLS))
        if path == "/api/settings":
            return self._send(200, "application/json", json.dumps({
                "system_prompt": SYSTEM_PROMPT["custom"],
                "builtin_prompt": BUILTIN,
                "using_builtin": SYSTEM_PROMPT["custom"].strip() == "",
                "config_path": "~/.config/koda/config.toml",
            }))
        return self._send(404, "application/json", '{"error":"not found"}')

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n).decode() if n else "{}"
        try: data = json.loads(body)
        except Exception: data = {}
        if self.path == "/api/skills":
            SKILLS.append({"name": data.get("name", "new"), "when": data.get("when", ""),
                           "role": data.get("role"), "body": data.get("body", ""),
                           "source": f"/proj/.koda/skills/{data.get('name','new')}.md"})
            return self._send(200, "application/json", '{"ok":true,"path":"/proj/.koda/skills/new.md"}')
        if self.path == "/api/settings":
            SYSTEM_PROMPT["custom"] = data.get("system_prompt", "")
            using = SYSTEM_PROMPT["custom"].strip() == ""
            return self._send(200, "application/json",
                              json.dumps({"ok": True, "path": "~/.config/koda/config.toml", "using_builtin": using}))
        return self._send(404, "application/json", '{"error":"not found"}')

    def do_DELETE(self):
        if self.path.startswith("/api/skills/"):
            name = self.path[len("/api/skills/"):]
            global SKILLS
            SKILLS = [s for s in SKILLS if s["name"] != name]
            return self._send(200, "application/json", '{"ok":true}')
        return self._send(404, "application/json", '{"error":"not found"}')


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8790
    srv = ThreadingHTTPServer(("127.0.0.1", port), H)
    print(f"mock webui on 127.0.0.1:{port}", flush=True)
    srv.serve_forever()
