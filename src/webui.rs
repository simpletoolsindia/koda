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
        ("DELETE", p) if p.starts_with("/api/skills/") => {
            let name = url_decode(p.trim_start_matches("/api/skills/"));
            let json = delete_skill(&ctx.root, &name);
            ("200 OK", "application/json", json.into_bytes())
        }
        ("GET", "/api/settings") => {
            let json = settings_json(&ctx.root);
            ("200 OK", "application/json", json.into_bytes())
        }
        ("POST", "/api/settings") => {
            let json = save_settings(&ctx.root, body);
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

/// Minimal percent-decoder for a single URL path segment (handles `%XX` and
/// `+`). Enough for skill names in a `DELETE /api/skills/<name>` path.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Delete a skill or role agent by name. The file removed is the `source` of the
/// loaded skill, which is guaranteed to live under one of the known skills dirs
/// (user config or `<root>/.koda/skills`) — so a crafted name cannot escape to
/// delete an arbitrary file. Returns `{ok, path}` or `{ok:false, error}`.
fn delete_skill(root: &Path, name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return serde_json::json!({ "ok": false, "error": "name is required" }).to_string();
    }
    let allowed = crate::skills::dirs(root);
    let skills = crate::skills::load(root);
    let Some(skill) = skills.into_iter().find(|s| s.name == name) else {
        return serde_json::json!({ "ok": false, "error": format!("no skill named {name:?}") })
            .to_string();
    };
    // Defence in depth: the file we delete must sit inside an allowed dir.
    let target = skill.source;
    let inside = allowed.iter().any(|d| {
        match (std::fs::canonicalize(d), std::fs::canonicalize(&target)) {
            (Ok(dd), Ok(tt)) => tt.starts_with(&dd),
            // Fall back to a lexical check if canonicalize fails (e.g. dir gone).
            _ => target.starts_with(d),
        }
    });
    if !inside {
        return serde_json::json!({
            "ok": false,
            "error": "refusing to delete a file outside the skills directories"
        })
        .to_string();
    }
    match std::fs::remove_file(&target) {
        Ok(()) => serde_json::json!({ "ok": true, "path": target.display().to_string() })
            .to_string(),
        Err(e) => {
            serde_json::json!({ "ok": false, "error": format!("delete: {e}") }).to_string()
        }
    }
}

/// Settings surfaced to the web UI. Currently the system prompt: the effective
/// custom prompt (empty means "use the built-in"), plus the built-in default so
/// the UI can show and let the user start from it.
fn settings_json(root: &Path) -> String {
    let cfg = crate::config::Config::load(root).unwrap_or_default();
    serde_json::json!({
        "system_prompt": cfg.system_prompt,
        "builtin_prompt": crate::prompt::base_prompt(),
        "using_builtin": cfg.system_prompt.trim().is_empty(),
        "config_path": crate::config::config_path().display().to_string(),
    })
    .to_string()
}

/// Update the system prompt from `{ "system_prompt": "..." }`. Saving the
/// built-in text verbatim (or empty) resets to the built-in. Persisted to the
/// user config; a running koda picks it up on next start (the UI notes this).
fn save_settings(root: &Path, body: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return serde_json::json!({ "ok": false, "error": format!("bad json: {e}") })
                .to_string()
        }
    };
    let prompt = v
        .get("system_prompt")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let mut cfg = match crate::config::Config::load(root) {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({ "ok": false, "error": format!("load config: {e}") })
                .to_string()
        }
    };
    // Storing the unchanged built-in (or empty) means "use the built-in".
    cfg.system_prompt = if prompt.trim().is_empty()
        || prompt.trim() == crate::prompt::base_prompt().trim()
    {
        String::new()
    } else {
        prompt
    };
    match crate::config::save(&cfg) {
        Ok(path) => serde_json::json!({
            "ok": true,
            "path": path.display().to_string(),
            "using_builtin": cfg.system_prompt.trim().is_empty(),
            "note": "saved — a running koda applies it on next start"
        })
        .to_string(),
        Err(e) => serde_json::json!({ "ok": false, "error": format!("save: {e}") }).to_string(),
    }
}

/// The full React UI, embedded at compile time so the installed binary serves
/// it no matter which directory koda runs in. A dev checkout can still override
/// it with a freshly rebuilt file on disk (see `load_index`).
const EMBEDDED_INDEX: &str = include_str!("../web-ui/dist/index.html");

