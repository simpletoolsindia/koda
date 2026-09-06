---
name: web-research-and-media
when: Exploring websites, conducting human-like web research, filling interactive forms, searching products or articles, inspecting visual layouts, or downloading media
---

# Human-Like Web Research, Exploration, Form Filling & Media Guide

This skill guides Koda to browse, search, fill forms, and research websites with the thoroughness, skepticism, and methodology of an expert human researcher.

---

## 1. The Human Researcher Mindset

When a human researches a website (e.g. searching products on Flipkart/Amazon, researching papers on arXiv, or reading technical documentation):
1. **Never settle for a single surface result**: Do not just grab the first item and stop. Human researchers look at multiple options, check ratings, read specifications, and compare alternatives.
2. **Formulate high-signal queries**: Use specific search queries (e.g. "wireless ergonomic mouse with bluetooth multi-device", not just "mouse").
3. **Use Filters & Sorting**: On e-commerce or directory sites, identify filter sidebars and sort dropdowns (e.g. "Customer Rating", "Price: Low to High", "Brand").
4. **Drill Down & Return**:
   - Click into a promising candidate (`action="click"`).
   - Read the detailed specifications, price breakdown, and real user reviews.
   - Use `action="back"` to return to the search list and inspect the next candidate, OR open a new tab (`tab=2`) to compare candidates side-by-side!
5. **Synthesize Findings**: Present concrete comparison tables containing: Product/Article Name, Price/Date, Key Features/Specs, Rating/Review Summary, and Direct Link.

---

## 2. Interactive Navigation & Exploration Actions

Use the `browse` tool for live browser interaction powered natively by `agent-browser`.
Elements in the returned snapshot are indexed with references like `@e1`, `@e2...`. You can target elements using `selector: "@e1"`, `index: 1`, or any CSS selector.

| Action | Description | Key Parameters |
|---|---|---|
| `navigate` | Open URL or inspect current page | `url: "https://..."` |
| `search` | Search via popular engine | `query: "..."`, `engine: "duckduckgo"` / `"google"` / `"youtube"` / `"bing"` |
| `click` | Click button, link, or tab | `index: <num>` or `selector: "@e<N>"` / `"..."` |
| `type` / `input` | Type text with realistic human keystrokes | `index: <num>` or `selector: "@e<N>"`, `text: "..."`, `press_enter: true/false`, `clear: true` |
| `select` | Select option in a `<select>` dropdown | `index: <num>`, `text: "Option label or value"` |
| `check` / `uncheck` | Check or uncheck a checkbox / radio | `index: <num>` or `selector: "..."` |
| `hover` | Hover mouse over element (flyout menus, tooltips) | `index: <num>` or `selector: "..."` |
| `press` | Send keyboard shortcut | `key: "Enter"` / `"Escape"` / `"Tab"` / `"ArrowDown"` |
| `scroll` | Scroll down or up | `direction: "down"` / `"up"` / `"top"` / `"bottom"`, `pages: 1.0` |
| `back` / `forward` | Go back or forward in session history | *(none)* |
| `reload` | Reload the active page | *(none)* |
| `tab` | Switch active tab (1-based) | `tab: 1` / `tab: 2` |
| `upload` | Upload a local workspace file | `file: "path/to/file"`, `index: <num>` or `selector: "..."` |
| `screenshot` | Capture crisp 16:9 viewport screenshot | `to: "view.png"`, `highlight: true/false`, `full_page: false` |
| `screenshot_element` | Crop screenshot of single element | `index: <num>` or `selector: "..."`, `to: "element.png"` |
| `wait` | Wait for network/dynamic DOM | `seconds: 2.0` or `wait_for: "selector"` |
| `download` | Download authenticated resource | `url: "..."`, `to: "downloads/file.ext"` |
| `close` | Release browser session | *(none)* |

---

## 3. Form Filling Protocol

When interacting with forms (search bars, authentication, checkout, filters, registration):
1. **Identify Required Fields**:
   - Inspect the interactive elements list: look for `<input>`, `<textarea>`, `<select>`, and `[role="button"]`.
   - View their `placeholder`, `name`, `type`, and `aria-label`.
2. **Fill Systematically**:
   - **Text inputs**: `browse({"action": "type", "index": <idx>, "text": "...", "press_enter": false})`
   - **Dropdown selects**: `browse({"action": "select", "index": <idx>, "text": "Visible Option"})`
   - **Checkboxes/Radios**: `browse({"action": "check", "index": <idx>})`
3. **Submit with Intent**:
   - Either click the dedicated submit button: `browse({"action": "click", "index": <submit_idx>})`
   - Or submit via enter: `browse({"action": "type", "index": <idx>, "text": "...", "press_enter": true})`
4. **Verify Submission Outcome**:
   - Always read the returned page title, URL, and interactive elements to verify:
     - Did the page navigate?
     - Are search results now displayed?
     - Did a form validation error appear?
     - Did a login or cookie banner popup appear? (The auto-dismiss watchdog handles common dialogs, but if one remains, click its close button).

---

## 4. Visual Verification with Vision Models

When text alone is ambiguous or you want to evaluate UI layout, badges, color swatches, or image quality:
1. Capture the viewport:
   `browse({"action": "screenshot", "to": "ui.png", "highlight": true})`
   *(Bounding box badges `[1]`, `[2]` will be overlayed onto interactive elements)*.
2. Inspect visually with `view_image`:
   `view_image({"path": "ui.png", "prompt": "Analyze the search results, product badges, and prices shown."})`
3. Or crop an individual product or chart:
   `browse({"action": "screenshot_element", "index": <idx>, "to": "product-card.png"})`
   `view_image({"path": "product-card.png", "prompt": "Transcribe the specs and warranty details."})`

---

## 5. Media Downloads & Durable Knowledge

1. **Direct Media / Documents (`.pdf`, `.mp4`, `.zip`)**:
   `browse({"action": "download", "url": "<download_url>", "to": "downloads/<filename>"})`
2. **Video Platforms (YouTube, Vimeo)**:
   - Identify video URL via `browse`.
   - Download in high quality using `run_command`:
     `yt-dlp -f "best[ext=mp4]/best" -o "downloads/%(title)s.%(ext)s" "<url>"`
   - Verify file on disk with `list_dir({"path": "downloads"})`.
3. **Record Durable Findings**:
   - If you discovered important facts that will matter in future sessions, call `remember({"note": "..."})`.
   - If you discovered a repeatable workflow for a specific platform, record it with `manage_skill`.
