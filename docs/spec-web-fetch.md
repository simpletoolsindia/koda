# spec: `web_fetch` tool

A tool that GETs a single URL and returns its readable text to the model. It is
the natural companion to `web_search`: search returns leads (title/url/snippet),
`web_fetch` lets the agent actually read one of those pages. It reuses koda's
existing reqwest + hand-rolled HTML-scraping patterns and is gated exactly like
`web_search`.

## Goals
- GET an `http`/`https` URL, strip HTML to plain text, return it to the model.
- Cap output with the existing `max_tool_output_bytes`; apply request timeouts.
- Config flag `web_fetch: bool`, a `/settings` toggle row, and removal from
  `advertised_tools` when off — mirroring `web_search`.
- Read-only, non-mutating tool. Treat fetched bytes as untrusted data.

## Non-goals
- No JS rendering, crawling, pagination, or link-following.
- No new HTTP stack, no readability/boilerplate heuristics beyond tag stripping.
- No caching layer.

## Dependency decision: hand-rolled, no new crate
`src/web.rs` already ships tag-stripping and whitespace-collapsing helpers, and
`regex` is already a dependency. Adding `scraper`/`html2text` is disproportionate
for "give me the visible text of a page." Extend the existing stripping with
`<script>`/`<style>`/comment removal in `web.rs`.

## New code in `src/web.rs`
- `pub async fn fetch_url(url, timeout_secs) -> Result<String>` — http/https only,
  browser UA, connect timeout 5s / request timeout ~20s, streams the body with a
  5 MiB on-the-wire cap (`read_capped`), then `html_to_text` when the content-type
  is HTML (or it looks like HTML), else the trimmed body.
- `html_to_text(html)` — remove `<script>/<style>/<!-- -->` regions (regex),
  insert `\n` for block-closing tags/`<br>`, drop remaining tags keeping newlines,
  unescape entities, collapse blank lines.

## Config (`src/config.rs`)
`#[serde(default)] pub web_fetch: bool` + `web_fetch: false` in Default + a
template line.

## Settings row (`src/settings.rs`)
A plain toggle mirroring `Row::WebSearch`: variant `WebFetch`, add to `ALL`,
label "web fetch", hint "let the agent GET a URL and read it as text", toggle in
`change()`, `on()` in `value()`.

## Spec (`src/tools.rs`, after web_search)
`web_fetch` — params `url` (required) + optional `max_bytes`; `mutating: false`.
Add `"web_fetch"` to `PLAN_TOOLS`; leave PARALLEL_SAFE unchanged (network stays
sequential). Make `truncate` `pub(crate)` and reuse it.

## Agent wiring (`src/agent.rs`)
- Filter `web_fetch` out of `advertised_tools` when `!cfg.web_fetch`.
- Dispatch arm `"web_fetch" => self.web_fetch(&args).await`.
- Method returns a disabled error when off; otherwise fetches, truncates to
  `min(max_bytes, max_tool_output_bytes)`, returns a Plain outcome.
- Add a preview label `"web_fetch" => format!("fetch {}", url)`.

## Safety
- Scheme allowlist: only http/https (reject file:/data:/ftp:).
- SSRF/localhost: a model-supplied URL becomes a request from the user's host and
  can reach 127.0.0.1 / 169.254.169.254 / RFC-1918. Keep the flag OFF by default;
  document it. Hardening follow-up: block loopback/private/link-local IPs after
  DNS resolution and on each redirect hop.
- Untrusted content: returned page text is data, not instructions (say so in desc).
- Bounds: connect 5s, request 20s, 5 MiB wire cap, then `max_tool_output_bytes`.

## Tests (offline, in `web.rs`)
- `html_to_text` strips script/style/comments and tags, unescapes entities,
  inserts breaks between blocks, collapses blank lines.
- `fetch_url` rejects non-http(s) schemes and empty URLs.
