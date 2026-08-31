//! A tiny local web server for the koda debug/observability UI.
//!
//! No web framework: koda stays dependency-light, so this is a minimal HTTP/1.1
//! server on raw tokio. It binds to 127.0.0.1 only and serves:
//!
//!   GET /                     the React UI (from `web-ui/dist/` if built,
//!                             otherwise a small built-in fallback page)
//!   GET /api/logs?since=&lvl= JSON log entries (live agent telemetry)
//!   GET /api/events           Server-Sent Events stream of new log lines
//!   GET /api/debug            captured raw request/response sessions
//!   GET /api/codegraph        the project symbol graph as nodes + edges
//!   GET /api/skills           skills and role agents (name/when/role/body)
//!   POST /api/skills          create/update a skill or role agent
//!   DELETE /api/skills/<name> remove a skill
//!
//! It is read-mostly and intended for localhost use during development.

use crate::log::{self, Level};
use serde::Serialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Map the UI detail level to the minimum log level surfaced.
fn detail_to_level(detail: &str) -> Level {
    match detail.trim().to_ascii_lowercase().as_str() {
        "simple" | "low" => Level::Info,
        "high" | "full" | "debug" => Level::Debug,
        _ => Level::Debug, // "medium": include debug; the UI filters client-side
    }
}

#[derive(Serialize)]
struct LogLine {
    seq: u64,
    at: f64,
    level: &'static str,
    area: &'static str,
    message: String,
    fields: Vec<(String, String)>,
}

#[derive(Serialize)]
struct GraphNode {
    id: String,
    kind: String,
    file: String,
    line: usize,
    refs: usize,
}

#[derive(Serialize)]
struct GraphEdge {
    from: String,
    to: String,
    kind: &'static str,
}

#[derive(Serialize)]
struct GraphJson {
    files: usize,
    languages: Vec<(String, usize)>,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    truncated: bool,
}

#[derive(Serialize)]
struct SkillJson {
    name: String,
    when: String,
    role: Option<String>,
    body: String,
    source: String,
}

/// Shared, cheap-to-clone server context.
#[derive(Clone)]
struct Ctx {
    root: PathBuf,
    detail: String,
}

/// Start the web UI server if enabled. Returns the bound address on success.
/// Failures are logged and swallowed — the UI is optional and must never stop
/// koda from running.
pub async fn start(root: PathBuf, port: u16, detail: String) -> Option<SocketAddr> {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            crate::tel_warn!("webui", format!("could not bind {addr}: {e}"));
            return None;
        }
    };
    let bound = listener.local_addr().ok()?;
    crate::tel_info!("webui", "web UI listening", "addr" => bound);
    let ctx = Arc::new(Ctx { root, detail });
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let ctx = ctx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle(stream, ctx).await {
                            crate::tel_debug!("webui", format!("connection ended: {e}"));
                        }
                    });
                }
                Err(e) => {
                    crate::tel_warn!("webui", format!("accept failed: {e}"));
                    break;
                }
            }
        }
    });
    Some(bound)
}

async fn handle(mut stream: TcpStream, ctx: Arc<Ctx>) -> std::io::Result<()> {
    // Read the request head (enough for method, path, headers). Bodies for our
    // POSTs are small, so we read what's buffered.
    let mut buf = vec![0u8; 64 * 1024];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let raw = String::from_utf8_lossy(&buf[..n]).to_string();
    let mut lines = raw.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };

    let (status, ctype, payload) = route(method, path, query, body, &ctx).await;
    write_response(&mut stream, status, ctype, &payload).await
}

async fn route(
    method: &str,
    path: &str,
    query: &str,
    body: &str,
    ctx: &Ctx,
) -> (&'static str, &'static str, Vec<u8>) {
    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            let html = load_index(&ctx.root);
            ("200 OK", "text/html; charset=utf-8", html.into_bytes())
        }
        ("GET", "/api/logs") => {
            let json = logs_json(query, &ctx.detail);
            ("200 OK", "application/json", json.into_bytes())
        }
        ("GET", "/api/events") => {
            // A one-shot snapshot framed as SSE. The client reconnects/polls;
            // keeping the connection stateless avoids a long-lived task per tab.
            let json = logs_json(query, &ctx.detail);
            let framed = format!("event: logs\ndata: {json}\n\n");
            ("200 OK", "text/event-stream", framed.into_bytes())
        }
        ("GET", "/api/debug") => {
            let json = debug_json();
            ("200 OK", "application/json", json.into_bytes())
        }
        ("GET", "/api/codegraph") => {
            let json = codegraph_json(&ctx.root);
            ("200 OK", "application/json", json.into_bytes())
        }
        ("GET", "/api/skills") => {
            let json = skills_json(&ctx.root);
            ("200 OK", "application/json", json.into_bytes())
        }
        ("POST", "/api/skills") => {
            let json = save_skill(&ctx.root, body);
            ("200 OK", "application/json", json.into_bytes())
        }
        _ => (
            "404 Not Found",
            "application/json",
            br#"{"error":"not found"}"#.to_vec(),
        ),
    }
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then_some(v)
    })
}

