//! A tiny local web server for the koda web control center.
//!
//! No web framework: koda stays dependency-light, so this is a minimal HTTP/1.1
//! server on raw tokio. It binds to 127.0.0.1 only and serves:
//!
//!   GET  /                        the React UI (from `web-ui/dist/` if built,
//!                                 otherwise the copy embedded in the binary)
//!   GET  /api/logs?since=&lvl=    JSON log entries (live agent telemetry)
//!   GET  /api/events              Server-Sent Events: log + trace snapshots
//!   GET  /api/trace               turn summaries, newest first, + the live turn
//!   DELETE /api/trace             drop the trace ring
//!   GET  /api/trace/<id>          one turn with every step payload
//!   GET  /api/debug               captured raw request/response sessions
//!   GET  /api/codegraph           the project symbol graph as nodes + edges
//!   GET  /api/codegraph/symbol    one symbol: definition and cross-file users
//!   GET  /api/skills              skills and role agents (name/when/role/body)
//!   POST /api/skills              create/update a skill or role agent
//!   DELETE /api/skills/<name>     remove a skill
//!   GET  /api/settings            system prompt (built-in + override)
//!   POST /api/settings            replace the system prompt
//!   GET  /api/config              live-editable runtime configuration
//!   POST /api/config              validate, persist, and apply it live
//!   GET  /api/memory              project memory (notes, commands, hot files)
//!   POST /api/memory              remember / forget a note
//!   GET  /api/learning            accepted rules and pending candidates
//!   POST /api/learning            accept / reject a candidate
//!   GET  /api/sessions            saved sessions, newest first
//!   POST /api/sessions/<id>/resume|fork
//!
//! Writes that only the running agent can make (config, memory, learning,
//! resume) are queued as `Control` requests; the TUI drains that queue on its
//! own loop and dispatches the matching `agent::Command`, so a change from the
//! browser takes effect in the live session rather than only after a restart.

use crate::log::{self, Level};
use serde::Serialize;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A change requested from the browser that must be applied by the running
/// agent, not by writing a file. Drained by the TUI loop.
#[derive(Debug)]
pub enum Control {
    /// Adopt an edited config live (model, mode, autonomy, toggles…).
    Config(Box<crate::config::Config>),
    /// Accept or reject a learned rule candidate.
    Learn(crate::agent::LearnAction),
    /// Add a durable project note.
    Remember(String),
    /// Drop notes matching a substring.
    Forget(String),
    /// Load a saved session in place of the current one.
    Resume(PathBuf),
}

fn control_queue() -> &'static Mutex<VecDeque<Control>> {
    static Q: OnceLock<Mutex<VecDeque<Control>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn push_control(c: Control) {
    if let Ok(mut q) = control_queue().lock() {
        // Bound it: a browser tab spamming requests must not grow memory.
        if q.len() > 64 {
            q.pop_front();
        }
        q.push_back(c);
    }
}

/// Take everything the browser has asked for since the last call. Cheap enough
/// to poll on a timer: an idle queue is one uncontended lock.
pub fn take_control() -> Vec<Control> {
    match control_queue().lock() {
        Ok(mut q) => q.drain(..).collect(),
        Err(_) => Vec::new(),
    }
}

/// Whether anything is waiting, so the poll can skip the drain entirely.
pub fn has_control() -> bool {
    control_queue()
        .lock()
        .map(|q| !q.is_empty())
        .unwrap_or(false)
}

/// What the *running* session is actually using. The config file is not the
/// truth: `-m`, `/model`, `/mode` and the settings overlay all change the live
/// agent without necessarily being saved. The TUI publishes this so the control
/// rail shows what koda is doing right now.
#[derive(Debug, Clone, Default)]
struct Runtime {
    model: String,
    endpoint: String,
    mode: String,
    auto_tier: String,
}

fn runtime_slot() -> &'static Mutex<Option<Runtime>> {
    static SLOT: OnceLock<Mutex<Option<Runtime>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Publish the live values. Cheap to call on a timer: it only writes when
/// something actually changed.
pub fn publish_runtime(model: &str, endpoint: &str, mode: &str, auto_tier: &str) {
    let Ok(mut slot) = runtime_slot().lock() else {
        return;
    };
    let same = slot.as_ref().is_some_and(|r| {
        r.model == model && r.endpoint == endpoint && r.mode == mode && r.auto_tier == auto_tier
    });
    if same {
        return;
    }
    *slot = Some(Runtime {
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        mode: mode.to_string(),
        auto_tier: auto_tier.to_string(),
    });
}

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

/// Largest request we will read. Generous enough for a pasted system prompt,
/// bounded so a runaway client cannot make us allocate without limit.
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;

