# spec-doc-parsing — reading PDF / DOCX / XLSX / CSV into the model

Status: design (P2 feature). Heavy formats gated behind a Cargo feature so the
default ~6 MB binary is unchanged.

## Problem
`read_file` reads UTF-8 text only and rejects binaries (NUL in the first 8000
bytes), so PDF/DOCX/XLSX are invisible and CSV has no table structure.

## Per-format extraction (crate evaluation)
- CSV — std only, ~40-line quoted-field splitter. Zero dep. (Always on.)
- XLSX/XLS/ODS — `calamine` (MIT, pure Rust, read-only; brings zip + quick-xml).
- DOCX — reuse calamine's transitive `zip` + `quick-xml` to pull `<w:t>` text
  (no new top-level crate). Not docx-rs (heavy read+write DOM).
- PDF — `pdf-extract` (MIT/Apache; one `extract_text` call; heaviest — lopdf +
  miniz_oxide). Fallback: `lopdf` directly.

Honesty: `docs` is moderate weight, `pdf` is the expensive one. Default build
excludes both.

## Cargo features
```toml
[features]
default = []
docs = ["dep:calamine", "dep:zip", "dep:quick-xml"]
pdf  = ["dep:pdf-extract"]
```
CI builds default AND `--features docs,pdf`.

## Integration: extend `read_file` (not a new tool)
Least model surprise; reuses resolve/sandbox/limits. Before the `looks_binary`
guard, if `DocKind::from_ext(ext)` matches, dispatch to an internal
`read_document`; else today's behaviour. When a format's feature is off, return a
clear "rebuild koda with --features docs" message.

## Presentation
- CSV → header-marked aligned text table.
- XLSX/ODS → one block per sheet with `=== Sheet: "name" (C×R) ===` markers.
- PDF → page-delimited (`----- Page N -----`); scanned/image-only → pointer to
  the vision path, never OCR.
- DOCX → paragraphs (blank line between `<w:p>`), tables flattened.
All fed through the shared offset/limit line slicer; `ToolView::Read` with a
synthetic lang tag.

## Limits & untrusted content
- New `max_document_bytes` (~8 MiB) caps INPUT before parsing.
- Extracted text still run through `truncate(text, max_file_bytes)` as OUTPUT cap.
- `sanitize_text` drops NUL/C0 controls (keep `\n`, `\t`) to block terminal-escape
  injection. Extracted text is data, never instructions. Watch mode never scans
  these extensions.

## Images
Out of scope → vision path (see spec-image-input.md). `DocKind::from_ext` returns
None for image exts.

## Signatures / edits
- tools.rs: `enum DocKind {Csv,Xlsx,Docx,Pdf}` + `from_ext`/`tag`; `read_document`;
  `extract_csv`; cfg-gated `extract_xlsx`/`extract_docx`/`extract_pdf`;
  `sanitize_text`; shared `number_lines`.
- config.rs: `max_document_bytes` field + default + template.
- read_file: branch to `read_document` when the extension is a DocKind.
- Cargo.toml: the feature + optional-dep block above.

## Tests
Tiny fixtures (tiny.csv/xlsx/docx/pdf + scanned.pdf); unit tests for CSV quoting,
sheet/page markers, sanitize_text, feature-off stub message, truncation; e2e
read of a fixture when built `--features docs`.

## Phasing
1. CSV + plumbing (zero dep change). 2. `docs` feature. 3. `pdf` feature. Each
independently mergeable; default binary size unchanged until a packager opts in.
