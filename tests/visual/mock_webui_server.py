#!/usr/bin/env python3
"""Serve the real web-ui/dist/index.html plus realistic koda API responses so
Playwright can visually exercise every navigation tab — including an in-flight
LLM debug session (to show the live "processing" view), skills/agents, and the
system prompt editor. Mirrors the shapes koda's webui.rs returns."""
import copy, json, os, sys, time
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

# ---- trace fixtures: one finished turn (2 model calls, 3 tools, a compaction)
# and one still running, which is what the console pins to the top. ----
def _req(messages, model="mtplx-qwen38-27b"):
    return json.dumps({"model": model, "messages": messages, "stream": True}, indent=2)

SYS = {"role": "system", "content": "You are koda, a terminal coding agent.\nBe precise."}
TURN_1 = {
    "id": 1, "started": 2.5, "ended": 21.75, "mode": "execute",
    "model": "mtplx-qwen38-27b", "endpoint": "http://127.0.0.1:20128/v1",
    "input": "Fix the off-by-one in paginate() and add a test",
    "status": "ok", "reply": "Fixed the slice bound and added a regression test.",
    "tokens": 8421,
    "steps": [
        {"seq": 0, "kind": "model", "label": "mtplx-qwen38-27b", "started": 2.5, "ms": 4210,
         "running": False,
         "model": {
             "request": _req([SYS, {"role": "user", "content": "Fix the off-by-one in paginate() and add a test"}]),
             "response": 'data: {"choices":[{"delta":{"content":"Let me read view.rs"}}]}\n\ndata: [DONE]\n\n',
             "reasoning": "The slice bound looks wrong; check paginate() first.",
             "text": "Let me read view.rs", "finish_reason": "tool_calls", "retries": 0,
             "prompt_tokens": 1820, "completion_tokens": 24,
             "tool_calls": ["read_file"], "error": None}},
        {"seq": 1, "kind": "tool", "label": "read_file", "started": 6.8, "ms": 38, "running": False,
         "tool": {"name": "read_file", "args": '{\n  "path": "src/view.rs"\n}', "ok": True,
                  "summary": "read src/view.rs (1910 lines)", "detail": "pub fn paginate(...) { ... }",
                  "approval": "auto", "diff": None}},
        {"seq": 2, "kind": "compaction", "label": "compaction", "started": 7.0, "ms": 1900,
         "running": False, "note": "18400 → 4200 tokens"},
        {"seq": 3, "kind": "model", "label": "mtplx-qwen38-27b", "started": 9.1, "ms": 5300,
         "running": False,
         "model": {
             "request": _req([SYS,
                              {"role": "user", "content": "[Context was compacted here] hand-off note…"},
                              {"role": "assistant", "content": "Understood — continuing."},
                              {"role": "user", "content": "Fix the off-by-one in paginate() and add a test"}]),
             "response": 'data: {"choices":[{"delta":{"content":"Applying the fix"}}]}\n\ndata: [DONE]\n\n',
             "reasoning": "", "text": "Applying the fix", "finish_reason": "tool_calls",
             "retries": 1, "prompt_tokens": 4200, "completion_tokens": 30,
             "tool_calls": ["edit_file", "run_command"], "error": None}},
        {"seq": 4, "kind": "tool", "label": "edit_file", "started": 14.6, "ms": 62, "running": False,
         "tool": {"name": "edit_file", "args": '{\n  "path": "src/view.rs"\n}', "ok": True,
                  "summary": "edit src/view.rs (+3 -1)", "detail": "applied 1 edit",
                  "approval": "approved",
                  "diff": "-    let end = start + n;\n+    let end = (start + n).min(len);"}},
        {"seq": 5, "kind": "tool", "label": "run_command", "started": 15.0, "ms": 6400,
         "running": False,
         "tool": {"name": "run_command", "args": '{\n  "command": "rm -rf /tmp/x"\n}', "ok": False,
                  "summary": "run_command: denied", "detail": "ERROR: the user denied this action.",
                  "approval": "denied", "diff": None}},
    ],
}
TURN_2 = {
    "id": 2, "started": 40.0, "ended": None, "mode": "vibe",
    "model": "mtplx-qwen38-27b", "endpoint": "http://127.0.0.1:20128/v1",
    "input": "Now write the changelog entry", "status": "running", "reply": "", "tokens": 4600,
    "steps": [
        {"seq": 0, "kind": "model", "label": "mtplx-qwen38-27b", "started": 40.0, "ms": 0,
         "running": True,
         "model": {"request": _req([SYS, {"role": "user", "content": "Now write the changelog entry"}]),
                   "response": 'data: {"choices":[{"delta":{"content":"## Unreleased"}}]}\n\n',
                   "reasoning": "", "text": "## Unreleased", "finish_reason": None, "retries": 0,
                   "prompt_tokens": 4600, "completion_tokens": 6, "tool_calls": [], "error": None}},
    ],
}


