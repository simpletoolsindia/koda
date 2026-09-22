# Changelog

## 1.0.0 — 2026-09-22

The first stable release. Everything since 0.1.0: 158 commits.

### Install

- **Prebuilt binaries** for Linux (x86_64, ARM64), macOS (Apple Silicon,
  Intel) and Windows (x64). The installers download the one for your
  platform, check its SHA-256 and run it once before installing, so an
  install takes seconds and needs no Rust.
  - macOS / Linux: `curl -fsSL https://raw.githubusercontent.com/simpletoolsindia/koda/master/install.sh | bash`
  - Windows: `irm https://raw.githubusercontent.com/simpletoolsindia/koda/master/install.ps1 | iex`
- The Linux binaries are built on Ubuntu 22.04 (glibc 2.35), so they run on
  Ubuntu 22.04+, Debian 12, Fedora and Arch. CI starts each build on Debian 12
  and Ubuntu 22.04 before release.
- Where no binary runs, the installer builds from source, keeping the clone
  in `~/.cache/koda` so updates rebuild only what changed.
  `KODA_FROM_SOURCE=1` always builds; `KODA_VERSION=x.y.z` pins a release.
- Installers are tested on every change: Ubuntu, macOS, Windows, and Debian 12,
  Ubuntu 22.04, Fedora 41 and Arch in containers.

### The terminal UI

- Opening titles: code rain, the `>_` mark decoding stroke by stroke, and a
  boot check of your model, workspace and mode. Any key skips it; `intro =
  false` turns it off; `/intro` replays it.
- A new welcome card, tool cards with animated emoji that act out the work,
  a header bar, a working wave, a typing cursor, toasts and transitions.
- Files show on screen while koda writes them.
- Type-to-filter dropdowns for model, provider, theme and mode; completion
  for slash-command arguments.
- A question dialog for everything the agent asks, including questions it
  writes as plain text.
- 20 themes, 9 of them new: kanagawa, everforest, one-dark, github-dark,
  ayu-mirage, night-owl, and three light ones (catppuccin-latte,
  rose-pine-dawn, everforest-light). Every theme keeps body text at WCAG AA
  contrast.
- Command output with progress bars or colour codes, and commands that ask
  for a password, can no longer scramble the screen.

### The agent

- **Verify**: detects the project (Cargo, Go, npm/pnpm/yarn/bun, Python,
  Maven, Gradle, or its own `koda.toml` check) and runs its real checks. A
  turn that changed code does not end until something has checked it.
- **Memory**: typed memories (facts, decisions with their reasons,
  preferences, procedures) in a local SQLite store, recalled per request by
  words and meaning, with outdated ones replaced.
- **Code understanding**: Tree-sitter parsing for Rust, Python, JavaScript,
  TypeScript and Go; semantic references from language servers; task-ranked
  code maps that put the code a request is about first.
- **Git safety**: every git command waits for your yes, even in full-auto
  (headless runs excepted).
- Lazy tool loading: heavy tool groups load only when a session needs them,
  keeping the prompt small for local models. `/fastmode` for a lean prompt.
- A real debugger (DAP), MCP servers, a browser engine that ships with koda,
  image input with OCR for models that cannot see.

### Windows

- `verify`, program lookup (`cargo.exe`, PATHEXT), git confirmation and
  file paths work on Windows. Windows Terminal gets Unicode and emoji.
- The full test suite runs on Windows, macOS and Linux in CI, with JUnit
  reports published on every commit.
