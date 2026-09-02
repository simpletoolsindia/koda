#!/usr/bin/env python3
"""Real-user check of codegraph and self-learning, end to end.

Drives the actual binary in a PTY against a real backend (the machine's
OmniRoute by default), typing the way a person would, and then uses the web
control center's API as an independent witness of what really happened:

  codegraph
    1. ask where a symbol lives and who uses it
    2. the trace must show a `codegraph` tool step (not a grep-and-read hunt)
    3. the answer must name the right file

  self-learning
    4. koda writes a file
    5. the *user* edits it afterwards (that is the correction signal)
    6. koda reads it again, notices the divergence, and mines a candidate rule
    7. /learn lists the candidate; accepting it puts it in rules.md
    8. the accepted rule then appears in the next request's system prompt —
       proof the loop closes, not just that a file was written

Usage:
  python3 tests/probe_learn_graph.py              # real backend (OmniRoute)
  BACKEND=mock PORT=8123 python3 ...              # hermetic, needs mock_server
"""
import importlib.util, json, os, shutil, subprocess, sys, tempfile, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
os.chdir(ROOT)

spec = importlib.util.spec_from_file_location("probe_compact", os.path.join(HERE, "probe_compact.py"))
pc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pc)

BACKEND = os.environ.get("BACKEND", "config")
WEB_PORT = int(os.environ.get("WEB_PORT", "8801"))
USER_CONFIG = os.path.join(os.path.expanduser("~"), ".config", "koda", "config.toml")
# A cloud-routed model needs room; these are budgets, not sleeps.
TURN_BUDGET = float(os.environ.get("TURN_BUDGET", "150" if BACKEND == "config" else "40"))

passed = failed = 0


def check(name, cond, extra=""):
    global passed, failed
    if cond:
        print(f"  ok   {name}")
        passed += 1
    else:
        print(f"  FAIL {name}  {extra}")
        failed += 1


def cfg_value(key, default=""):
    if not os.path.exists(USER_CONFIG):
        return default
    for line in open(USER_CONFIG):
        k, _, v = line.strip().partition("=")
        if k.strip() == key:
            return v.strip().strip('"')
    return default


if BACKEND == "config":
    pc.URL = cfg_value("base_url", "http://localhost:20128/v1")
    pc.MODEL = cfg_value("model", "auto")
else:
    port = os.environ.get("PORT", "8123")
    pc.URL = f"http://127.0.0.1:{port}/v1"
    pc.MODEL = "mock-coder"


def api(path):
    with urllib.request.urlopen(f"http://127.0.0.1:{WEB_PORT}{path}", timeout=8) as r:
        return json.loads(r.read().decode())