async fn handle(mut stream: TcpStream, ctx: Arc<Ctx>) -> std::io::Result<()> {
    // Read until the head is complete, then until `Content-Length` bytes of body
    // have arrived. A single read() is not enough: TCP is free to split a POST
    // into a head segment and a body segment, which would silently truncate the
    // body (and closing with unread bytes still in the socket makes the kernel
    // send RST, losing the response we just wrote).
    let mut raw: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let head_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(()); // client hung up before sending a request
        }
        raw.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_head_end(&raw) {
            break pos;
        }
        if raw.len() > MAX_REQUEST_BYTES {
            return write_response(
                &mut stream,
                "431 Request Header Fields Too Large",
                "application/json",
                br#"{"error":"request head too large"}"#,
            )
            .await;
        }
    };

    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let want = content_length(&head).min(MAX_REQUEST_BYTES);
    while raw.len() - head_end < want {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break; // truncated body; the handler will reject it
        }
        raw.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&raw[head_end..]).to_string();

    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };

    let (status, ctype, payload) = route(method, path, query, &body, &ctx).await;
    write_response(&mut stream, status, ctype, &payload).await
}

/// Index just past the blank line that ends the request head, if it's there yet.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        // Tolerate bare-LF clients (curl --http0.9, hand-typed requests).
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