fn logs_json(query: &str, detail: &str) -> String {
    let since: u64 = query_param(query, "since").and_then(|s| s.parse().ok()).unwrap_or(0);
    // The ring doesn't track a per-entry sequence, so derive a stable seq from
    // position by fetching a generous window and numbering it.
    let min = detail_to_level(detail);
    let entries = log::recent(min, 1000);
    let base = log::version().saturating_sub(entries.len() as u64);
    let mut out: Vec<LogLine> = Vec::new();
    for (i, e) in entries.into_iter().enumerate() {
        let seq = base + i as u64;
        if seq < since {
            continue;
        }
        out.push(LogLine {
            seq,
            at: e.at,
            level: e.level.label(),
            area: e.area,
            message: e.message,
            fields: e.fields,
        });
    }
    serde_json::json!({ "version": log::version(), "entries": out }).to_string()
}

fn debug_json() -> String {
    let dir = crate::debug::dir();
    let mut sessions: Vec<serde_json::Value> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut names: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        names.sort();
        for req in names {
            let res = req.with_extension("res.log");
            let request = std::fs::read_to_string(&req).unwrap_or_default();
            let response = std::fs::read_to_string(&res).unwrap_or_default();
            sessions.push(serde_json::json!({
                "id": req.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                "request": request,
                "response": response,
            }));
        }
    }
    serde_json::json!({
        "enabled": crate::debug::enabled(),
        "dir": dir.display().to_string(),
        "sessions": sessions,
    })
    .to_string()
}

fn codegraph_json(root: &Path) -> String {
    let g = crate::graph::scan(root);
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    // One node per definition; cap so the browser stays responsive.
    const CAP: usize = 1500;
    for (name, defs) in g.defs.iter() {
        for d in defs {
            if nodes.len() >= CAP {
                break;
            }
            let refs = g.refs.get(name).map(|s| s.len()).unwrap_or(0);
            nodes.push(GraphNode {
                id: name.clone(),
                kind: d.kind.to_string(),
                file: d.file.clone(),
                line: d.line,
                refs,
            });
            // Edge: file "contains" this symbol (file nodes are implicit).
            edges.push(GraphEdge { from: d.file.clone(), to: name.clone(), kind: "defines" });
        }
    }
    let languages: Vec<(String, usize)> =
        g.languages.iter().map(|(k, v)| (k.clone(), *v)).collect();
    serde_json::to_string(&GraphJson {
        files: g.files,
        languages,
        nodes,
        edges,
        truncated: g.truncated,
    })
    .unwrap_or_else(|_| "{}".into())
}

fn skills_json(root: &Path) -> String {
    let skills = crate::skills::load(root);
    let out: Vec<SkillJson> = skills
        .into_iter()
        .map(|s| SkillJson {
            name: s.name,
            when: s.when,
            role: s.role,
            body: s.body,
            source: s.source.display().to_string(),
        })
        .collect();
    serde_json::to_string(&out).unwrap_or_else(|_| "[]".into())
}

/// Create or update a skill/role agent from a JSON body:
/// `{ "name": "...", "when": "...", "role": "qa"?, "body": "..." }`.
fn save_skill(root: &Path, body: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({ "ok": false, "error": format!("bad json: {e}") }).to_string(),
    };
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let when = v.get("when").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let bodytext = v.get("body").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let role = v.get("role").and_then(|x| x.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if name.is_empty() || when.is_empty() || bodytext.is_empty() {
        return serde_json::json!({ "ok": false, "error": "name, when and body are required" }).to_string();
    }
    let front_role = role.as_ref().map(|r| format!("role: {r}\n")).unwrap_or_default();
    let doc = format!("---\nname: {name}\n{front_role}when: {when}\n---\n\n{bodytext}\n");
    if crate::skills::parse(&doc).is_none() {
        return serde_json::json!({ "ok": false, "error": "composed skill did not parse" }).to_string();
    }
    let dir = root.join(".koda").join("skills");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return serde_json::json!({ "ok": false, "error": format!("mkdir: {e}") }).to_string();
    }
    // Sanitize the filename from the name.
    let file: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let path = dir.join(format!("{file}.md"));
    match std::fs::write(&path, doc) {
        Ok(()) => serde_json::json!({ "ok": true, "path": path.display().to_string() }).to_string(),
        Err(e) => serde_json::json!({ "ok": false, "error": format!("write: {e}") }).to_string(),
    }
}

