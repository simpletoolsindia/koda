#!/usr/bin/env python3
"""Record the koda demos listed in demos/manifest.json.

    record.py                 # every demo
    record.py hero themes     # just these
    record.py --list          # what is in the manifest
    record.py --check         # re-verify the casts already on disk

Each demo gets a throwaway git workspace, its own config, and a scripted model
(`tests/mock_server.py`) speaking real OpenAI-shaped SSE. Everything else is
koda: its parser, tool dispatch, approval path, diff rendering and status bar
all run for real, and the tools really touch the disk.

A recording that ran but painted the wrong screen looks exactly like a good one
on disk, so nothing is published until its `expect` strings have been found in
the frames — that check needs `pyte` (pip install pyte).
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO = os.path.dirname(ROOT)
MANIFEST = os.path.join(ROOT, "manifest.json")
OUT = os.path.join(REPO, "docs-site", "public", "demos")
CASTS = os.path.join(ROOT, "casts")
PORT = 8911
URL = f"http://127.0.0.1:{PORT}/v1"


def load():
    with open(MANIFEST) as fh:
        return json.load(fh)["demos"]


def binary():
    for path in ("target/release/koda", "target/debug/koda"):
        full = os.path.join(REPO, path)
        if os.access(full, os.X_OK):
            return full
    sys.exit("no koda binary — cargo build --release first")


def workspace(demo):
    """A fresh git repo with this demo's fixtures, thrown away afterwards.

    The directory is named, not a mktemp string, because koda puts the
    workspace name in its status bar and `koda-demo-approve-8s53nwb2` is not
    what anyone's project is called.
    """
    parent = tempfile.mkdtemp(prefix="koda-demo-")
    ws = os.path.join(parent, demo.get("project", "sandbox"))
    os.makedirs(ws)
    # `koda` on PATH, for the demos that drive a shell instead of the TUI.
    bindir = os.path.join(parent, "bin")
    os.makedirs(bindir)
    os.symlink(binary(), os.path.join(bindir, "koda"))
    fixtures = os.path.join(ROOT, "fixtures")
    for name in demo.get("fixtures", []):
        shutil.copy(os.path.join(fixtures, name), ws)
    for name, body in demo.get("files", {}).items():
        path = os.path.join(ws, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as fh:
            fh.write(body)
    # An isolated config, so whoever records this does not bake their own
    # provider, theme or autonomy tier into the published cast.
    cfgdir = os.path.join(ws, ".config", "koda")
    os.makedirs(cfgdir, exist_ok=True)
    if demo.get("config"):
        with open(os.path.join(cfgdir, "config.toml"), "w") as fh:
            # {url} is the scripted model's endpoint, so a demo can point koda
            # at it through the config and keep the command line on screen the
            # one a reader would actually type.
            fh.write(demo["config"].replace("{url}", URL))
    env = dict(os.environ, GIT_AUTHOR_NAME="koda demo",
               GIT_AUTHOR_EMAIL="demo@example.com",
               GIT_COMMITTER_NAME="koda demo",
               GIT_COMMITTER_EMAIL="demo@example.com")
    for cmd in ("git init -q .", "git add -A", "git commit -qm init"):
        subprocess.run(cmd, shell=True, cwd=ws, env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return ws


def mock(mode, extra_env=None):
    proc = subprocess.Popen(
        [sys.executable, os.path.join(REPO, "tests", "mock_server.py"), str(PORT)],
        env=dict(os.environ, MOCK_MODE=mode, **(extra_env or {})),
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    for _ in range(40):
        try:
            urllib.request.urlopen(f"{URL}/models", timeout=0.5).read()
            return proc
        except Exception:
            time.sleep(0.25)
    proc.terminate()
    sys.exit(f"mock server did not come up in mode {mode}")


def record(demo):
    cols, rows = demo.get("cols", 100), demo.get("rows", 30)
    cast = os.path.join(CASTS, demo["id"] + ".cast")
    ws = workspace(demo)
    spec = dict(demo, workspace=ws, url=URL, cols=cols, rows=rows)
    spec_path = os.path.join(ws, "_spec.json")
    with open(spec_path, "w") as fh:
        json.dump(spec, fh)

    server = mock(demo.get("mode", "showcase"), demo.get("mock_env"))
    try:
        # Off-camera setup: earlier sessions for the /resume picker, an index
        # to warm, a file to have already been changed. Whatever the demo needs
        # to be *about* something rather than about an empty directory.
        for cmd in demo.get("pre_sh", []):
            subprocess.run(
                cmd, shell=True, cwd=ws,
                env=dict(os.environ,
                         XDG_CONFIG_HOME=os.path.join(ws, ".config"),
                         HOME=os.path.dirname(ws),
                         XDG_DATA_HOME=os.path.join(os.path.dirname(ws), "data"),
                         PATH=os.path.join(os.path.dirname(ws), "bin")
                              + os.pathsep + os.environ["PATH"],
                         KODA_URL=URL),
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        os.makedirs(CASTS, exist_ok=True)
        drive = os.path.join(ROOT, "scripts", "drive.py")
        # asciicast-v2, not the v3 default: agg reads v3 without erroring but
        # collapses the session into two frames, and asciinema-player wants v2.
        subprocess.run(
            ["asciinema", "rec", cast, "--output-format", "asciicast-v2",
             "--overwrite", "--window-size", f"{cols}x{rows}",
             "--command", f"{sys.executable} {drive} {spec_path}"],
            cwd=REPO,
            env=dict(os.environ, KODA_BIN=binary()),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
    finally:
        server.terminate()
        shutil.rmtree(os.path.dirname(ws), ignore_errors=True)
    if not os.path.exists(cast) or os.path.getsize(cast) == 0:
        return None
    return cast


def verify(demo, cast):
    """Every `expect` string must show up in some frame of the recording."""
    sys.path.insert(0, os.path.join(ROOT, "scripts"))
    from screen import frames  # noqa: E402  (needs pyte)

    wanted = list(demo.get("expect", []))
    if not wanted:
        return []
    missing = {w.lower() for w in wanted}
    for text in frames(cast):
        low = text.lower()
        missing = {w for w in missing if w not in low}
        if not missing:
            return []
    return [w for w in wanted if w.lower() in missing]


def publish(cast):
    os.makedirs(OUT, exist_ok=True)
    shutil.copy(cast, os.path.join(OUT, os.path.basename(cast)))


def duration(cast):
    """How long the recording runs, from its last frame."""
    last = 0.0
    with open(cast) as fh:
        fh.readline()
        for line in fh:
            try:
                last = json.loads(line)[0]
            except (ValueError, IndexError):
                continue
    return round(last, 1)


def site_data(demos):
    """What the docs site needs to build the gallery: no recording internals.

    The manifest is the single source of truth for both, so a demo cannot be
    recorded without appearing on the site, or listed on the site without a
    recording behind it.
    """
    out = []
    for demo in demos:
        cast = os.path.join(OUT, demo["id"] + ".cast")
        if not os.path.exists(cast):
            continue
        out.append({
            "id": demo["id"],
            "title": demo["title"],
            "category": demo.get("category", "Capabilities"),
            "page": demo.get("page", ""),
            "blurb": demo.get("blurb", ""),
            "cols": demo.get("cols", 100),
            "rows": demo.get("rows", 30),
            "shell": demo.get("driver") == "shell",
            "duration": duration(cast),
        })
    path = os.path.join(REPO, "docs-site", "src", "data", "demos.json")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        json.dump(out, fh, indent=2)
        fh.write("\n")
    return path


def main():
    args = [a for a in sys.argv[1:]]
    demos = load()
    if "--list" in args:
        for d in demos:
            print(f"{d['id']:<14} {d.get('category',''):<14} {d['title']}")
        return 0
    check_only = "--check" in args
    wanted = [a for a in args if not a.startswith("--")]
    if wanted:
        demos = [d for d in demos if d["id"] in wanted]
        missing = set(wanted) - {d["id"] for d in demos}
        if missing:
            sys.exit(f"not in the manifest: {', '.join(sorted(missing))}")

    failures = []
    for demo in demos:
        cast = os.path.join(CASTS, demo["id"] + ".cast")
        if not check_only:
            print(f"recording {demo['id']:<14} ({demo.get('mode','showcase')})…",
                  end="", flush=True)
            cast = record(demo)
            if not cast:
                print(" FAILED: nothing recorded")
                failures.append(demo["id"])
                continue
        elif not os.path.exists(cast):
            print(f"{demo['id']:<14} missing cast")
            failures.append(demo["id"])
            continue
        gaps = verify(demo, cast)
        if gaps:
            print(f" FAILED: never showed {gaps}")
            failures.append(demo["id"])
            continue
        publish(cast)
        size = os.path.getsize(cast) // 1024
        print(f" ok ({size}K)")

    path = site_data(load())
    print(f"wrote {os.path.relpath(path, REPO)}")

    if failures:
        print(f"\n{len(failures)} failed: {', '.join(failures)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
