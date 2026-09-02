#!/usr/bin/env python3
"""End-to-end check of the trace-first control center against a real koda.

Runs the release binary in a PTY with the web UI enabled, pointed at the mock
LLM server, then:
  1) drives one real turn (streaming reply + read_file + edit_file),
  2) asserts /api/trace reconstructed that turn: model calls, tool calls,
     the exact request body, the raw SSE, and the arguments/diff,
  3) asserts a POST to /api/config is applied to the *live* session (mode
     changes in the running TUI, not just on disk),
  4) asserts POST /api/memory reaches the agent (the note lands in memory.md).

Usage:  PORT=8123 python3 tests/probe_trace.py     (mock server must be up, or
        this script starts one itself)
"""
import importlib.util, json, os, subprocess, sys, tempfile, time, urllib.error, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
os.chdir(ROOT)

# Reuse the PTY/vt100 harness from probe_compact.py rather than duplicating it.
spec = importlib.util.spec_from_file_location("probe_compact", os.path.join(HERE, "probe_compact.py"))
pc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pc)

MOCK_PORT = os.environ.get("PORT", "8123")
WEB_PORT = int(os.environ.get("WEB_PORT", "8791"))
# By default this probe is hermetic (mock LLM server). Point it at a real
# backend — the machine's OmniRoute, say — with:
#   BACKEND=config python3 tests/probe_trace.py
# which reads base_url/model/api_key from the user's koda config.
BACKEND = os.environ.get("BACKEND", "mock")
USER_CONFIG = os.path.join(os.path.expanduser("~"), ".config", "koda", "config.toml")


def user_config_value(key, default=""):
    if not os.path.exists(USER_CONFIG):
        return default
    for line in open(USER_CONFIG):
        line = line.strip()
        if line.startswith(f"{key}") and "=" in line:
            k, _, v = line.partition("=")
            if k.strip() == key:
                return v.strip().strip('"')
    return default


if BACKEND == "config":
    pc.URL = user_config_value("base_url", "http://localhost:20128/v1")
    pc.MODEL = user_config_value("model", "auto")
else:
    pc.PORT = MOCK_PORT
    pc.URL = f"http://127.0.0.1:{MOCK_PORT}/v1"
    pc.MODEL = "mock-coder"

passed = failed = 0


def check(name, cond, extra=""):
    global passed, failed
    if cond:
        print(f"  ok   {name}")
        passed += 1
    else:
        print(f"  FAIL {name}  {extra}")
        failed += 1


def get(path):
    with urllib.request.urlopen(f"http://127.0.0.1:{WEB_PORT}{path}", timeout=5) as r:
        return json.loads(r.read().decode())


def post(path, payload):
    req = urllib.request.Request(
        f"http://127.0.0.1:{WEB_PORT}{path}",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=5) as r:
        return json.loads(r.read().decode())


def wait_for_web(timeout=15):
    end = time.time() + timeout
    while time.time() < end:
        try:
            get("/api/trace")
            return True
        except Exception:
            time.sleep(0.2)
    return False


