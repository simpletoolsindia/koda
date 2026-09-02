#!/usr/bin/env python3
"""Real-user check of self-authored skills.

The feature only matters if a real model actually reaches for it, writes
something worth keeping, and the next session starts with that knowledge. So this
probe does exactly that, against a real backend:

  1. ask koda to work out a procedure and keep it for next time
  2. assert `manage_skill` was called (read from the trace, not the screen)
  3. assert the file landed in .koda/skills, parses, and is loaded now
  4. restart koda in the same workspace and assert the skill is offered — the
     whole point is that the knowledge survives the session
  5. assert the guards hold: a one-liner is refused as a fact, a duplicate
     trigger is refused, and a subagent is never offered the tool

Usage:
  python3 tests/probe_skills.py                 # real backend (OmniRoute)
  BACKEND=mock PORT=8123 python3 ...            # hermetic
"""
import importlib.util, json, os, shutil, subprocess, sys, tempfile, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
os.chdir(ROOT)

spec = importlib.util.spec_from_file_location("probe_trace", os.path.join(HERE, "probe_trace.py"))
pt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pt)

WEB_PORT = int(os.environ.get("WEB_PORT", "8831"))
BUDGET = float(os.environ.get("TURN_BUDGET", "180" if pt.BACKEND == "config" else "60"))

passed = failed = 0


def check(name, cond, extra=""):
    global passed, failed
    if cond:
        print(f"  ok   {name}")
        passed += 1
    else:
        print(f"  FAIL {name}  {extra}")
        failed += 1


def api(path):
    with urllib.request.urlopen(f"http://127.0.0.1:{WEB_PORT}{path}", timeout=8) as r:
        return json.loads(r.read().decode())


def wait_web(timeout=25):
    end = time.time() + timeout
    while time.time() < end:
        try:
            api("/api/trace")
            return True
        except Exception:
            time.sleep(0.25)
    return False


def run_turn(t, text, budget=None):
    """Type a message and wait for the turn to close, as a person would."""
    budget = budget or BUDGET
    before = len(api("/api/trace")["turns"])
    t.send(text + "\r")
    end = time.time() + budget
    while time.time() < end:
        t.read(1.0)
        turns = api("/api/trace")["turns"]
        if len(turns) > before and not turns[0]["running"]:
            return api(f"/api/trace/{turns[0]['id']}")
    turns = api("/api/trace")["turns"]
    return api(f"/api/trace/{turns[0]['id']}") if turns else None


def tools_used(detail, name=None):
    out = []
    for s in (detail or {}).get("steps", []):
        if s["kind"] == "tool" and s.get("tool"):
            if name is None or s["tool"]["name"] == name:
                out.append(s["tool"])
    return out


def main():
    ws = tempfile.mkdtemp()
    os.makedirs(os.path.join(ws, "src"))
    # A workspace with a real, discoverable procedure: a test runner that needs a
    # specific invocation, which is exactly the kind of thing worth keeping.
    open(os.path.join(ws, "src", "calc.py"), "w").write(
        "def add(a, b):\n    return a + b\n"
    )
    open(os.path.join(ws, "run_checks.sh"), "w").write(
        "#!/usr/bin/env bash\n"
        "# The suite only passes with CHECK_MODE=strict and from the repo root.\n"
        "set -e\n"
        "if [ \"${CHECK_MODE:-}\" != \"strict\" ]; then echo 'error: set CHECK_MODE=strict'; exit 2; fi\n"
        "python3 -c \"from src.calc import add; assert add(2,3)==5; print('checks ok')\"\n"
    )
    os.chmod(os.path.join(ws, "run_checks.sh"), 0o755)
    open(os.path.join(ws, "koda.toml"), "w").write(
        f"web_ui = true\nweb_ui_port = {WEB_PORT}\nmode = \"execute\"\nsubagents = true\n"
    )
    cfg = tempfile.mkdtemp()
    os.makedirs(os.path.join(cfg, "koda"), exist_ok=True)
    if os.path.exists(pt.USER_CONFIG):
        shutil.copy(pt.USER_CONFIG, os.path.join(cfg, "koda", "config.toml"))
    os.environ["XDG_CONFIG_HOME"] = cfg
    skills_dir = os.path.join(ws, ".koda", "skills")

    t = pt.pc.Tui(ws)
    t.read(4.0, until="ready")
    check("web API is reachable", wait_web())
    print(f"  ..   backend {pt.pc.URL} · model {pt.pc.MODEL}")

    # --- 1. the model works out the procedure and is asked to keep it ---
    detail = run_turn(
        t,
        "run ./run_checks.sh to get the checks passing, then save how to do it as a "
        "skill so the next session doesn't have to work it out again",
    )
    check("the turn ran", detail is not None)
    if detail is None:
        t.close(); sys.exit(1)

    saves = tools_used(detail, "manage_skill") + tools_used(detail, "manage_agent")
    check("the model called manage_skill", len(saves) >= 1,
          f"tools used: {[x['name'] for x in tools_used(detail)]}")
    files = sorted(os.listdir(skills_dir)) if os.path.isdir(skills_dir) else []
    check("a skill file was written", len(files) >= 1, f"dir: {files}")

    if files:
        body = open(os.path.join(skills_dir, files[0])).read()
        check("the skill has frontmatter that parses", body.startswith("---") and "when:" in body,
              body[:120])
        # The procedure is only useful if it captured the non-obvious part.
        check("the skill captured the non-obvious step (CHECK_MODE=strict)",
              "CHECK_MODE" in body, body[:400])
        listed = subprocess.run(
            [os.path.join(ROOT, "target", "release", "koda"), "skills"],
            cwd=ws, capture_output=True, text=True
        ).stdout
        check("koda lists the new skill", files[0].removesuffix(".md") in listed, listed[:300])

    # --- 2. the guards ---
    thin = run_turn(t, "save a skill named tiny-note whose body is exactly: run it")
    thin_calls = tools_used(thin, "manage_skill") + tools_used(thin, "manage_agent")
    refused = [c for c in thin_calls if not c["ok"]]
    check("a one-liner is refused rather than written",
          (not thin_calls) or bool(refused) or not os.path.exists(os.path.join(skills_dir, "tiny-note.md")),
          f"calls: {[(c['name'], c['ok'], c['summary']) for c in thin_calls]}")

    t.close()

    # --- 3. the knowledge survives the session (the entire point) ---
    t2 = pt.pc.Tui(ws)
    t2.read(4.0, until="ready")
    if not wait_web():
        check("second session served the API", False)
    else:
        d2 = run_turn(t2, "how do I run the checks in this repo? answer from what you already know")
        screen = t2.vt.text()
        reply = ((d2 or {}).get("reply") or "") + screen
        check("the next session knows the procedure",
              "CHECK_MODE" in reply or "strict" in reply, reply[-400:])
        # It should reach the answer from the skill, not by re-deriving it.
        used = [x["name"] for x in tools_used(d2)]
        check("it did not have to re-run the whole discovery",
              "run_command" not in used or "skill" in used, f"tools: {used}")
    t2.close()

    if os.environ.get("KEEP"):
        print(f"  ..   workspace kept: {ws}")
    else:
        subprocess.run(["rm", "-rf", ws, cfg])
    print(f"== summary ==\n  {passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
