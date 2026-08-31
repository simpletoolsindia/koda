#!/usr/bin/env bash
# build.sh — assemble the self-contained web-ui/dist/index.html from src/
# koda serves web-ui/dist/index.html directly; no npm build required.
set -euo pipefail
cd "$(dirname "$0")"

cat \
  src/_head.html \
  src/components/LiveLogs.jsx \
  src/components/LlmDebug.jsx \
  src/components/CodeGraph.jsx \
  src/components/AgentsSkills.jsx \
  src/components/SystemPrompt.jsx \
  src/App.jsx \
  src/_tail.html \
  > dist/index.html

echo "Built dist/index.html ($(wc -c < dist/index.html | tr -d ' ') bytes)"