/// Load the built React app: prefer a dist on disk (so `web-ui/build.sh` during
/// development is picked up without recompiling), else the copy embedded in the
/// binary, else — only if the embed is somehow empty — the tiny fallback page.
fn load_index(root: &Path) -> String {
    for candidate in [
        root.join("web-ui").join("dist").join("index.html"),
        PathBuf::from("web-ui/dist/index.html"),
    ] {
        if let Ok(html) = std::fs::read_to_string(&candidate) {
            return html;
        }
    }
    if !EMBEDDED_INDEX.trim().is_empty() {
        return EMBEDDED_INDEX.to_string();
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
    fn url_decode_handles_percent_and_plus() {
        assert_eq!(url_decode("plain"), "plain");
        assert_eq!(url_decode("a%2Db"), "a-b");
        assert_eq!(url_decode("hello+world"), "hello world");
        assert_eq!(url_decode("rust%2Derror"), "rust-error");
    }

    #[test]
    fn delete_skill_round_trips_and_is_sandboxed() {
        let root = std::env::temp_dir().join(format!("koda-del-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        // Create a project skill via the same path save_skill uses.
        let created = save_skill(
            &root,
            r#"{"name":"temp-skill","when":"testing","body":"just a test"}"#,
        );
        assert!(created.contains("\"ok\":true"), "{created}");

        // Deleting an unknown name is a clean, safe error.
        let missing = delete_skill(&root, "does-not-exist");
        assert!(missing.contains("\"ok\":false"), "{missing}");

        // Deleting the real one succeeds and removes the file.
        let removed = delete_skill(&root, "temp-skill");
        assert!(removed.contains("\"ok\":true"), "{removed}");
        assert!(
            crate::skills::load(&root).iter().all(|s| s.name != "temp-skill"),
            "skill should be gone after delete"
        );

        // A second delete now reports it's gone (not a panic, not a stray write).
        let again = delete_skill(&root, "temp-skill");
        assert!(again.contains("\"ok\":false"), "{again}");
        std::fs::remove_dir_all(&root).ok();
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

    #[test]
    fn settings_json_reports_builtin_by_default() {
        let root = std::env::temp_dir().join(format!("koda-settings-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let json = settings_json(&root);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // A fresh project uses the built-in prompt and exposes it to the UI.
        assert_eq!(v["using_builtin"], serde_json::Value::Bool(true));
        assert!(v["builtin_prompt"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn save_settings_rejects_bad_json() {
        let root = std::env::temp_dir().join("koda-settings-badjson");
        let out = save_settings(&root, "not json");
        assert!(out.contains("\"ok\":false"), "{out}");
    }

    #[tokio::test]
    async fn server_gets_and_posts_system_prompt() {
        // Isolate the global config dir so the POST never touches the real user
        // config. (config::save writes to XDG_CONFIG_HOME/koda/config.toml.)
        let cfg_home = std::env::temp_dir().join(format!("koda-xdg-{}", std::process::id()));
        std::fs::remove_dir_all(&cfg_home).ok();
        std::fs::create_dir_all(&cfg_home).ok();
        std::env::set_var("XDG_CONFIG_HOME", &cfg_home);

        let root = std::env::temp_dir().join(format!("koda-settings-http-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).ok();
        let addr = start(root.clone(), 0, "medium".into()).await.expect("bind");

        async fn req(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> String {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            let r = format!(
                "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(r.as_bytes()).await.unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf).to_string()
        }

        // GET exposes the built-in prompt.
        let got = req(addr, "GET", "/api/settings", "").await;
        assert!(got.contains("200 OK"), "{got}");
        assert!(got.contains("\"using_builtin\":true"), "{got}");

        // POST a custom prompt, then GET reflects it as custom.
        let posted = req(
            addr,
            "POST",
            "/api/settings",
            r#"{"system_prompt":"You are a terse code reviewer."}"#,
        )
        .await;
        assert!(posted.contains("\"ok\":true"), "{posted}");
        let got2 = req(addr, "GET", "/api/settings", "").await;
        assert!(got2.contains("terse code reviewer"), "{got2}");
        assert!(got2.contains("\"using_builtin\":false"), "{got2}");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&cfg_home).ok();
    }

    #[tokio::test]
    async fn server_deletes_a_skill_over_http() {
        // Seed a project skill, then delete it through the live DELETE route.
        let root = std::env::temp_dir().join(format!("koda-webui-del-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let created = save_skill(&root, r#"{"name":"web-temp","when":"t","body":"b"}"#);
        assert!(created.contains("\"ok\":true"), "{created}");

        let addr = start(root.clone(), 0, "medium".into()).await.expect("bind");

        async fn req(addr: std::net::SocketAddr, method: &str, path: &str) -> String {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            let r = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
            s.write_all(r.as_bytes()).await.unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf).to_string()
        }

        // The skill is listed…
        let before = req(addr, "GET", "/api/skills").await;
        assert!(before.contains("web-temp"), "skill should be listed: {before}");

        // …DELETE removes it (200 + ok:true)…
        let del = req(addr, "DELETE", "/api/skills/web-temp").await;
        assert!(del.contains("200 OK"), "{del}");
        assert!(del.contains("\"ok\":true"), "{del}");

        // …and it's gone from the listing and disk.
        let after = req(addr, "GET", "/api/skills").await;
        assert!(!after.contains("web-temp"), "skill should be gone: {after}");
        assert!(
            crate::skills::load(&root).iter().all(|s| s.name != "web-temp"),
            "skill file should be removed"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
