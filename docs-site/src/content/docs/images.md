---
title: Files, images & OCR
description: What read_file does with text, CSVs, PDFs, spreadsheets and screenshots — and what happens when your model has no vision.
---

Mention a file with `@` and koda decides how to hand it to the model.

```
@report.pdf summarise this
@data.csv which row is highest?
@screenshot.png what is this error saying?
```

Mentions of ordinary source files are left alone for the agent to fetch with `read_file`
when it needs them. Documents come with your message, so the model answers straight away
instead of spending a turn asking to read what you just pointed at.

## What each kind becomes

| Kind | What the model gets |
| --- | --- |
| Text | Numbered lines. `offset`/`limit` for big files. |
| `.csv .tsv .tab` | An aligned table with a header rule, so columns read reliably. |
| `.pdf` | Extracted text. |
| `.docx` | Extracted text. |
| `.xlsx .xlsm .xls .ods` | Extracted cell content. |
| `.png .jpg .jpeg .gif .webp .bmp .tiff .avif .svg` | Attached as an image, for a vision model. |

Attachments are capped at `max_file_bytes`.

:::note[Legacy `.doc` is not supported]
Only `.docx`. The pre-2007 binary Word format is a different thing entirely — convert it,
or open it and save as `.docx`.
:::

Document reading is on by default and adds about 1.4 MB to the binary, which is the price
of "read this spreadsheet" working when it is asked. A packager who wants the smaller
build can use `cargo build --release --no-default-features`.

## Images and vision

A model with no vision simply never sees the extra content, so leaving image attachment on
is safe. koda detects vision capability from the model name.

## OCR fallback

If you attach an image and your model is not vision-capable, `ocr` (off by default, in
`/settings`) extracts the image's text and sends that instead — so a screenshot of an
error still reaches a text-only model.

There are two backends, and koda tries them in this order:

**1. A vision model you name.** Set `ocr_model` to a vision-capable model — often one
already reachable on the same endpoint, since most routers and local servers host several.
koda sends the image there, asks for a verbatim transcription plus a short description of
any non-text content, and hands the reply to your active model in place of the picture.

**2. `tesseract`.** The offline fallback, used if `ocr_model` is unset or that request
failed. Needs the CLI installed (`brew install tesseract`, `apt install tesseract-ocr`);
if it is missing koda says so and skips the image.

Tesseract reads printed text and nothing else — it misses layout, tables, handwriting, and
anything that is not text. `ocr_model` is the better backend when you have one, which is
why declining tesseract at install time does not cost you the feature.
