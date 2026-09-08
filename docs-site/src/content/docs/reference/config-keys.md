---
title: Configuration keys
description: Every key koda reads, its default, and what it changes.
---

Written to `~/.config/koda/config.toml`, overridden by a project `koda.toml`, then by
environment variables, then by CLI flags. `koda config` prints the effective result;
`koda config --init` writes a commented starter file.

## Connection

| Key | Default | What it does |
| --- | --- | --- |
| `base_url` | `http://localhost:11434/v1` | OpenAI-compatible base URL. |
| `api_key` | `local` | API key, if the server needs one. |
| `model` | `""` | Empty auto-picks the first model the server reports. |
| `active_provider` | `""` | Which saved `[[provider]]` is in use. |
| `insecure_tls` | `false` | Accept a certificate that does not verify. See the [warning](/koda/providers/#servers-behind-a-private-ca). |

## Sampling and budget

| Key | Default | What it does |
| --- | --- | --- |
| `temperature` | `0.2` | Sampling temperature. Low is better for code. |
| `top_p` | `0.95` | Nucleus sampling. |
| `max_tokens` | `0` | `0` lets the server decide. |
| `context_tokens` | `110000` | History budget. Requests are curated to fit. |
| `auto_compact_at` | `0.85` | Compact once context passes this fraction. `0` disables. |
| `reasoning_effort` | `off` | `off`, `low`, `medium`, `high`. |

## Behaviour

| Key | Default | What it does |
| --- | --- | --- |
| `mode` | `execute` | Starting mode: `plan`, `execute`, `vibe`. |
| `tool_protocol` | `auto` | `auto`, `native`, `text`. |
| `max_steps` | `24` | Model↔tool round trips per turn before the step check. |
| `step_check` | `true` | At `max_steps`, ask the model whether work remains. |
| `max_steps_hard` | `96` | Absolute ceiling once `step_check` extends the budget. |
| `auto_approve` | `false` | Equivalent to `auto_tier = "full"`. |
| `auto_tier` | `ask` | `ask`, `write`, `full`. |
| `sandbox` | `true` | Confine file tools to the workspace root, symlinks included. |
| `confirm_destructive` | `true` | Ask before irreversible commands even under auto-approve. |

## Subagents

| Key | Default | What it does |
| --- | --- | --- |
| `subagents` | `true` | Allow delegation of read-only investigations. |
| `subagent_max_steps` | `12` | Step budget for one subagent run. |
| `subagent_review_rounds` | `1` | Vibe-mode re-prompts of a report that does not hold up. |
| `max_subagent_depth` | `1` | Nesting depth. `1` means subagents cannot delegate. |

## Capabilities

| Key | Default | What it does |
| --- | --- | --- |
| `codegraph` | `true` | Scan the project into a symbol graph on open. |
| `codegraph_refresh_ms` | `15000` | Re-index files changed outside koda. `0` disables. |
| `sessions` | `true` | Record each session to `<project>/.koda/sessions`. |
| `memory` | `true` | Carry notes and command outcomes in `.koda/memory.md`. |
| `learning` | `false` | Distil inspectable rules from how you work. |
| `learning_daily` | `true` | Consolidate learned rules once a day. |
| `learning_promote_days` | `3` | Distinct days a candidate must recur before promotion. |
| `learning_retire_days` | `30` | Days without reinforcement before an auto rule retires. |
| `web_search` | `false` | Allow the `web_search` tool. |
| `search_backend` | `duckduckgo` | `duckduckgo` or `searxng`. |
| `searx_url` | `""` | A SearXNG instance with JSON output enabled. |
| `search_results` | `6` | Results per search. |
| `web_fetch` | `false` | Allow GETting a URL and reading it as text. |
| `browser` | `false` | Allow the `browse` tool. |
| `browser_channel` | `chrome` | `chrome`, `msedge`, a path, or `""` for the bundled engine. |
| `browser_headless` | `true` | `false` opens a visible window. |
| `browser_path` | `""` | Point at your own copy of the engine. |
| `watch` | `false` | Act on `AI!` / `AI?` comment triggers. |
| `watch_interval_ms` | `1500` | How often the workspace is rescanned. |
| `ocr` | `false` | Extract text from an image for a text-only model. |
| `ocr_model` | `""` | A vision model to route OCR through, before tesseract. |

## The web control center

| Key | Default | What it does |
| --- | --- | --- |
| `web_ui` | `false` | Serve the trace and control page on `127.0.0.1`. |
| `web_ui_port` | `7717` | Port for it. |
| `ui_detail` | `medium` | Log detail: `simple`, `medium`, `high`. |

## Appearance

| Key | Default | What it does |
| --- | --- | --- |
| `theme` | `auto` | Palette name; `auto` resolves to `neon`. |
| `icons` | `auto` | `auto`, `unicode`, `ascii`. |
| `motion` | `true` | Animate spinners, gauges and reveal. |
| `reveal` | `true` | Reveal streaming replies progressively. Needs `motion`. |
| `mouse_capture` | `true` | On, the wheel scrolls and dragging selects. |
| `sync_output` | `true` | Wrap each frame in DEC 2026 synchronized-update markers. |

## Limits

| Key | Default | What it does |
| --- | --- | --- |
| `shell` | `/bin/sh` | Shell used for commands. |
| `command_timeout_ms` | `120000` | Command timeout. |
| `max_file_bytes` | `262144` | Max bytes read from a file; also caps attached images. |
| `max_tool_output_bytes` | `24576` | Max bytes of tool output reaching the model. |
| `max_retries` | `3` | Attempts per request. `1` means no retry. |

## Logging

| Key | Default | What it does |
| --- | --- | --- |
| `log_level` | `info` | `debug`, `info`, `warn`, `error`. |
| `log_to_file` | `true` | Mirror the event log to `~/.local/state/koda/koda.log`. |
| `log_detail` | `false` | Show debug-level telemetry in `/logs`. |
| `debug` | `false` | Dump raw requests and responses to disk. |

## Prompt

| Key | Default | What it does |
| --- | --- | --- |
| `system_prompt` | `""` | Replace the built-in prompt. Empty uses the built-in. |
| `instructions` | `""` | Appended verbatim to the system prompt. |
| `[tool_prompts]` | — | Per-tool prompt overrides. |

## Tables

| Table | What it declares |
| --- | --- |
| `[[provider]]` | A named endpoint. See [LLM providers](/koda/providers/#several-endpoints-at-once). |
| `[[tools]]` | A [custom tool](/koda/customtools/). |