fn content_length(head: &str) -> usize {
    head.split("\r\n")
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0)
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
            // A one-shot snapshot framed as SSE, carrying both streams the UI
            // follows. The client reconnects/polls; keeping the connection
            // stateless avoids a long-lived task per tab.
            let logs = logs_json(query, &ctx.detail);
            let trace = trace_json();
            let framed = format!("event: logs\ndata: {logs}\n\nevent: trace\ndata: {trace}\n\n");
            ("200 OK", "text/event-stream", framed.into_bytes())
        }
        ("GET", "/api/trace") => ("200 OK", "application/json", trace_json().into_bytes()),
        ("DELETE", "/api/trace") => {
            crate::trace::clear();
            ("200 OK", "application/json", br#"{"ok":true}"#.to_vec())
        }
        ("GET", p) if p.starts_with("/api/trace/") => {
            let id: Option<u64> = p.trim_start_matches("/api/trace/").parse().ok();
            match id.and_then(crate::trace::turn) {
                Some(t) => (
                    "200 OK",
                    "application/json",
                    serde_json::to_string(&t)
                        .unwrap_or_else(|_| "{}".into())
                        .into_bytes(),
                ),
                None => (
                    "404 Not Found",
                    "application/json",
                    br#"{"error":"no such turn"}"#.to_vec(),
                ),
            }
        }
        ("GET", "/api/debug") => {
            let json = debug_json();
            ("200 OK", "application/json", json.into_bytes())
        }
        ("GET", "/api/codegraph") => {
            let json = codegraph_json(&ctx.root);
            ("200 OK", "application/json", json.into_bytes())
        }
        ("GET", "/api/codegraph/symbol") => {
            let name = url_decode(query_param(query, "name").unwrap_or(""));
            let json = symbol_json(&ctx.root, &name);
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
        ("GET", "/api/config") => (
            "200 OK",
            "application/json",
            config_json(&ctx.root).into_bytes(),
        ),
        ("POST", "/api/config") => {
            let json = save_config(&ctx.root, body);
            ("200 OK", "application/json", json.into_bytes())
        }
        ("GET", "/api/memory") => (
            "200 OK",
            "application/json",
            memory_json(&ctx.root).into_bytes(),
        ),
        ("POST", "/api/memory") => ("200 OK", "application/json", post_memory(body).into_bytes()),
        ("GET", "/api/learning") => (
            "200 OK",
            "application/json",
            learning_json(&ctx.root).into_bytes(),
        ),
        ("POST", "/api/learning") => (
            "200 OK",
            "application/json",
            post_learning(body).into_bytes(),
        ),
        ("GET", "/api/sessions") => (
            "200 OK",
            "application/json",
            sessions_json(&ctx.root).into_bytes(),
        ),
        ("POST", p) if p.starts_with("/api/sessions/") => {
            let rest = p.trim_start_matches("/api/sessions/");
            let (id, action) = rest.split_once('/').unwrap_or((rest, "resume"));
            let json = session_action(&ctx.root, &url_decode(id), action);
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
    let since: u64 = query_param(query, "since")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
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

/// The turn rail plus the live turn in full, so a browser that polls this one
/// endpoint can render the whole console without a second request.
fn trace_json() -> String {
    let turns = crate::trace::summaries();
    let live = crate::trace::live();
    serde_json::json!({
        "enabled": crate::trace::enabled(),
        "version": crate::trace::version(),
        "turns": turns,
        "live": live,
    })
    .to_string()
}

/// Scanning a repo is not free, so both graph endpoints share one short-lived
/// cache. 30s keeps an interactive symbol lookup instant while still reflecting
/// edits made while you work.
fn cached_graph(root: &Path) -> Arc<crate::graph::Graph> {
    type Cache = Mutex<Option<(std::time::Instant, PathBuf, Arc<crate::graph::Graph>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some((at, path, g)) = guard.as_ref() {
            if path == root && at.elapsed() < std::time::Duration::from_secs(30) {
                return g.clone();
            }
        }
    }
    let g = Arc::new(crate::graph::scan(root));
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((std::time::Instant::now(), root.to_path_buf(), g.clone()));
    }
    g
}

/// One symbol: where it is defined and which files use it. This is the same
/// report the `codegraph` tool gives the model, so the UI and the agent agree.
fn symbol_json(root: &Path, name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return serde_json::json!({ "ok": false, "error": "name is required" }).to_string();
    }
    let g = cached_graph(root);
    let report = g.symbol(name);
    let defs: Vec<serde_json::Value> = g
        .defs
        .get(name)
        .map(|ds| {
            ds.iter()
                .map(|d| {
                    serde_json::json!({
                        "kind": d.kind.to_string(),
                        "file": d.file,
                        "line": d.line,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let refs: Vec<String> = g
        .refs
        .get(name)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default();
    serde_json::json!({
        "ok": !defs.is_empty() || !refs.is_empty(),
        "name": name,
        "report": report,
        "defs": defs,
        "refs": refs,
    })
    .to_string()
}

/// The runtime configuration the control rail may change. A curated subset on
/// purpose: the API never exposes `api_key`, and never lets the browser rewrite
/// fields (paths, shells, limits) that would be a foot-gun from a web form.
fn config_json(root: &Path) -> String {
    let cfg = crate::config::Config::load(root).unwrap_or_default();
    // Prefer what the live session is using; fall back to the file for a koda
    // that isn't running a TUI (or hasn't published yet).
    let live = runtime_slot().lock().ok().and_then(|s| s.clone());
    let (model, base_url, mode, tier) = match live {
        Some(r) => (r.model, r.endpoint, r.mode, r.auto_tier),
        None => (
            cfg.model.clone(),
            cfg.base_url.clone(),
            cfg.mode.to_string(),
            cfg.auto_tier.to_string(),
        ),
    };
    serde_json::json!({
        "model": model,
        "base_url": base_url,
        "mode": mode,
        "auto_tier": tier,
        "reasoning_effort": cfg.reasoning_effort,
        "temperature": cfg.temperature,
        "max_steps": cfg.max_steps,
        "toggles": {
            "learning": cfg.learning,
            "memory": cfg.memory,
            "codegraph": cfg.codegraph,
            "web_search": cfg.web_search,
            "web_fetch": cfg.web_fetch,
            "subagents": cfg.subagents,
            "sessions": cfg.sessions,
            "debug": cfg.debug,
            "watch": cfg.watch,
        },
        "has_api_key": !cfg.api_key.trim().is_empty(),
        "config_path": crate::config::config_path().display().to_string(),
        "modes": ["plan", "execute", "vibe"],
        "tiers": ["ask", "write", "full"],
        "efforts": ["off", "low", "medium", "high"],
    })
    .to_string()
}

/// Apply an edited config: validate every field, persist it, and queue it for
/// the running agent. Anything invalid is rejected whole — a half-applied config
/// is worse than a rejected one.
fn save_config(root: &Path, body: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return serde_json::json!({ "ok": false, "error": format!("bad json: {e}") })
                .to_string()
        }
    };
    let mut cfg = match crate::config::Config::load(root) {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({ "ok": false, "error": format!("load config: {e}") })
                .to_string()
        }
    };
    let mut errors: Vec<String> = Vec::new();

    if let Some(m) = v.get("model") {
        match m.as_str().map(str::trim) {
            Some(s) if !s.is_empty() => cfg.model = s.to_string(),
            _ => errors.push("model must be a non-empty string".into()),
        }
    }
    if let Some(u) = v.get("base_url") {
        match u.as_str().map(str::trim) {
            Some(s) if s.starts_with("http://") || s.starts_with("https://") => {
                cfg.base_url = s.trim_end_matches('/').to_string()
            }
            _ => errors.push("base_url must start with http:// or https://".into()),
        }
    }
    if let Some(m) = v.get("mode") {
        match m.as_str().unwrap_or("").parse::<crate::config::Mode>() {
            Ok(mode) => cfg.mode = mode,
            Err(e) => errors.push(e),
        }
    }
    if let Some(t) = v.get("auto_tier") {
        match t.as_str().unwrap_or("").parse::<crate::config::AutoTier>() {
            Ok(tier) => {
                cfg.auto_tier = tier;
                // `auto_approve` is the hard override; keep it consistent with
                // the tier so the two can never disagree.
                cfg.auto_approve = tier == crate::config::AutoTier::Full;
            }
            Err(e) => errors.push(e),
        }
    }
    if let Some(r) = v.get("reasoning_effort") {
        let s = r.as_str().unwrap_or("").trim().to_ascii_lowercase();
        if matches!(s.as_str(), "off" | "low" | "medium" | "high") {
            cfg.reasoning_effort = s;
        } else {
            errors.push("reasoning_effort must be off|low|medium|high".into());
        }
    }
    if let Some(t) = v.get("temperature") {
        match t.as_f64() {
            Some(f) if (0.0..=2.0).contains(&f) => cfg.temperature = f,
            _ => errors.push("temperature must be a number between 0 and 2".into()),
        }
    }
    if let Some(s) = v.get("max_steps") {
        match s.as_u64() {
            Some(n) if (1..=500).contains(&n) => cfg.max_steps = n as usize,
            _ => errors.push("max_steps must be between 1 and 500".into()),
        }
    }
    if let Some(toggles) = v.get("toggles") {
        let set = |key: &str, slot: &mut bool, errors: &mut Vec<String>| {
            if let Some(x) = toggles.get(key) {
                match x.as_bool() {
                    Some(b) => *slot = b,
                    None => errors.push(format!("{key} must be true or false")),
                }
            }
        };
        set("learning", &mut cfg.learning, &mut errors);
        set("memory", &mut cfg.memory, &mut errors);
        set("codegraph", &mut cfg.codegraph, &mut errors);
        set("web_search", &mut cfg.web_search, &mut errors);
        set("web_fetch", &mut cfg.web_fetch, &mut errors);
        set("subagents", &mut cfg.subagents, &mut errors);
        set("sessions", &mut cfg.sessions, &mut errors);
        set("debug", &mut cfg.debug, &mut errors);
        set("watch", &mut cfg.watch, &mut errors);
    }

    if !errors.is_empty() {
        return serde_json::json!({ "ok": false, "error": errors.join("; ") }).to_string();
    }
    // Debug capture is a process-global switch, so it can take effect here.
    crate::debug::set_enabled(cfg.debug);
    let saved = crate::config::save(&cfg);
    push_control(Control::Config(Box::new(cfg)));
    match saved {
        Ok(path) => serde_json::json!({
            "ok": true,
            "path": path.display().to_string(),
            "note": "applied to the running session and saved"
        })
        .to_string(),
        // The live session still gets the change; only persistence failed.
        Err(e) => serde_json::json!({
            "ok": true,
            "warning": format!("applied live but could not save: {e}")
        })
        .to_string(),
    }
}

fn memory_json(root: &Path) -> String {
    let m = crate::memory::Memory::load(root);
    let commands: Vec<serde_json::Value> = m
        .commands
        .iter()
        .map(|(cmd, (ok, fail))| serde_json::json!({ "command": cmd, "ok": ok, "failed": fail }))
        .collect();
    let files: Vec<serde_json::Value> = m
        .hot_files
        .iter()
        .map(|(path, n)| serde_json::json!({ "path": path, "edits": n }))
        .collect();
    serde_json::json!({
        "notes": m.notes,
        "commands": commands,
        "hot_files": files,
        "path": crate::memory::path(root).display().to_string(),
    })
    .to_string()
}

/// `{ "remember": "..." }` or `{ "forget": "substring" }`. Routed through the
/// agent so the note is in the system prompt for the very next turn.
fn post_memory(body: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return serde_json::json!({ "ok": false, "error": format!("bad json: {e}") })
                .to_string()
        }
    };
    if let Some(note) = v.get("remember").and_then(|x| x.as_str()) {
        let note = note.trim();
        if note.is_empty() {
            return serde_json::json!({ "ok": false, "error": "note is empty" }).to_string();
        }
        push_control(Control::Remember(note.to_string()));
        return serde_json::json!({ "ok": true, "note": "remembered" }).to_string();
    }
    if let Some(needle) = v.get("forget").and_then(|x| x.as_str()) {
        let needle = needle.trim();
        if needle.is_empty() {
            return serde_json::json!({ "ok": false, "error": "nothing to forget" }).to_string();
        }
        push_control(Control::Forget(needle.to_string()));
        return serde_json::json!({ "ok": true, "note": "forgotten" }).to_string();
    }
    serde_json::json!({ "ok": false, "error": "expected `remember` or `forget`" }).to_string()
}

fn learning_json(root: &Path) -> String {
    let l = crate::learning::Learning::load(root);
    let rule = |r: &crate::learning::Rule| {
        serde_json::json!({
            "key": r.key,
            "text": r.text,
            "support": r.support,
            "accepted": r.accepted,
        })
    };
    let accepted: Vec<serde_json::Value> =
        l.rules.iter().filter(|r| r.accepted).map(rule).collect();
    let candidates: Vec<serde_json::Value> = l.candidates().into_iter().map(rule).collect();
    serde_json::json!({
        "accepted": accepted,
        "candidates": candidates,
        "brief": l.brief(),
    })
    .to_string()
}

/// `{ "accept": 1 }`, `{ "accept": "all" }` or `{ "reject": 2 }` — 1-based, the
/// same indices `/learn` uses and the same indices this endpoint lists.
fn post_learning(body: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return serde_json::json!({ "ok": false, "error": format!("bad json: {e}") })
                .to_string()
        }
    };
    use crate::agent::LearnAction;
    if let Some(a) = v.get("accept") {
        let action = match a {
            serde_json::Value::String(s) if s == "all" => LearnAction::Accept(None),
            other => match other.as_u64() {
                Some(n) if n >= 1 => LearnAction::Accept(Some(n as usize)),
                _ => {
                    return serde_json::json!({
                        "ok": false,
                        "error": "accept takes a 1-based index or \"all\""
                    })
                    .to_string()
                }
            },
        };
        push_control(Control::Learn(action));
        return serde_json::json!({ "ok": true }).to_string();
    }
    if let Some(n) = v.get("reject").and_then(|x| x.as_u64()) {
        if n < 1 {
            return serde_json::json!({ "ok": false, "error": "reject takes a 1-based index" })
                .to_string();
        }
        push_control(Control::Learn(LearnAction::Reject(n as usize)));
        return serde_json::json!({ "ok": true }).to_string();
    }
    serde_json::json!({ "ok": false, "error": "expected `accept` or `reject`" }).to_string()
}

