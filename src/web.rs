//! Web search through a user-supplied SearXNG instance.
//!
//! SearXNG is the only backend on purpose: it is self-hostable, needs no API
//! key, and keeps queries off third-party services the user did not choose.
//! Nothing here runs unless `web_search` is on and `searx_url` is set.

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
    }
}