def summary(t):
    return {
        "id": t["id"], "started": t["started"],
        "ms": int(((t["ended"] or t["started"] + 3) - t["started"]) * 1000),
        "mode": t["mode"], "model": t["model"], "input": t["input"], "status": t["status"],
        "steps": len(t["steps"]),
        "model_calls": len([s for s in t["steps"] if s["kind"] == "model"]),
        "tool_calls": len([s for s in t["steps"] if s["kind"] == "tool"]),
        "tokens": t["tokens"], "reply": t["reply"], "running": t["status"] == "running",
    }


TURNS = {1: TURN_1, 2: TURN_2}

CONFIG = {
    "model": "mtplx-qwen38-27b", "base_url": "http://127.0.0.1:20128/v1",
    "mode": "execute", "auto_tier": "ask", "reasoning_effort": "medium",
    "temperature": 0.2, "max_steps": 40,
    "toggles": {"learning": True, "memory": True, "codegraph": True, "web_search": False,
                "web_fetch": True, "subagents": True, "sessions": True, "debug": False,
                "watch": False},
    "has_api_key": True, "config_path": "~/.config/koda/config.toml",
    "modes": ["plan", "execute", "vibe"], "tiers": ["ask", "write", "full"],
    "efforts": ["off", "low", "medium", "high"],
}
MEMORY = {
    "notes": ["Tests run with cargo test -- --test-threads=1",
              "The web UI is built by web-ui/build.sh"],
    "commands": [{"command": "cargo test", "ok": 12, "failed": 1},
                 {"command": "bash web-ui/build.sh", "ok": 4, "failed": 0}],
    "hot_files": [{"path": "src/agent.rs", "edits": 9}],
    "path": "/proj/.koda/memory.md",
}
LEARNING = {
    "accepted": [{"key": "cmd.test", "text": "Run tests with cargo test -- --test-threads=1",
                  "support": 4, "accepted": True}],
    "candidates": [{"key": "naming.fn.case", "text": "Functions here use snake_case",
                    "support": 6, "accepted": False},
                   {"key": "import.serde", "text": "serde_json is imported in most modules",
                    "support": 3, "accepted": False}],
    "brief": "1 accepted rule",
}
SESSIONS = {"sessions": [
    {"id": "20260901-142233", "model": "mtplx-qwen38-27b", "endpoint": "http://127.0.0.1:20128/v1",
     "started": 1756000000, "messages": 42, "title": "Fix the off-by-one in paginate()",
     "modified": 1756000900, "ago": "12m ago", "path": "/proj/.koda/sessions/20260901-142233.jsonl"},
    {"id": "20260831-090501", "model": "granite4.1:8b", "endpoint": "http://127.0.0.1:11434/v1",
     "started": 1755900000, "messages": 18, "title": "Add document parsing",
     "modified": 1755903000, "ago": "1d ago", "path": "/proj/.koda/sessions/20260831-090501.jsonl"},
]}


