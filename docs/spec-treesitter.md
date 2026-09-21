# Tree-sitter extraction for the code graph and the search index

Status: landed behind the `treesitter` Cargo feature, on by default.
Code: `src/syntax.rs` (new), `graph::parse_file`, `index::chunk_file`.

## What changed

`graph::parse_file` is the one place both the code graph and the search index
get their facts about a file. It now tries a real parse first and falls back to
the line-pattern extractor:

| Language | Extraction |
| --- | --- |
| Rust, Python, JavaScript, TypeScript (and TSX), Go | Tree-sitter — `syntax-parsed` |
| everything else koda recognises | line patterns — `lexical` |

The graph's overview labels each language accordingly, and says plainly that
references are by name either way (see *Boundary* below).

A syntax parse returns the same facts as before, in the same conventions —
definitions (a method recorded both bare and as `Type::name`), imports in the
same text form, identifiers of three or more characters that are not keywords —
plus what only a parser knows:

- **No fake definitions.** `def fake():` inside a docstring and `// fn fake()`
  in a comment are text, not definitions.
- **Owners the patterns missed.** Go receiver methods (`func (s *Server)
  Start()` → `Server::Start`), TypeScript/JavaScript class methods, trait items.
- **Real extents.** Each definition's first line, last line, and the comments,
  attributes or decorators attached above it; whether it is nested in another.

## Search chunks

A chunk still runs from one definition to the next — measured best for
one-line gaps, closing braces, blanks. What the extents add: when three or
more lines of real code sit between a definition's end and the next one (a
script's body, module- or class-level statements), that code is its own chunk,
named after whatever encloses it, instead of the tail of the function above.
Chunk starts come from the parsed doc extent rather than a backwards line scan.

## Cost, measured

4,000 Rust files (the local Cargo registry, capped at `graph::MAX_FILES`),
release build, Apple Silicon:

| | lexical only | with Tree-sitter |
| --- | --- | --- |
| binary | 10.8 MB | 16.0 MB (+5.2 MB) |
| full scan | ~0.94 s | ~2.1 s |
| peak memory (scan process) | ~287 MB | ~338 MB (+18%) |
| definitions found | 85,287 | 80,763 |

The full scan runs on a background thread at startup and is followed only by
incremental per-file updates, so the scan-time cost is paid once, off the UI.
The lower definition count is mostly false definitions the line patterns found
in comments, strings and docstrings.

Retrieval quality on the repository's gold set (`tests/retrieval_gold.txt`, at
the shipped fusion constants) is unchanged: P@1 0.571, R@3 0.743, R@10 0.971,
MRR 0.696 before and after. That set is koda's own Rust source, where the
chunking change rarely applies; its target is script-shaped code, covered by
`a_script_body_is_not_the_tail_of_the_last_function`.

A packager who wants the smaller binary builds with
`--no-default-features --features docs,pdf`; every language then uses the
lexical extractor, and the full test suite still passes.

## Versions

`tree-sitter` 0.27, `tree-sitter-rust` 0.24, `-python` 0.25, `-javascript`
0.25, `-typescript` 0.23, `-go` 0.25 — all through `tree-sitter-language`, so
grammars and runtime agree on one ABI. Pinned in `Cargo.lock`.

## Boundary

Tree-sitter is syntax. It knows `foo()` calls something named `foo`; it does
not know *which* `foo`, across files or through an alias. That is semantic
resolution, and it belongs to the language server (`src/lsp.rs`).