/// Load the built React app if present, else a helpful placeholder page.
fn load_index(root: &Path) -> String {
    for candidate in [
        root.join("web-ui").join("dist").join("index.html"),
        PathBuf::from("web-ui/dist/index.html"),
    ] {
        if let Ok(html) = std::fs::read_to_string(&candidate) {
            return html;
        }
    }
    FALLBACK_INDEX.to_string()
}

async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    ctype: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {ctype}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

/// A minimal, dependency-free page shown when the React app hasn't been built.
/// It talks to the same API, so logs and debug are usable immediately.
const FALLBACK_INDEX: &str = r#"<!doctype html><html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>koda observability</title>
<script src="https://cdn.tailwindcss.com"></script>
</head><body class="bg-slate-950 text-slate-200 font-mono">
<div class="max-w-5xl mx-auto p-6">
  <h1 class="text-2xl font-bold text-emerald-400">koda · live logs</h1>
  <p class="text-slate-400 text-sm mb-4">Fallback view. Build <code>web-ui/</code> for the full React UI.</p>
  <div id="logs" class="text-xs whitespace-pre-wrap bg-black/40 rounded p-3 h-[70vh] overflow-auto"></div>
</div>
<script>
let since = 0;
async function tick(){
  try {
    const r = await fetch('/api/logs?since='+since);
    const j = await r.json();
    const el = document.getElementById('logs');
    for (const e of j.entries){ since = Math.max(since, e.seq+1);
      const line = document.createElement('div');
      const c = {error:'text-rose-400',warn:'text-amber-400',info:'text-slate-300',debug:'text-slate-500'}[e.level]||'';
      line.className = c;
      line.textContent = e.at.toFixed(3).padStart(8)+'  '+e.level.padEnd(5)+' '+e.area.padEnd(8)+' '+e.message;
      el.appendChild(line);
    }
    el.scrollTop = el.scrollHeight;
  } catch(_){}
}
setInterval(tick, 1000); tick();
</script>
</body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_maps_to_level() {
        assert_eq!(detail_to_level("simple"), Level::Info);
        assert_eq!(detail_to_level("high"), Level::Debug);
        assert_eq!(detail_to_level("medium"), Level::Debug);
    }

    #[test]
    fn query_param_extracts() {
        assert_eq!(query_param("since=5&lvl=info", "since"), Some("5"));
        assert_eq!(query_param("since=5&lvl=info", "lvl"), Some("info"));
        assert_eq!(query_param("since=5", "missing"), None);
    }

    #[test]
    fn save_skill_validates() {
        let bad = save_skill(Path::new("/tmp/koda-webui-test"), "{}");
        assert!(bad.contains("\"ok\":false"));
    }

    #[test]
    fn fallback_index_is_html() {
        assert!(FALLBACK_INDEX.contains("<html"));
        assert!(FALLBACK_INDEX.contains("/api/logs"));
    }

    #[tokio::test]
    async fn server_serves_index_and_logs() {
        // Bind on an ephemeral port and hit two endpoints over a raw socket.
        let root = std::env::temp_dir().join("koda-webui-live");
        std::fs::create_dir_all(&root).ok();
        let addr = start(root, 0, "medium".into()).await.expect("bind");

        async fn get(addr: std::net::SocketAddr, path: &str) -> String {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
            s.write_all(req.as_bytes()).await.unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf).to_string()
        }

        let index = get(addr, "/").await;
        assert!(index.contains("200 OK"));
        assert!(index.to_lowercase().contains("<html"));

        let logs = get(addr, "/api/logs?since=0").await;
        assert!(logs.contains("200 OK"));
        assert!(logs.contains("\"entries\""));

        let missing = get(addr, "/nope").await;
        assert!(missing.contains("404 Not Found"));
    }
}