fn sessions_json(root: &Path) -> String {
    let list: Vec<serde_json::Value> = crate::session::list(root)
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "id": s.header.id,
                "model": s.header.model,
                "endpoint": s.header.endpoint,
                "started": s.header.started,
                "messages": s.messages,
                "title": s.title,
                "modified": s.modified,
                "ago": crate::session::ago(s.modified),
                "path": s.path.display().to_string(),
            })
        })
        .collect();
    serde_json::json!({ "sessions": list }).to_string()
}

/// `resume` swaps the live conversation for a saved one; `fork` copies it first
/// so the original stays untouched, then resumes the copy.
fn session_action(root: &Path, id: &str, action: &str) -> String {
    let Some(found) = crate::session::list(root)
        .into_iter()
        .find(|s| s.header.id == id)
    else {
        return serde_json::json!({ "ok": false, "error": format!("no session {id}") }).to_string();
    };
    match action {
        "resume" => {
            push_control(Control::Resume(found.path.clone()));
            serde_json::json!({ "ok": true, "resumed": id }).to_string()
        }
        "fork" => match crate::session::fork(&found.path, root) {
            Ok(path) => {
                let new_id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                push_control(Control::Resume(path.clone()));
                serde_json::json!({ "ok": true, "forked": new_id,
                                    "path": path.display().to_string() })
                .to_string()
            }
            Err(e) => serde_json::json!({ "ok": false, "error": format!("fork: {e}") }).to_string(),
        },
        other => serde_json::json!({
            "ok": false,
            "error": format!("unknown action {other:?} (resume|fork)")
        })
        .to_string(),
    }
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
    let g = cached_graph(root);
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
            edges.push(GraphEdge {
                from: d.file.clone(),
                to: name.clone(),
                kind: "defines",
            });
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
        Err(e) => {
            return serde_json::json!({ "ok": false, "error": format!("bad json: {e}") })
                .to_string()
        }
    };
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let when = v
        .get("when")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let bodytext = v
        .get("body")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let role = v
        .get("role")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if name.is_empty() || when.is_empty() || bodytext.is_empty() {
        return serde_json::json!({ "ok": false, "error": "name, when and body are required" })
            .to_string();
    }
    let front_role = role
        .as_ref()
        .map(|r| format!("role: {r}\n"))
        .unwrap_or_default();
    let doc = format!("---\nname: {name}\n{front_role}when: {when}\n---\n\n{bodytext}\n");
    if crate::skills::parse(&doc).is_none() {
        return serde_json::json!({ "ok": false, "error": "composed skill did not parse" })
            .to_string();
    }
    let dir = root.join(".koda").join("skills");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return serde_json::json!({ "ok": false, "error": format!("mkdir: {e}") }).to_string();
    }
    // Sanitize the filename from the name.
    let file: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
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
        Ok(()) => {
            serde_json::json!({ "ok": true, "path": target.display().to_string() }).to_string()
        }
        Err(e) => serde_json::json!({ "ok": false, "error": format!("delete: {e}") }).to_string(),
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
    cfg.system_prompt =
        if prompt.trim().is_empty() || prompt.trim() == crate::prompt::base_prompt().trim() {
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
    stream.flush().await?;
    // Half-close so the peer gets a FIN after the full body. Dropping the socket
    // with anything still unread would make the kernel send RST instead, which
    // can discard the response the client is mid-read of.
    let _ = stream.shutdown().await;
    Ok(())
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

    /// Serializes the tests that touch process-global state: `XDG_CONFIG_HOME`
    /// (which `Config::load` and `config::save` resolve on every call) and the
    /// control queue. Both are one-per-process, so tests using them cannot run
    /// concurrently — one test's `remove_var` lands in the middle of another's
    /// save, and a queue assertion sees a neighbour's push. The guard is
    /// poison-tolerant: a panicking test has already failed, and re-reporting
    /// its poison as a failure in every other test hides the real one.
    fn global_state_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

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
            crate::skills::load(&root)
                .iter()
                .all(|s| s.name != "temp-skill"),
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

    #[test]
    fn request_head_and_body_are_parsed_independently() {
        // The head isn't complete until the blank line arrives.
        assert_eq!(find_head_end(b"POST / HTTP/1.1\r\nHost: x\r\n"), None);
        assert_eq!(
            find_head_end(b"POST / HTTP/1.1\r\nHost: x\r\n\r\nbody"),
            Some(28)
        );
        // Bare-LF clients still work.
        assert!(find_head_end(b"GET / HTTP/1.1\nHost: x\n\n").is_some());
        // Content-Length is case-insensitive and defaults to zero.
        assert_eq!(
            content_length("POST / HTTP/1.1\r\nContent-Length: 42\r\n"),
            42
        );
        assert_eq!(
            content_length("POST / HTTP/1.1\r\ncontent-length:  7 \r\n"),
            7
        );
        assert_eq!(content_length("GET / HTTP/1.1\r\nHost: x\r\n"), 0);
        assert_eq!(
            content_length("POST / HTTP/1.1\r\nContent-Length: nonsense\r\n"),
            0
        );
    }

    #[tokio::test]
    // Holding the guard across the awaits is the point: the test owns the
    // global config env for its whole body, not just up to the first await.
    #[allow(clippy::await_holding_lock)]
    async fn a_post_split_across_packets_is_read_whole() {
        let _env = global_state_lock();
        // TCP may deliver the head and body separately. Before this was handled,
        // the body was silently truncated and closing the socket with unread
        // bytes reset the connection, losing the response.
        let cfg_home = std::env::temp_dir().join(format!("koda-xdg-split-{}", std::process::id()));
        std::fs::remove_dir_all(&cfg_home).ok();
        std::fs::create_dir_all(&cfg_home).ok();
        std::env::set_var("XDG_CONFIG_HOME", &cfg_home);
        // This test POSTs an 80 KB system prompt, and the handler saves it. If
        // the env var is not in force the save lands in the developer's real
        // config, where 80 KB of "You are a reviewer." costs 20k tokens on every
        // message thereafter and nothing says so. That happened. Fail loudly
        // here rather than quietly there.
        assert!(
            crate::config::config_path().starts_with(&cfg_home),
            "refusing to run: config_path() is {}, not under {}",
            crate::config::config_path().display(),
            cfg_home.display()
        );
        let root = std::env::temp_dir().join(format!("koda-split-{}", std::process::id()));
        std::fs::create_dir_all(&root).ok();
        let addr = start(root.clone(), 0, "medium".into()).await.expect("bind");

        // A body big enough that it would never share a segment with the head.
        let prompt = "You are a reviewer. ".repeat(4000);
        let body = serde_json::json!({ "system_prompt": prompt }).to_string();
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "POST /api/settings HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        s.write_all(head.as_bytes()).await.unwrap();
        s.flush().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        s.write_all(body.as_bytes()).await.unwrap();
        s.flush().await.unwrap();

        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("200 OK"), "{text}");
        assert!(text.contains("\"ok\":true"), "{text}");
        // The whole body was applied, not a truncated prefix.
        let saved = crate::config::Config::load(&root).unwrap();
        assert_eq!(saved.system_prompt.len(), prompt.len());

        std::env::remove_var("XDG_CONFIG_HOME");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&cfg_home).ok();
    }

    #[test]
    fn config_post_rejects_bad_input_and_changes_nothing() {
        let _env = global_state_lock();
        let root = std::env::temp_dir().join(format!("koda-cfg-bad-{}", std::process::id()));
        std::fs::create_dir_all(&root).ok();
        // Each of these is individually invalid and must be refused.
        for body in [
            r#"{"model":""}"#,
            r#"{"base_url":"ftp://nope"}"#,
            r#"{"mode":"turbo"}"#,
            r#"{"auto_tier":"whatever"}"#,
            r#"{"reasoning_effort":"extreme"}"#,
            r#"{"temperature":9}"#,
            r#"{"max_steps":0}"#,
            r#"{"toggles":{"memory":"yes"}}"#,
            "not json at all",
        ] {
            let out = save_config(&root, body);
            assert!(out.contains("\"ok\":false"), "{body} → {out}");
        }
        // Nothing was queued for the agent, because nothing was accepted.
        assert!(take_control().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn memory_and_learning_posts_queue_control_requests() {
        let _env = global_state_lock();
        // Start from a clean queue: other tests may have left items.
        let _ = take_control();
        assert!(post_memory(r#"{"remember":"tests run with cargo test"}"#).contains("\"ok\":true"));
        assert!(post_memory(r#"{"forget":"cargo"}"#).contains("\"ok\":true"));
        assert!(post_learning(r#"{"accept":"all"}"#).contains("\"ok\":true"));
        assert!(post_learning(r#"{"reject":2}"#).contains("\"ok\":true"));
        // Malformed requests are refused and queue nothing.
        assert!(post_memory(r#"{"remember":"   "}"#).contains("\"ok\":false"));
        assert!(post_memory(r#"{}"#).contains("\"ok\":false"));
        assert!(post_learning(r#"{"accept":0}"#).contains("\"ok\":false"));
        assert!(post_learning(r#"{}"#).contains("\"ok\":false"));

        let queued = take_control();
        assert_eq!(queued.len(), 4, "only the valid requests queue: {queued:?}");
        assert!(matches!(queued[0], Control::Remember(_)));
        assert!(matches!(queued[1], Control::Forget(_)));
        assert!(matches!(
            queued[2],
            Control::Learn(crate::agent::LearnAction::Accept(None))
        ));
        assert!(matches!(
            queued[3],
            Control::Learn(crate::agent::LearnAction::Reject(2))
        ));
        assert!(take_control().is_empty(), "the queue drains once");
    }

    #[tokio::test]
    async fn trace_endpoints_serve_a_captured_turn() {
        crate::trace::set_enabled(true);
        crate::trace::clear();
        let t = crate::trace::begin_turn("execute", "test-model", "http://x/v1", "trace me");
        let s = crate::trace::open_step(t, crate::trace::StepKind::Model, "test-model");
        crate::trace::append_sse(
            s,
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
        );
        crate::trace::finish_model(
            s,
            crate::trace::ModelCall {
                request: "{\"model\":\"test-model\"}".into(),
                text: "hi".into(),
                ..Default::default()
            },
        );
        let ts = crate::trace::open_step(t, crate::trace::StepKind::Tool, "read_file");
        crate::trace::finish_tool(
            ts,
            crate::trace::ToolStep {
                name: "read_file".into(),
                args: "{\"path\":\"src/main.rs\"}".into(),
                ok: true,
                summary: "read 400 lines".into(),
                approval: Some(crate::trace::Approval::Auto),
                ..Default::default()
            },
        );
        let id = t.expect("tracing is on");
        crate::trace::end_turn(t, crate::trace::Status::Ok, "all done", 1200);

        let root = std::env::temp_dir().join(format!("koda-trace-http-{}", std::process::id()));
        std::fs::create_dir_all(&root).ok();
        let addr = start(root.clone(), 0, "medium".into()).await.expect("bind");

        async fn get(addr: std::net::SocketAddr, path: &str) -> String {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
            s.write_all(req.as_bytes()).await.unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf).to_string()
        }

        // The rail lists the turn with its shape, not its payloads.
        let list = get(addr, "/api/trace").await;
        assert!(list.contains("200 OK"), "{list}");
        assert!(list.contains("trace me"), "{list}");
        assert!(list.contains("\"model_calls\":1"), "{list}");
        assert!(list.contains("\"tool_calls\":1"), "{list}");

        // The detail endpoint returns every payload for that turn.
        let one = get(addr, &format!("/api/trace/{id}")).await;
        assert!(one.contains("200 OK"), "{one}");
        assert!(one.contains("test-model"), "{one}");
        assert!(one.contains("read_file"), "{one}");
        assert!(one.contains("src/main.rs"), "{one}");
        assert!(one.contains("data:"), "raw SSE is served: {one}");
        assert!(one.contains("all done"), "{one}");

        // An unknown turn is a clean 404, not an empty 200.
        let missing = get(addr, "/api/trace/999999").await;
        assert!(missing.contains("404 Not Found"), "{missing}");

        // The SSE snapshot carries both streams the console follows.
        let events = get(addr, "/api/events").await;
        assert!(events.contains("event: logs"), "{events}");
        assert!(events.contains("event: trace"), "{events}");

        crate::trace::clear();
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    // Holding the guard across the awaits is the point: the test owns the
    // global config env for its whole body, not just up to the first await.
    #[allow(clippy::await_holding_lock)]
    async fn control_endpoints_round_trip_over_http() {
        let _env = global_state_lock();
        let cfg_home = std::env::temp_dir().join(format!("koda-xdg-ctl-{}", std::process::id()));
        std::fs::remove_dir_all(&cfg_home).ok();
        std::fs::create_dir_all(&cfg_home).ok();
        std::env::set_var("XDG_CONFIG_HOME", &cfg_home);
        let root = std::env::temp_dir().join(format!("koda-ctl-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).ok();
        let addr = start(root.clone(), 0, "medium".into()).await.expect("bind");

        async fn req(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> String {
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

        let _ = take_control();

        // Config: the key is never exposed, only whether one is set.
        let got = req(addr, "GET", "/api/config", "").await;
        assert!(got.contains("200 OK"), "{got}");
        assert!(got.contains("\"has_api_key\""), "{got}");
        assert!(
            !got.contains("\"api_key\""),
            "the API key must not be served: {got}"
        );

        // A valid edit is accepted, persisted, and queued for the live agent.
        let posted = req(
            addr,
            "POST",
            "/api/config",
            r#"{"mode":"plan","auto_tier":"write","reasoning_effort":"low",
                "toggles":{"codegraph":true,"debug":false}}"#,
        )
        .await;
        assert!(posted.contains("\"ok\":true"), "{posted}");
        let after = req(addr, "GET", "/api/config", "").await;
        assert!(after.contains("\"mode\":\"plan\""), "{after}");
        assert!(after.contains("\"auto_tier\":\"write\""), "{after}");
        let queued = take_control();
        assert!(
            queued.iter().any(|c| matches!(c, Control::Config(_))),
            "the live session gets the change: {queued:?}"
        );

        // An invalid edit is refused with a message naming the field.
        let bad = req(addr, "POST", "/api/config", r#"{"mode":"nope"}"#).await;
        assert!(bad.contains("\"ok\":false"), "{bad}");
        assert!(bad.contains("mode"), "{bad}");

        // Memory and learning read cleanly on an empty project…
        let mem = req(addr, "GET", "/api/memory", "").await;
        assert!(mem.contains("\"notes\""), "{mem}");
        let learn = req(addr, "GET", "/api/learning", "").await;
        assert!(learn.contains("\"candidates\""), "{learn}");
        // …and their writes queue for the agent.
        let remembered = req(
            addr,
            "POST",
            "/api/memory",
            r#"{"remember":"use cargo test"}"#,
        )
        .await;
        assert!(remembered.contains("\"ok\":true"), "{remembered}");
        assert!(take_control()
            .iter()
            .any(|c| matches!(c, Control::Remember(_))));

        // Sessions list (empty here) and an unknown id is a clean error.
        let sessions = req(addr, "GET", "/api/sessions", "").await;
        assert!(sessions.contains("\"sessions\""), "{sessions}");
        let nope = req(addr, "POST", "/api/sessions/does-not-exist/resume", "").await;
        assert!(nope.contains("\"ok\":false"), "{nope}");

        std::env::remove_var("XDG_CONFIG_HOME");
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&cfg_home).ok();
    }

    #[tokio::test]
    async fn symbol_lookup_answers_from_the_graph() {
        // A tiny project so the scan is fast and deterministic.
        let root = std::env::temp_dir().join(format!("koda-sym-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("src")).ok();
        std::fs::write(
            root.join("src").join("lib.rs"),
            "pub fn unique_marker_fn() -> u8 { 7 }\n",
        )
        .ok();
        std::fs::write(
            root.join("src").join("main.rs"),
            "fn main() { let _ = unique_marker_fn(); }\n",
        )
        .ok();

        let found = symbol_json(&root, "unique_marker_fn");
        assert!(found.contains("\"ok\":true"), "{found}");
        assert!(found.contains("lib.rs"), "definition is reported: {found}");

        // A name that isn't there says so rather than pretending.
        let missing = symbol_json(&root, "no_such_symbol_anywhere");
        assert!(missing.contains("\"ok\":false"), "{missing}");
        // An empty query is a clean error, not a full scan dump.
        assert!(symbol_json(&root, "  ").contains("name is required"));
        std::fs::remove_dir_all(&root).ok();
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
        let _env = global_state_lock();
        let root = std::env::temp_dir().join(format!("koda-settings-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        // settings_json merges the global config under the project one, so the
        // global has to be an empty dir of ours rather than the real user
        // config — which may well set a system prompt.
        let cfg_home =
            std::env::temp_dir().join(format!("koda-xdg-builtin-{}", std::process::id()));
        std::fs::remove_dir_all(&cfg_home).ok();
        std::fs::create_dir_all(&cfg_home).ok();
        std::env::set_var("XDG_CONFIG_HOME", &cfg_home);
        let json = settings_json(&root);
        std::env::remove_var("XDG_CONFIG_HOME");
        std::fs::remove_dir_all(&cfg_home).ok();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // A fresh project uses the built-in prompt and exposes it to the UI.
        assert_eq!(v["using_builtin"], serde_json::Value::Bool(true));
        assert!(!v["builtin_prompt"].as_str().unwrap().is_empty());
    }

    #[test]
    fn save_settings_rejects_bad_json() {
        let root = std::env::temp_dir().join("koda-settings-badjson");
        let out = save_settings(&root, "not json");
        assert!(out.contains("\"ok\":false"), "{out}");
    }

    #[tokio::test]
    // Holding the guard across the awaits is the point: the test owns the
    // global config env for its whole body, not just up to the first await.
    #[allow(clippy::await_holding_lock)]
    async fn server_gets_and_posts_system_prompt() {
        let _env = global_state_lock();
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
        assert!(
            before.contains("web-temp"),
            "skill should be listed: {before}"
        );

        // …DELETE removes it (200 + ok:true)…
        let del = req(addr, "DELETE", "/api/skills/web-temp").await;
        assert!(del.contains("200 OK"), "{del}");
        assert!(del.contains("\"ok\":true"), "{del}");

        // …and it's gone from the listing and disk.
        let after = req(addr, "GET", "/api/skills").await;
        assert!(!after.contains("web-temp"), "skill should be gone: {after}");
        assert!(
            crate::skills::load(&root)
                .iter()
                .all(|s| s.name != "web-temp"),
            "skill file should be removed"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