def start_mock():
    """Start the mock LLM server unless we're driving a real backend."""
    if BACKEND == "config":
        return None
    try:
        urllib.request.urlopen(f"http://127.0.0.1:{MOCK_PORT}/v1/models", timeout=2).read()
        return None
    except Exception:
        pass
    env = dict(os.environ, MOCK_MODE="native")
    proc = subprocess.Popen([sys.executable, os.path.join(HERE, "mock_server.py"), MOCK_PORT],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
    end = time.time() + 10
    while time.time() < end:
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{MOCK_PORT}/v1/models", timeout=1).read()
            return proc
        except Exception:
            time.sleep(0.2)
    proc.kill()
    raise SystemExit("mock server did not start")


def main():
    mock = start_mock()
    ws = tempfile.mkdtemp()
    open(os.path.join(ws, "demo.txt"), "w").write("hello world\nsecond line\n")
    # Project config: turn the web UI on (which is what enables tracing) and pin
    # its port so this probe can talk to it. `mode` is pinned too, so whatever
    # the user's config says the probe still exercises a real edit.
    open(os.path.join(ws, "koda.toml"), "w").write(
        f"web_ui = true\nweb_ui_port = {WEB_PORT}\nsessions = true\nmemory = true\n"
        f"mode = \"execute\"\n"
    )
    # Isolate the user config so the probe never writes the real one — but carry
    # a copy across, because a real backend needs its api_key.
    cfg_home = tempfile.mkdtemp()
    os.makedirs(os.path.join(cfg_home, "koda"), exist_ok=True)
    if os.path.exists(USER_CONFIG):
        with open(USER_CONFIG) as src, open(os.path.join(cfg_home, "koda", "config.toml"), "w") as dst:
            dst.write(src.read())
    os.environ["XDG_CONFIG_HOME"] = cfg_home

    t = pc.Tui(ws)
    t.read(3.0, until="ready")
    check("web API is reachable", wait_for_web(), "koda did not serve the web UI")

    # ---- 1. a real turn ----
    # A cloud-routed model is slower and phrases its reply however it likes, so
    # wait on the observable fact (the file changed), not on wording.
    budget = 90.0 if BACKEND == "config" else 15.0
    demo = os.path.join(ws, "demo.txt")
    t.send("replace hello with goodbye in demo.txt\r")
    deadline = time.time() + budget
    while time.time() < deadline:
        t.read(1.0)
        if "goodbye" in open(demo).read():
            break
    # Let the turn close (final reply after the tool calls) before reading the trace.
    t.read(6.0 if BACKEND == "config" else 2.0, until="ready")
    check("the turn ran (file edited on disk)", "goodbye world" in open(demo).read(),
          open(demo).read()[:80])

    # Give the turn a moment to close in the trace. A real model keeps going
    # after the write (verifying, summarising), so wait for the turn itself to
    # finish rather than assuming the file change was the last thing it did.
    settle = time.time() + (120.0 if BACKEND == "config" else 15.0)
    trace = get("/api/trace")
    while time.time() < settle:
        trace = get("/api/trace")
        turns = trace.get("turns", [])
        if turns and not turns[0]["running"]:
            break
        t.read(1.0)
    turns = trace.get("turns", [])
    check("tracing is enabled with the web UI", trace.get("enabled") is True)
    check("the turn appears in the trace", len(turns) >= 1, json.dumps(trace)[:300])
    if not turns:
        print("== summary ==\n  aborting: nothing traced")
        t.close(); sys.exit(1)

    top = turns[0]
    check("the turn records the user's input",
          "replace hello with goodbye" in top["input"], top["input"])
    check("the turn records at least one model call", top["model_calls"] >= 1, str(top))
    check("the turn records its tool calls", top["tool_calls"] >= 1, str(top))
    check("the turn finished cleanly", top["status"] in ("ok", "cancelled"), top["status"])
    check("the turn reports a context size", top["tokens"] > 0, str(top["tokens"]))

    full = get(f"/api/trace/{top['id']}")
    steps = full["steps"]
    kinds = [s["kind"] for s in steps]
    check("steps are ordered model-first", kinds and kinds[0] == "model", str(kinds))
    check("no step is left running", all(not s["running"] for s in steps), str(kinds))

    models = [s for s in steps if s["kind"] == "model"]
    tools = [s for s in steps if s["kind"] == "tool"]
    first = models[0]["model"]
    check("the exact request body is captured",
          '"messages"' in first["request"] and pc.MODEL in first["request"],
          first["request"][:200])
    check("the request includes the system prompt",
          '"role": "system"' in first["request"], first["request"][:200])
    check("the raw SSE response is captured", "data:" in first["response"],
          first["response"][:200])
    check("token estimates are recorded", first["prompt_tokens"] > 0)
    check("the tools the model asked for are recorded",
          len(first["tool_calls"]) >= 1, str(first["tool_calls"]))

    names = [s["tool"]["name"] for s in tools if s["tool"]]
    check("tool calls are traced by name", len(names) >= 1, str(names))
    # Which write tool a model picks is its business; that a write was traced
    # with its arguments and diff is what matters.
    writes = [s for s in tools if s["tool"] and s["tool"]["name"] in ("edit_file", "write_file")]
    if writes:
        tool = writes[0]["tool"]
        check("tool arguments are captured", "demo.txt" in tool["args"], tool["args"][:120])
        check("the tool outcome is captured", tool["ok"] is True and bool(tool["summary"]),
              str(tool)[:160])
        check("approval is recorded", tool["approval"] in ("auto", "approved"), str(tool["approval"]))
        check("a write records its diff", bool(tool["diff"]), str(tool["diff"])[:120])
    else:
        check("a write tool step is present", False, str(names))

    # ---- 2. live config control ----
    cfg = get("/api/config")
    check("the config API hides the API key", "api_key" not in cfg, str(cfg.keys()))
    target = "plan" if cfg["mode"] != "plan" else "execute"
    res = post("/api/config", {"mode": target})
    check("a valid config POST is accepted", res.get("ok") is True, str(res))
    # The TUI drains the control queue on its own tick; the status line shows the mode.
    t.read(6.0, until=target.upper())
    check("the running TUI adopted the new mode", t.saw(target.upper()), t.vt.text()[-200:])
    bad = post("/api/config", {"mode": "turbo"})
    check("an invalid config POST is rejected", bad.get("ok") is False, str(bad))

    # ---- 3. live memory control ----
    note = f"probe note {int(time.time())}"
    res = post("/api/memory", {"remember": note})
    check("a memory POST is accepted", res.get("ok") is True, str(res))
    t.read(4.0, until="remembered")
    mem_file = os.path.join(ws, ".koda", "memory.md")
    on_disk = open(mem_file).read() if os.path.exists(mem_file) else ""
    check("the note reached the running agent's memory", note in on_disk, on_disk[:200])
    check("the memory API reads it back",
          any(note in n for n in get("/api/memory")["notes"]))

    t.close()
    subprocess.run(["rm", "-rf", ws, cfg_home])
    if mock:
        mock.kill()
    print(f"== summary ==\n  {passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
