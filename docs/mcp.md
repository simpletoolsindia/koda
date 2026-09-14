# MCP: borrowing tools from other programs

The Model Context Protocol is how the rest of this space grew an extension
ecosystem. A server is a small program — or an HTTP endpoint — that advertises
tools, resources and prompts, and koda calls them as if they were built in. If
somebody has published an MCP server for Postgres, Sentry, Linear, your
company's internal API, koda can use it without a line of Rust.

## Adding one

Servers are `[[mcp_server]]` tables, in `koda.toml` for a project or in
`~/.config/koda/config.toml` for all of them:

```toml
[[mcp_server]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[mcp_server.env]
GITHUB_TOKEN = "${GH_TOKEN}"
```

`${VAR}` and `$VAR` expand from koda's own environment, so a config with
`token = "${GH_TOKEN}"` is a file you can commit. An unset variable becomes
empty rather than being passed through literally — a missing secret should fail
as a missing secret, not be sent as the string `${GH_TOKEN}`.

A hosted server speaks streamable HTTP instead:

```toml
[[mcp_server]]
name = "docs"
url = "https://mcp.example.com/v1"

[mcp_server.headers]
Authorization = "Bearer ${DOCS_KEY}"
```

Set `command` or `url`, not both.

## What the model sees

Each tool arrives as `mcp__<server>__<tool>` — `mcp__github__create_issue`.
Namespaced because two servers may both call something `search`, and because a
model reading its tool list should be able to tell, without being told, which
answers come from outside the repository. (The double underscore rather than a
slash is not cosmetic: OpenAI-compatible tool names may not contain one.)

The `mcp` tool covers the half of the protocol that is not tools:

| action | |
|---|---|
| `servers` | every server, its state, and the tools it lends |
| `resources` / `read_resource` | documents a server publishes, by URI |
| `prompts` / `get_prompt` | a server's prepared prompt templates |

`/mcp` shows the same report in the TUI, and `koda mcp` from a shell connects to
every server and prints what it found — the first thing to run when a tool you
expected is not there.

## Approval

An MCP server runs somebody else's code, so **its tools ask before running**,
like `write_file` does. Two things narrow that:

- A server that marks a tool `readOnlyHint` is believed. Read-only tools run
  without asking and are available in plan mode.
- `trust = true` on a server treats everything it offers as read-only. Per
  server on purpose: *"my local filesystem server may act without asking"* is a
  sentence you can mean; *"every MCP server may"* is not one you should be able
  to say by accident.

Tool descriptions and results are data, never instructions. A server cannot talk
koda into anything by putting directions in a tool description.

## Keeping it small

A server with sixty tools costs more context than the conversation. `tools`
takes only the ones named, `exclude` drops them, and `enabled = false` silences
a server without deleting its entry:

```toml
[[mcp_server]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
tools = ["search_issues", "get_issue"]
```

This matters more than it looks on a local model. The tool schema is the
largest fixed cost in every request, and the server caches by prefix — so the
schema is also what you pay to prefill the first time. Twenty tools you never
call are twenty tools you pay for on every turn, for ever.

## When something is wrong

- `koda mcp` connects and reports, including the failure if there is one.
- A server's own stderr goes to the event log — `/logs`, or `log_detail = true`
  for the full stream. A missing API key usually says so there.
- `mcp = false` turns the whole subsystem off without touching any entry.

Servers come up in the background: a handshake with somebody else's process must
not stand between you and your first prompt. Tools appear as each server
answers, and koda waits for the list to stop moving before warming the model's
prompt cache, so the shape it caches is the shape the next request sends.
