# Language servers: when the answer has to be right

koda's code graph is regexes. That is a deliberate trade — it is instant, needs
nothing installed, and is right often enough to point at the right file, which
the model then reads properly. What it cannot do is resolve anything. It does
not know that the `run` you asked about is the trait method and not the free
function three modules over; its "who uses this" is a name match, not a
resolution; and it has no idea what type anything is.

For those questions a real language server is not a nicer answer, it is the only
correct one. So this sits beside the graph rather than replacing it.

## Using it

The `lsp` tool takes 1-based positions, and you give it the **symbol name as it
appears on the line** rather than a column — counting characters is a thing
models get wrong:

```
lsp action=definition  file=src/agent.rs line=412 symbol=execute
lsp action=references  file=src/agent.rs line=412 symbol=execute
lsp action=hover       file=src/agent.rs line=412 symbol=outcome
lsp action=diagnostics file=src/agent.rs
```

| action | |
|---|---|
| `definition`, `type_definition`, `implementation` | resolve, not name-match |
| `references` | every genuine use |
| `hover` | the type, signature and doc of an expression |
| `diagnostics` | what the compiler or type checker says is wrong |
| `document_symbols`, `workspace_symbols` | outline one file, or find a symbol anywhere |
| `servers` | which servers koda knows and which are usable here |

`/lsp` shows the same server report plus any diagnostics collected so far.

## What it costs

Nothing, until you have a server installed. koda checks for one per language at
startup — a few `stat` calls for the project's marker files (`Cargo.toml`,
`go.mod`, `pyproject.toml`, …) and a PATH lookup only for the ones that match —
and if nothing turns up, the `lsp` tool is not even advertised. A project with
no language server does not pay a schema entry for a tool that could only ever
answer "no server".

The server itself starts on first use, not at launch: rust-analyzer on a large
workspace is a minute of CPU and a gigabyte of memory, which a session that
never asks a type-aware question should not pay. `lsp_eager = true` starts them
at launch instead, so the first call answers immediately.

koda waits for a server to finish indexing before believing an empty answer.
Queried mid-index, rust-analyzer returns `null` — which reads exactly like "no
such symbol" and is the single most misleading thing this tool could report.

## With the code graph

`codegraph query=symbol` folds in a language server's resolved answer when one
is already running, under a hard time budget, failing silently. The graph's
promise — instant, always available — is not up for negotiation, so a server
that is cold, missing or slow is skipped rather than waited for. Turn it off
with `lsp_in_codegraph = false`.

Keep reaching for `codegraph` first, for orientation. Reach for `lsp` when a
name is ambiguous or overloaded, or when you need a type.

## "It says it will not run"

Being on PATH is not the same as being installed. `rustup` puts a proxy for
`rust-analyzer` in `~/.cargo/bin` whether or not the component is there, and
running it just prints `error: Unknown binary`. `lsp action=servers` and `/lsp`
check by running each candidate, so they report **on PATH but will not run**
rather than claiming it is usable. The fix is usually one command:

```
rustup component add rust-analyzer
```

## Servers koda knows

rust-analyzer, pyright, pylsp, typescript-language-server, gopls, clangd, zls,
lua-language-server, solargraph, dart, elixir-ls, ocamllsp. Each is the standard
server for its language and each speaks LSP on stdio, which is the only
transport here. `lsp action=servers` says which are installed on this machine
and which are relevant to this project.
