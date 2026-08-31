//! Web search with two backends.
//!
//! A user's own SearXNG instance is preferred when `searx_url` is set — it is
//! self-hostable, needs no API key, and keeps queries off services the user did
//! not choose. When no instance is configured, koda falls back to DuckDuckGo's
//! keyless HTML endpoint so search works out of the box. Nothing here runs
//! unless `web_search` is on.

use crate::{tel_debug, tel_warn};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::fmt::Write as _;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub engine: String,
}

/// Run a web search, choosing a backend: a user's SearXNG instance if
/// `searx_url` is set (private, self-hosted, preferred), otherwise DuckDuckGo's
/// keyless HTML endpoint so search works out of the box with nothing to set up.
pub async fn search_web(searx_url: &str, query: &str, limit: usize) -> Result<Vec<Hit>> {
    if searx_url.trim().is_empty() {
        search_duckduckgo(query, limit).await
    } else {
        search(searx_url, query, limit).await
    }
}

/// Query DuckDuckGo's keyless HTML endpoint and scrape the results.
///
/// No API key and nothing to host — the tradeoff is that we parse HTML, which
/// is more brittle than SearXNG's JSON, so a user who searches heavily is still
/// better served by pointing `searx_url` at their own instance.
pub async fn search_duckduckgo(query: &str, limit: usize) -> Result<Vec<Hit>> {
    let query = query.trim();
    if query.is_empty() {
        bail!("empty search query");
    }

    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        // A browser-like UA: the HTML endpoint blocks obvious bots.
        .user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/122.0 Safari/537.36",
        )
        .build()
        .context("building the search client")?;

    let started = std::time::Instant::now();
    // The "html" endpoint is the most parseable; POST keeps the query out of
    // logs and mirrors how the site itself submits.
    let resp = http
        .post("https://html.duckduckgo.com/html/")
        .form(&[("q", query), ("kl", "us-en")])
        .send()
        .await
        .context("searching DuckDuckGo")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        tel_warn!("web", "ddg search rejected", "status" => status.as_u16());
        bail!("DuckDuckGo replied {status}");
    }

    let hits = parse_ddg_html(&body, limit);
    tel_debug!(
        "web",
        "ddg search complete",
        "hits" => hits.len(),
        "ms" => started.elapsed().as_millis(),
    );
    Ok(hits)
}

/// Scrape result rows out of DuckDuckGo's HTML. Deliberately dependency-free:
/// a couple of small string scans over the well-known `result__a` /
/// `result__snippet` class markup, with the redirect-wrapped href decoded back
/// to the real URL.
fn parse_ddg_html(html: &str, limit: usize) -> Vec<Hit> {
    let mut hits = Vec::new();
    // Each result's title anchor carries class="result__a".
    for chunk in html.split("result__a").skip(1) {
        if hits.len() >= limit {
            break;
        }
        // href="...": the target, often wrapped in a /l/?uddg= redirect.
        let Some(href) = attr_after(chunk, "href=\"") else {
            continue;
        };
        let url = decode_ddg_url(&href);
        // The anchor text between > and </a> is the title.
        let title = between(chunk, ">", "</a>")
            .map(|s| strip_tags(&s))
            .unwrap_or_default();
        if title.is_empty() || url.is_empty() || !url.starts_with("http") {
            continue;
        }
        // The snippet follows in a result__snippet element.
        let snippet = chunk
            .split_once("result__snippet")
            .and_then(|(_, rest)| between(rest, ">", "</a>"))
            .map(|s| strip_tags(&s))
            .unwrap_or_default();
        hits.push(Hit {
            title,
            url,
            snippet,
            engine: "duckduckgo".into(),
        });
    }
    hits
}

