# Task-ranked code maps

Status: landed. Code: `src/repomap.rs`; wired into `codegraph query=context`
and, when a request names this project's code, into the request itself.

## The problem

The code graph's overview ranks symbols by how widely they are used. For
orientation that is right; for a task it is exactly wrong — the logging helper
and the error type top every list, and the function the request is about is
nowhere near it.

## The ranking

After Aider's repo map:

1. **A file graph.** File A points at file B when A uses a symbol B defines.
   The edge weighs how specific that symbol is (`1/√(places it is defined)`),
   and how good the evidence is: a mention read by Tree-sitter counts 1.0, one
   matched by a line pattern 0.5. A symbol the request names counts 10×;
   private-looking or very short names count less.
2. **Personalised PageRank** (damping 0.85, 30 iterations). The walk restarts
   at what the request is about: files it names, files defining symbols it
   names, and — at a fifth of the weight — files whose path or defined names
   share the request's words. With nothing to go on it degrades to plain
   PageRank, i.e. global importance.
3. **Symbols.** Each file's rank is shared out to the definitions its edges
   land on; a file the request names contributes its own definitions too, and
   symbols the request names are boosted outright.
4. **A budget.** Best-first, each definition's signature line is printed under
   its file until the token budget (≈4 characters a token) is spent. Every map
   is iterated in sorted order and ties break on (file, name), so the same
   inputs give the same map.

It is not semantic: edges are name matches between files. The language
server resolves names (`codegraph query=symbol`), but asking it about every
edge of a whole project on every request is not affordable; the ranking
weighs evidence instead of trusting it.

## Where it is used

- `codegraph query=context text="…" budget=N` (default 1024 tokens) — the
  model asks for the map of a task.
- **Attached to a request** that names a file or symbol of this project, at
  `graph_context_tokens` (default 600; 0 turns it off). Not attached in fast
  mode, and not when the request names nothing in the project — a general
  question does not pay for a map. Cached by graph generation, request and
  budget.

Hybrid search (`codegraph query=search`) is unchanged; the map is a different
question — "what code is this task about" rather than "where is this phrase".

## Measured

koda's own source against its retrieval gold set (35 plain-language
questions, each with the file that answers it): is that file in a 600-token
map?

| map | files found |
| --- | --- |
| untargeted (global importance, what an overview ranks by) | 19 / 35 |
| task-ranked | 23 / 35 |

These questions name no symbols, so only the weak word signal applies; a
request that names a file or symbol — the case the map is attached for —
anchors the walk exactly (`the_request_outranks_global_popularity`).