def wait_for_web(timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        try:
            api("/api/trace")
            return True
        except Exception:
            time.sleep(0.25)
    return False


def turn(t, text, budget=None, settle="ready"):
    """Type a message and wait for the turn to actually finish, the way a person
    waits for the prompt to come back."""
    budget = budget or TURN_BUDGET
    before = len(api("/api/trace")["turns"])
    t.send(text + "\r")
    end = time.time() + budget
    while time.time() < end:
        t.read(1.0)
        turns = api("/api/trace")["turns"]
        if len(turns) > before and not turns[0]["running"]:
            return turns[0]
    turns = api("/api/trace")["turns"]
    return turns[0] if turns else None


def full(turn_id):
    return api(f"/api/trace/{turn_id}")


def tool_steps(detail, name=None):
    out = []
    for s in detail["steps"]:
        if s["kind"] != "tool" or not s.get("tool"):
            continue
        if name is None or s["tool"]["name"] == name:
            out.append(s["tool"])
    return out


def main():
    ws = tempfile.mkdtemp()
    src = os.path.join(ws, "src")
    os.makedirs(src)
    # A small, unambiguous project: one symbol defined once and used in two
    # other files, so "where is it and who uses it" has a checkable answer.
    open(os.path.join(src, "billing.py"), "w").write(
        "def compute_invoice_total(items):\n"
        "    return sum(i['price'] * i['qty'] for i in items)\n"
    )
    open(os.path.join(src, "report.py"), "w").write(
        "from billing import compute_invoice_total\n\n"
        "def monthly_report(items):\n"
        "    return f\"total: {compute_invoice_total(items)}\"\n"
    )
    open(os.path.join(src, "checkout.py"), "w").write(
        "from billing import compute_invoice_total\n\n"
        "def checkout(cart):\n"
        "    return compute_invoice_total(cart)\n"
    )
    open(os.path.join(ws, "koda.toml"), "w").write(
        f"web_ui = true\nweb_ui_port = {WEB_PORT}\n"
        'mode = "execute"\ncodegraph = true\nlearning = true\nmemory = true\n'
    )
    cfg_home = tempfile.mkdtemp()
    os.makedirs(os.path.join(cfg_home, "koda"), exist_ok=True)
    if os.path.exists(USER_CONFIG):
        shutil.copy(USER_CONFIG, os.path.join(cfg_home, "koda", "config.toml"))
    os.environ["XDG_CONFIG_HOME"] = cfg_home

    t = pc.Tui(ws)
    t.read(4.0, until="ready")
    if not wait_for_web():
        print("FAIL: koda did not serve the web UI")
        t.close(); sys.exit(1)
    print(f"  ..   backend {pc.URL} · model {pc.MODEL}")

    # ---------------------------------------------------------------- codegraph
    print("== codegraph ==")
    tr = turn(t, "where is compute_invoice_total defined and which files use it?")
    check("the question produced a turn", tr is not None)
    if tr is None:
        t.close(); sys.exit(1)
    detail = full(tr["id"])
    graph_calls = tool_steps(detail, "codegraph")
    names = [s["tool"]["name"] for s in detail["steps"] if s["kind"] == "tool" and s.get("tool")]
    check("codegraph was called for a symbol question", len(graph_calls) >= 1, f"tools used: {names}")
    if graph_calls:
        check("codegraph was asked about the right symbol",
              "compute_invoice_total" in graph_calls[0]["args"], graph_calls[0]["args"][:160])
        check("codegraph returned the definition site",
              "billing.py" in graph_calls[0]["detail"], graph_calls[0]["detail"][:200])
        check("codegraph returned the cross-file users",
              "report.py" in graph_calls[0]["detail"] and "checkout.py" in graph_calls[0]["detail"],
              graph_calls[0]["detail"][:300])
    check("codegraph came first, before any grep/read",
          not names or names[0] == "codegraph", f"tools in order: {names}")
    reply = (detail.get("reply") or "") + t.vt.text()
    check("the answer names the defining file", "billing.py" in reply, reply[-300:])
    check("the answer names the callers",
          "report.py" in reply and "checkout.py" in reply, reply[-400:])

    # A structural question should also go through the graph, not a file crawl.
    tr2 = turn(t, "give me a structural overview of this project")
    if tr2:
        d2 = full(tr2["id"])
        names2 = [s["tool"]["name"] for s in d2["steps"] if s["kind"] == "tool" and s.get("tool")]
        check("a structure question uses codegraph too", "codegraph" in names2, f"tools used: {names2}")

    # ------------------------------------------------------------ self-learning
    print("== self-learning ==")
    learn_before = api("/api/learning")
    check("learning starts with no accepted rules",
          len(learn_before["accepted"]) == 0, str(learn_before["accepted"])[:200])

    tr3 = turn(t, "create src/discount.py with a function that applies a percentage discount to a price")
    disc = os.path.join(src, "discount.py")
    check("koda wrote the file", os.path.exists(disc),
          f"files: {os.listdir(src)}")
    if tr3 and os.path.exists(disc):
        wrote = tool_steps(full(tr3["id"]), "write_file") or tool_steps(full(tr3["id"]), "edit_file")
        check("the write is traced with its diff", bool(wrote) and bool(wrote[0]["diff"]),
              str(wrote)[:200] if wrote else "no write step")

    # The user corrects koda's output by hand. This is the signal koda is meant
    # to notice — not a prompt, an actual divergence on disk.
    koda_text = open(disc).read() if os.path.exists(disc) else ""
    open(disc, "w").write(
        "# Reviewed by the user.\n"
        "def apply_discount(price_cents: int, percent_off: int) -> int:\n"
        "    \"\"\"Prices are integer cents in this project — never floats.\"\"\"\n"
        "    return price_cents - (price_cents * percent_off) // 100\n"
    )
    check("the user's edit differs from what koda wrote",
          open(disc).read() != koda_text)

    # koda reads it again: that read is where it compares disk against what it
    # last wrote and records the correction.
    turn(t, "read src/discount.py and tell me what it does now")

    # Rule induction runs at turn end; /learn shows what it found.
    t.send("/learn\r")
    t.read(6.0)
    learn_after = api("/api/learning")
    cands = learn_after["candidates"]
    check("a single user correction produced a candidate rule",
          len(cands) >= 1, f"candidates: {json.dumps(cands)[:300]}")
    # "No new candidate rules" also contains the word "candidate", so assert on
    # the affirmative form only.
    check("/learn offers the candidates in the TUI",
          t.saw("/learn accept") and not t.saw("No new candidate"), t.vt.text()[-400:])
    rules_file = os.path.join(ws, ".koda", "learning", "rules.md")
    check("candidates are persisted to rules.md", os.path.exists(rules_file),
          f".koda: {os.listdir(os.path.join(ws, '.koda')) if os.path.exists(os.path.join(ws, '.koda')) else 'missing'}")

    if cands:
        t.send("/learn accept 1\r")
        t.read(8.0)
        after_accept = api("/api/learning")
        check("accepting a candidate promotes it to an accepted rule",
              len(after_accept["accepted"]) >= 1, str(after_accept)[:300])
        check("the accepted rule is written to rules.md",
              os.path.exists(rules_file) and "Accepted" in open(rules_file).read(),
              open(rules_file).read()[:300] if os.path.exists(rules_file) else "")
        accepted_text = after_accept["accepted"][0]["text"] if after_accept["accepted"] else ""

        # The real test: does the accepted rule actually reach the model? The
        # trace holds the exact request body of the next turn.
        tr5 = turn(t, "what is 2 + 2?")
        if tr5:
            d5 = full(tr5["id"])
            models = [s for s in d5["steps"] if s["kind"] == "model"]
            req = models[0]["model"]["request"] if models else ""
            # Compare on a distinctive fragment: rule text is one line of prose.
            frag = accepted_text.strip().split(".")[0][:40]
            check("the accepted rule is injected into the system prompt",
                  bool(frag) and frag in req, f"looked for {frag!r}")

    # Memory should also have recorded which files the work happened in.
    mem = api("/api/memory")
    check("memory recorded the files koda worked in",
          any("discount.py" in f["path"] for f in mem["hot_files"]) or len(mem["hot_files"]) >= 1,
          str(mem["hot_files"])[:200])

    t.close()
    if os.environ.get("KEEP"):
        print(f"  ..   workspace kept: {ws}")
    else:
        subprocess.run(["rm", "-rf", ws, cfg_home])
    print(f"== summary ==\n  {passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
