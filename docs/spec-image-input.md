# Image input — gap analysis & completion plan

Status: audit of existing support + minimal plan to close gaps. No heavy deps —
koda already hand-rolls its own base64 encoder.

## What already works
End-to-end: `@image.png` mention → `data:` URL → OpenAI multimodal `content`.

- `Message.images: Vec<String>` of data URLs, `#[serde(skip)]` so it never hits
  the wire shape or session file; constructor `Message::user_with_images` (llm.rs).
- `ChatRequest::to_json` expands image-bearing messages into `content` arrays:
  a `{type:text}` part (if text non-empty) + one `{type:"image_url",image_url:{url}}`
  per image. Imageless messages serialize unchanged. (Tested.)
- `image_mime` / `is_image_path` / `image_data_url(path, max_bytes)` and a tested
  RFC-4648 `base64_encode` in tools.rs build the data URL.
- `Agent::user_message` scans `@`-tokens, and for image paths resolves + encodes
  (size-checked against `max_file_bytes`), attaching via `user_with_images`.

## Gaps
- G1 Extensions: only png/jpg/jpeg/gif/webp; bmp/tiff/avif/svg fall through to text.
- G2 MIME is extension-only (a mislabelled file sends the wrong MIME).
- G3 base64 encoder duplicated in tui.rs (OSC-52 clipboard) — consolidate.
- G4 Size cap is on raw bytes (256 KiB default); base64 inflates ~33%; many images
  are unbounded in total.
- G5 No vision-capability gating: images attach regardless of model; a non-vision
  model may 400 with no clear hint.
- G6 Attach only via `@mention`; a bare path in a normal message is not attached.

## Minimal completion plan (no new crates)
1. Broaden `image_mime`: add bmp, tif/tiff, avif, svg (image/svg+xml).
2. De-duplicate base64: tui.rs OSC-52 calls `tools::base64_encode`.
3. Clarify the size check message; optionally add `max_image_bytes` config.
4. Vision awareness: `llm::model_is_vision(&str) -> bool` (substring heuristic:
   vl, vision, llava, qwen2-vl, minicpm-v, gemma-3, pixtral, gpt-4o, llama-3.2…);
   still send, but emit a Notice when the model likely can't see images.
5. Bare-path attach: a second pass in `user_message` over non-`@` tokens that look
   like an image path under the workspace.
6. Optional magic-byte MIME sniff (no dep).

Dependency stance: reuse `tools::base64_encode`; Cargo.toml unchanged.

## Implemented: vision detection + OCR fallback

- `llm::model_is_vision(model) -> bool` (llm.rs): substring heuristic over the
  model id (vl, vision, llava, qwen2-vl, minicpm-v, gemma-3, llama-3.2, pixtral,
  gpt-4o, gemini, glm-4v, …). False negatives only downgrade to OCR, never a
  wrong answer.
- `Agent::user_message` now branches on capability:
  - vision model → attach the image as a data URL (as before);
  - non-vision + `ocr = true` → run `tools::ocr_image` (shells out to the
    `tesseract` CLI, `tesseract <img> stdout`) and fold the recognized text into
    the message as an `[OCR text of …]` block;
  - non-vision + OCR off → skip the image with a notice pointing at `/settings`.
- `ocr` config flag (default false) + a **image ocr** settings toggle. No new
  Rust dependency; if tesseract isn't installed, `ocr_image` returns a clear,
  actionable error and the image is skipped.

Extensions were also broadened to png/jpg/jpeg/gif/webp/bmp/tiff/avif/svg.
Still open (lower priority): bare-path (non-`@`) attach, magic-byte MIME sniff,
and a separate `max_image_bytes`.
