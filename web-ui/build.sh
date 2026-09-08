#!/usr/bin/env bash
# build.sh — assemble the self-contained web-ui/dist/index.html from src/
#
# koda serves dist/index.html directly. The file is genuinely self-contained:
# Tailwind, React and the app are all inlined, and the page fetches nothing at
# runtime. It used to load four scripts from two CDNs — including an
# *unversioned* @babel/standalone that transpiled the JSX in the browser — which
# meant the UI did not work offline (koda's only environment) and executed
# whatever unpkg served that day inside a page that can read your source tree.
#
# The JSX is transpiled here instead, once, with esbuild. That is what lets the
# 2.9 MB in-browser transpiler go: vendored assets are ~540 KB, not 3.4 MB.
#
# Order matters below: shared helpers (CopyButton, fmtMs) are defined in the
# files concatenated before the components that use them.
set -euo pipefail
cd "$(dirname "$0")"

need() { [ -f "vendor/$1" ] || { echo "missing vendor/$1 — see vendor/README.md" >&2; exit 1; }; }
need react.js; need react-dom.js; need tailwind.js

SRC="$(mktemp -t koda-ui-src).jsx"
APP="$(mktemp -t koda-ui-app).js"
trap 'rm -f "$SRC" "$APP"' EXIT

cat \
  src/components/LiveLogs.jsx \
  src/components/SessionStatus.jsx \
  src/components/LlmDebug.jsx \
  src/components/CodeGraph.jsx \
  src/components/AgentsSkills.jsx \
  src/components/SystemPrompt.jsx \
  src/components/TraceRail.jsx \
  src/components/TraceWaterfall.jsx \
  src/components/TraceInspector.jsx \
  src/components/ControlRail.jsx \
  src/components/CommandPalette.jsx \
  src/App.jsx \
  > "$SRC"

# The mount lives in _tail.html and is JSX too, so it is transpiled with the
# rest rather than left for a transpiler that is no longer there.
sed -n '/---- Mount ----/,/^  <\/script>/p' src/_tail.html | sed '$d' >> "$SRC"

npx --yes esbuild@0.25.0 "$SRC" \
  --loader:.jsx=jsx --jsx-factory=React.createElement --jsx-fragment=React.Fragment \
  --minify --outfile="$APP" --log-level=warning

python3 - "$APP" <<'PY'
import pathlib, sys
app = pathlib.Path(sys.argv[1]).read_text()
head = pathlib.Path('src/_head.html').read_text()
for token, path in (("/*__TAILWIND__*/", "vendor/tailwind.js"),
                    ("/*__REACT__*/",    "vendor/react.js"),
                    ("/*__REACTDOM__*/", "vendor/react-dom.js")):
    assert token in head, f"{token} missing from src/_head.html"
    head = head.replace(token, pathlib.Path(path).read_text(), 1)
pathlib.Path('dist/index.html').write_text(head + app + "\n  </script>\n</body>\n</html>\n")
PY

echo "Built dist/index.html ($(wc -c < dist/index.html | tr -d ' ') bytes, no runtime fetches)"
