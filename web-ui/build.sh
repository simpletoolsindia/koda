#!/usr/bin/env bash
# build.sh — assemble the self-contained web-ui/dist/index.html from src/
# koda serves web-ui/dist/index.html directly; no npm build required.
# Order matters: shared helpers (CopyButton, fmtMs) are defined in the files
# concatenated before the components that use them.
set -euo pipefail
cd "$(dirname "$0")"

cat \
  src/_head.html \
  src/components/LiveLogs.jsx \
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
  src/_tail.html \
  > dist/index.html

echo "Built dist/index.html ($(wc -c < dist/index.html | tr -d ' ') bytes)"