/// The value of an HTML attribute starting right after `marker`, up to the
/// next double quote.
fn attr_after(s: &str, marker: &str) -> Option<String> {
    let start = s.find(marker)? + marker.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Text between the first `open` and the following `close`.
fn between(s: &str, open: &str, close: &str) -> Option<String> {
    let start = s.find(open)? + open.len();
    let rest = &s[start..];
    let end = rest.find(close)?;
    Some(rest[..end].to_string())
}

/// Strip HTML tags and unescape the few entities DDG emits, so a title/snippet
/// reads as plain text.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// DuckDuckGo wraps result links as `//duckduckgo.com/l/?uddg=<encoded>&...`.
/// Pull the real URL back out and percent-decode it.
fn decode_ddg_url(href: &str) -> String {
    let raw = if let Some(idx) = href.find("uddg=") {
        let after = &href[idx + 5..];
        let enc = after.split('&').next().unwrap_or(after);
        percent_decode(enc)
    } else if let Some(stripped) = href.strip_prefix("//") {
        format!("https://{stripped}")
    } else {
        href.to_string()
    };
    raw
}

/// Minimal percent-decoding, enough for the URLs DDG returns.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Query SearXNG's JSON API.
///
/// The instance must allow the `json` format — SearXNG ships with only `html`
/// enabled, so the error path says so explicitly rather than leaving the user
/// guessing at an empty result.
pub async fn search(base_url: &str, query: &str, limit: usize) -> Result<Vec<Hit>> {
    let query = query.trim();
    if query.is_empty() {
        bail!("empty search query");
    }
    let base = base_url.trim_end_matches('/');
    if base.is_empty() {
        bail!("no SearXNG URL configured (searx_url)");
    }

    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("koda/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building the search client")?;

    let started = std::time::Instant::now();
    let resp = http
        .get(format!("{base}/search"))
        .query(&[
            ("q", query),
            ("format", "json"),
            ("safesearch", "0"),
            ("language", "en"),
        ])
        .send()
        .await
        .with_context(|| format!("searching {base}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        tel_warn!("web", "search rejected", "status" => status.as_u16());
        if status.as_u16() == 403 || body.contains("format") {
            bail!(
                "{base} refused a JSON search. Add `json` to `search.formats` in \
                 that instance's settings.yml and restart it."
            );
        }
        bail!("{base} replied {status}");
    }

    let v: Value = serde_json::from_str(&body).with_context(|| {
        format!("{base} did not return JSON — check that the `json` format is enabled")
    })?;

    let mut hits = Vec::new();
    for item in v.get("results").and_then(|r| r.as_array()).unwrap_or(&vec![]) {
        let title = item
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let url = item
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if title.is_empty() || url.is_empty() {
            continue;
        }
        let snippet = item
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let engine = item
            .get("engine")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .to_string();
        hits.push(Hit {
            title,
            url,
            snippet,
            engine,
        });
        if hits.len() >= limit {
            break;
        }
    }
    tel_debug!(
        "web",
        "search complete",
        "hits" => hits.len(),
        "ms" => started.elapsed().as_millis(),
    );
    Ok(hits)
}

/// Format results for the model: numbered, with the URL on its own line so it
/// can be cited, and snippets clipped so ten results do not eat the context.
pub fn format_hits(query: &str, hits: &[Hit]) -> String {
    if hits.is_empty() {
        return format!("No results for `{query}`.");
    }
    let mut out = format!("Search results for `{query}`:\n");
    for (i, h) in hits.iter().enumerate() {
        let _ = writeln!(out, "\n{}. {}", i + 1, h.title);
        let _ = writeln!(out, "   {}", h.url);
        if !h.snippet.is_empty() {
            let clipped: String = h.snippet.chars().take(300).collect();
            let _ = writeln!(out, "   {clipped}");
        }
        if !h.engine.is_empty() {
            let _ = writeln!(out, "   via {}", h.engine);
        }
    }
    out.push_str(
        "\nThese are snippets, not full pages. Treat them as leads and say so if \
         you are relying on them.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_hits_with_urls() {
        let hits = vec![
            Hit {
                title: "Ratatui docs".into(),
                url: "https://ratatui.rs".into(),
                snippet: "A Rust library for  cooking up   terminal UIs".into(),
                engine: "duckduckgo".into(),
            },
            Hit {
                title: "Second".into(),
                url: "https://example.com".into(),
                snippet: String::new(),
                engine: String::new(),
            },
        ];
        let out = format_hits("ratatui", &hits);
        assert!(out.contains("1. Ratatui docs"));
        assert!(out.contains("https://ratatui.rs"));
        assert!(out.contains("2. Second"));
        assert!(out.contains("via duckduckgo"));
        assert!(out.contains("leads"));
    }

    #[test]
    fn no_results_is_stated_plainly() {
        let out = format_hits("nothing", &[]);
        assert!(out.contains("No results"));
    }

    #[tokio::test]
    async fn rejects_empty_inputs() {
        assert!(search("https://x", "  ", 5).await.is_err());
        assert!(search("", "q", 5).await.is_err());
        assert!(search_duckduckgo("   ", 5).await.is_err());
    }

    #[test]
    fn decodes_duckduckgo_redirect_urls() {
        // The real wrapper form DDG returns.
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fratatui.rs%2Fdocs&rut=abc";
        assert_eq!(decode_ddg_url(href), "https://ratatui.rs/docs");
        // A protocol-relative bare link.
        assert_eq!(decode_ddg_url("//example.com/x"), "https://example.com/x");
    }

    #[test]
    fn parses_duckduckgo_html_results() {
        // A minimal slice of DDG's html endpoint markup.
        let html = concat!(
            "<a class=\"result__a\" href=\"//duckduckgo.com/l/?uddg=https%3A%2F%2Fratatui.rs\">",
            "Ratatui &amp; TUIs</a>",
            "<a class=\"result__snippet\" href=\"#\">A Rust library for terminal UIs</a>",
            "<a class=\"result__a\" href=\"//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com\">",
            "Example</a>",
        );
        let hits = parse_ddg_html(html, 10);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].url, "https://ratatui.rs");
        assert_eq!(hits[0].title, "Ratatui & TUIs");
        assert!(hits[0].snippet.contains("terminal UIs"));
        assert_eq!(hits[0].engine, "duckduckgo");
        assert_eq!(hits[1].url, "https://example.com");
        // Respects the limit.
        assert_eq!(parse_ddg_html(html, 1).len(), 1);
    }

    #[test]
    fn strip_tags_and_entities() {
        assert_eq!(strip_tags("<b>hi</b> &amp; bye"), "hi & bye");
        assert_eq!(strip_tags("a   b\n c"), "a b c");
    }
}