# Pristine copies, restored by POST /api/__reset so each test is independent.
CONFIG_0 = copy.deepcopy(CONFIG)
MEMORY_0 = copy.deepcopy(MEMORY)
LEARNING_0 = copy.deepcopy(LEARNING)
SKILLS_0 = copy.deepcopy(SKILLS)


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
            payload = {'version': len(LOG_ENTRIES), 'entries': LOG_ENTRIES}
            trace = {"enabled": True, "version": 7,
                     "turns": [summary(TURNS[2]), summary(TURNS[1])], "live": TURNS[2]}
            return self._send(200, "text/event-stream",
                              f"event: logs\ndata: {json.dumps(payload)}\n\n"
                              f"event: trace\ndata: {json.dumps(trace)}\n\n")
        if path == "/api/trace":
            return self._send(200, "application/json", json.dumps({
                "enabled": True, "version": 7,
                "turns": [summary(TURNS[2]), summary(TURNS[1])],
                "live": TURNS[2],
            }))
        if path.startswith("/api/trace/"):
            try: tid = int(path[len("/api/trace/"):])
            except ValueError: tid = -1
            if tid in TURNS:
                return self._send(200, "application/json", json.dumps(TURNS[tid]))
            return self._send(404, "application/json", '{"error":"no such turn"}')
        if path == "/api/config":
            return self._send(200, "application/json", json.dumps(CONFIG))
        if path == "/api/memory":
            return self._send(200, "application/json", json.dumps(MEMORY))
        if path == "/api/learning":
            return self._send(200, "application/json", json.dumps(LEARNING))
        if path == "/api/sessions":
            return self._send(200, "application/json", json.dumps(SESSIONS))
        if path == "/api/codegraph/symbol":
            q = self.path.split("?", 1)[1] if "?" in self.path else ""
            name = ""
            for kv in q.split("&"):
                if kv.startswith("name="):
                    from urllib.parse import unquote_plus
                    name = unquote_plus(kv[5:])
            known = {n["id"]: n for n in GRAPH["nodes"]}
            if name in known:
                n = known[name]
                return self._send(200, "application/json", json.dumps({
                    "ok": True, "name": name,
                    "report": f"{name} ({n['kind']}) defined at {n['file']}:{n['line']}\n"
                              f"used in {n['refs']} file(s)",
                    "defs": [{"kind": n["kind"], "file": n["file"], "line": n["line"]}],
                    "refs": ["src/tui.rs"],
                }))
            return self._send(200, "application/json", json.dumps({
                "ok": False, "name": name, "report": "", "defs": [], "refs": [],
                "error": f"no symbol named {name!r} in the index"}))
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
        if self.path == "/api/__reset":
            # Tests mutate this fixture server; each one starts from the same
            # state so assertions do not depend on run order.
            global SKILLS
            CONFIG.clear(); CONFIG.update(copy.deepcopy(CONFIG_0))
            MEMORY.clear(); MEMORY.update(copy.deepcopy(MEMORY_0))
            LEARNING.clear(); LEARNING.update(copy.deepcopy(LEARNING_0))
            SKILLS = copy.deepcopy(SKILLS_0)
            SYSTEM_PROMPT["custom"] = ""
            return self._send(200, "application/json", '{"ok":true}')
        if self.path == "/api/config":
            # Mirror webui.rs: reject bad input, otherwise apply and echo ok.
            if data.get("mode") and data["mode"] not in CONFIG["modes"]:
                return self._send(200, "application/json",
                                  json.dumps({"ok": False, "error": "unknown mode"}))
            for k in ("model", "base_url", "mode", "auto_tier", "reasoning_effort",
                      "temperature", "max_steps"):
                if k in data: CONFIG[k] = data[k]
            for k, v in (data.get("toggles") or {}).items():
                CONFIG["toggles"][k] = v
            return self._send(200, "application/json",
                              json.dumps({"ok": True, "note": "applied to the running session"}))
        if self.path == "/api/memory":
            if data.get("remember"):
                MEMORY["notes"].append(data["remember"])
            elif data.get("forget"):
                MEMORY["notes"] = [x for x in MEMORY["notes"] if data["forget"] not in x]
            else:
                return self._send(200, "application/json",
                                  json.dumps({"ok": False, "error": "expected remember or forget"}))
            return self._send(200, "application/json", json.dumps({"ok": True}))
        if self.path == "/api/learning":
            if data.get("accept") == "all":
                LEARNING["accepted"] += [dict(r, accepted=True) for r in LEARNING["candidates"]]
                LEARNING["candidates"] = []
            elif isinstance(data.get("accept"), int):
                i = data["accept"] - 1
                if 0 <= i < len(LEARNING["candidates"]):
                    LEARNING["accepted"].append(dict(LEARNING["candidates"].pop(i), accepted=True))
            elif isinstance(data.get("reject"), int):
                i = data["reject"] - 1
                if 0 <= i < len(LEARNING["candidates"]):
                    LEARNING["candidates"].pop(i)
            else:
                return self._send(200, "application/json",
                                  json.dumps({"ok": False, "error": "expected accept or reject"}))
            return self._send(200, "application/json", json.dumps({"ok": True}))
        if self.path.startswith("/api/sessions/"):
            rest = self.path[len("/api/sessions/"):]
            sid, _, action = rest.partition("/")
            if any(s["id"] == sid for s in SESSIONS["sessions"]):
                return self._send(200, "application/json",
                                  json.dumps({"ok": True, (action or "resume"): sid}))
            return self._send(200, "application/json",
                              json.dumps({"ok": False, "error": f"no session {sid}"}))
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
