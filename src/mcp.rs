//! Model Context Protocol client.
//!
//! MCP is how every other agent in this space grew an extension ecosystem:
//! a server is a small process (or an HTTP endpoint) that advertises tools,
//! resources and prompts over JSON-RPC, and the agent calls them as if they
//! were built in. koda had nothing here, which meant a user with a Postgres
//! server, a Sentry server, or their own company's internal server had to
//! rewrite it as a `[[tools]]` shell command or go without.
//!
//! Two transports, because those are the two that exist in the wild:
//!
//! * **stdio** — a child process, newline-delimited JSON on its stdin/stdout.
//!   This is what nearly every published server uses.
//! * **streamable HTTP** — one POST per message, the reply either a JSON body
//!   or an SSE stream carrying it. This is what hosted servers use.
//!
//! The catalog (what each server offers) is deliberately split from the live
//! connections: the catalog is behind a plain `RwLock` so the synchronous
//! `advertised_tools` can read it without an await, while the connections live
//! behind an async mutex because talking to them is I/O.

use crate::config::{Config, McpServer};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, RwLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{oneshot, Mutex};

/// The revision of the protocol koda speaks. Servers negotiate down; one that
/// insists on something newer says so in `initialize` and we report that rather
/// than pretending the handshake worked.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// How long any single MCP request may take. Generous, because an MCP tool is
/// often a network call to somebody else's API, but bounded — a server that
/// never answers must not hang the turn.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// The handshake gets less: a server that cannot say hello in this long is
/// broken, and every second here is a second before the first prompt.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Cap on the text one tool call may return to the model. An MCP server can
/// hand back a whole database dump; the context window cannot take one.
const MAX_RESULT_BYTES: usize = 64 * 1024;

/// The prefix that makes an MCP tool's name unmistakable in the tool list.
///
/// Namespaced because two servers may both call something `search`, and because
/// a model that sees `mcp__github__search` knows without being told where the
/// answer will come from. The double underscore is the de-facto convention.
pub const PREFIX: &str = "mcp__";

// ------------------------------------------------------------------- catalog

/// One tool as its server describes it.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// The name the server knows it by, unqualified.
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments, passed to the model untouched.
    pub schema: Value,
    /// The server's own claim that this tool only reads. MCP calls it
    /// `annotations.readOnlyHint`. When a server says so we believe it enough
    /// to skip the approval prompt and to allow the tool in plan mode; when it
    /// says nothing, the tool asks. Trusting the hint is the difference between
    /// a read-only server being usable and it being a permission dialogue.
    pub read_only: bool,
}

impl ToolDef {
    /// The name the model calls: `mcp__<server>__<tool>`.
    pub fn qualified(&self, server: &str) -> String {
        qualify(server, &self.name)
    }
}

/// A resource a server exposes, as listed.
#[derive(Debug, Clone)]
pub struct ResourceDef {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime: String,
}

/// A prompt template a server exposes.
#[derive(Debug, Clone)]
pub struct PromptDef {
    pub name: String,
    pub description: String,
    pub arguments: Vec<String>,
}

/// Whether a configured server ever came up, and what it offers if it did.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub name: String,
    /// stdio command line or HTTP url, for the status view.
    pub origin: String,
    pub connected: bool,
    /// Why it is not connected, when it is not.
    pub error: Option<String>,
    /// What the server called itself in the handshake.
    pub server_name: String,
    pub server_version: String,
    pub protocol: String,
    pub tools: Vec<ToolDef>,
    pub resources: Vec<ResourceDef>,
    pub prompts: Vec<PromptDef>,
}

impl ServerInfo {
    fn stub(cfg: &McpServer) -> Self {
        Self {
            name: cfg.name.clone(),
            origin: cfg.origin(),
            connected: false,
            error: None,
            server_name: String::new(),
            server_version: String::new(),
            protocol: String::new(),
            tools: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
        }
    }
}

/// Qualify a server-local tool name for the model.
pub fn qualify(server: &str, tool: &str) -> String {
    format!("{PREFIX}{server}{}{tool}", "__")
}

/// Split a qualified name back into (server, tool). `None` if it is not one of
/// ours, which is how every caller tells an MCP call from a built-in.
pub fn split(qualified: &str) -> Option<(&str, &str)> {
    let rest = qualified.strip_prefix(PREFIX)?;
    let (server, tool) = rest.split_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

/// Whether a tool name belongs to MCP at all. Cheap, and called on every tool
/// call, so it is a prefix test and nothing more.
pub fn is_mcp_tool(name: &str) -> bool {
    split(name).is_some()
}

// ------------------------------------------------------------------ registry

struct Registry {
    /// What the model may be told about. Read synchronously, on the hot path
    /// of building every request, so it is a plain lock over cheap clones.
    catalog: RwLock<Vec<ServerInfo>>,
    /// The live connections. Async, because using one is I/O.
    conns: Mutex<HashMap<String, Arc<Conn>>>,
    /// Set once `connect_all` has been started, so a second call is a no-op
    /// rather than a second set of child processes.
    started: StdMutex<bool>,
}

fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| Registry {
        catalog: RwLock::new(Vec::new()),
        conns: Mutex::new(HashMap::new()),
        started: StdMutex::new(false),
    })
}

/// Every configured server and its state, for `/mcp` and `koda mcp list`.
pub fn catalog() -> Vec<ServerInfo> {
    registry()
        .catalog
        .read()
        .map(|c| c.clone())
        .unwrap_or_default()
}

/// Whether every configured server has finished trying to connect.
///
/// A server is settled once it is either connected or has failed; the stub the
/// catalog starts with is neither.
pub fn settled() -> bool {
    registry()
        .catalog
        .read()
        .map(|c| c.iter().all(|s| s.connected || s.error.is_some()))
        .unwrap_or(true)
}

/// Wait for every server to settle, up to `wait`.
///
/// Exists because connecting changes the advertised tool list, and a changed
/// tool list is a changed prompt: the model server caches by prefix, so tools
/// arriving mid-session throw that cache away and cost a full re-prefill of the
/// preamble -- about eleven seconds on a local model. Knowing when the list has
/// stopped moving is what lets the cache be warmed once, for the right shape.
pub async fn wait_settled(wait: Duration) {
    let deadline = std::time::Instant::now() + wait;
    while !settled() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether any server is connected and offering at least one tool. Used to
/// decide whether the system prompt should mention MCP at all — a session with
/// no servers should not pay a paragraph for a feature it is not using.
pub fn any_tools() -> bool {
    registry()
        .catalog
        .read()
        .map(|c| c.iter().any(|s| !s.tools.is_empty()))
        .unwrap_or(false)
}

/// Look one tool up by its qualified name.
pub fn find_tool(qualified: &str) -> Option<(String, ToolDef)> {
    let (server, tool) = split(qualified)?;
    let cat = registry().catalog.read().ok()?;
    let s = cat.iter().find(|s| s.name == server)?;
    let t = s.tools.iter().find(|t| t.name == tool)?;
    Some((s.name.clone(), t.clone()))
}

/// Whether calling this tool should stop and ask. Unknown tools ask: an MCP
/// server runs somebody else's code, and the safe default when we have no
/// annotation is the loud one.
pub fn tool_is_mutating(qualified: &str) -> bool {
    find_tool(qualified)
        .map(|(_, t)| !t.read_only)
        .unwrap_or(true)
}

/// The OpenAI-shaped tool schemas for every connected server.
///
/// `read_only_only` restricts it to the tools a server has promised do not
/// change anything — which is what plan mode advertises.
pub fn openai_schemas(read_only_only: bool) -> Vec<Value> {
    let Ok(cat) = registry().catalog.read() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in cat.iter().filter(|s| s.connected) {
        for t in &s.tools {
            if read_only_only && !t.read_only {
                continue;
            }
            // A description is not optional for a model that has to choose
            // between twenty tools, so a server that omitted one gets a
            // serviceable stand-in naming where the tool came from.
            let desc = if t.description.trim().is_empty() {
                format!("`{}` from the `{}` MCP server.", t.name, s.name)
            } else {
                format!("[{}] {}", s.name, t.description.trim())
            };
            out.push(json!({
                "type": "function",
                "function": {
                    "name": t.qualified(&s.name),
                    "description": desc,
                    "parameters": normalize_schema(&t.schema),
                }
            }));
        }
    }
    out
}

/// Each connected server and its tools' names, for the prompt's one-line
/// mention of the deferred `mcp` group: `github: search_issues, get_issue`.
pub fn catalog_summary() -> String {
    let Ok(cat) = registry().catalog.read() else {
        return String::new();
    };
    cat.iter()
        .filter(|s| s.connected && !s.tools.is_empty())
        .map(|s| {
            let names: Vec<&str> = s.tools.iter().take(12).map(|t| t.name.as_str()).collect();
            let more = s.tools.len().saturating_sub(12);
            if more > 0 {
                format!("{}: {} +{more}", s.name, names.join(", "))
            } else {
                format!("{}: {}", s.name, names.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// One line per tool, for the text protocol's prompt listing.
pub fn text_protocol_help(read_only_only: bool) -> String {
    let Ok(cat) = registry().catalog.read() else {
        return String::new();
    };
    let mut out = String::new();
    for s in cat.iter().filter(|s| s.connected) {
        for t in &s.tools {
            if read_only_only && !t.read_only {
                continue;
            }
            let args: Vec<String> = t
                .schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "- {}({}): {}",
                t.qualified(&s.name),
                args.join(", "),
                t.description.trim()
            );
        }
    }
    out
}

/// A schema every OpenAI-compatible server will accept.
///
/// Servers in the wild send `inputSchema` in shapes the strict endpoints reject:
/// a bare `{}`, a schema with no `type`, or one carrying `$schema` and
/// `additionalProperties` that some local runtimes choke on. The model only
/// needs the properties, so this normalises rather than forwarding verbatim —
/// a tool that is advertised wrongly is a tool that can never be called.
fn normalize_schema(schema: &Value) -> Value {
    let obj = match schema.as_object() {
        Some(o) if !o.is_empty() => o.clone(),
        _ => return json!({ "type": "object", "properties": {} }),
    };
    let mut out = serde_json::Map::new();
    out.insert("type".into(), json!("object"));
    out.insert(
        "properties".into(),
        obj.get("properties").cloned().unwrap_or_else(|| json!({})),
    );
    if let Some(req) = obj.get("required").filter(|r| r.is_array()) {
        out.insert("required".into(), req.clone());
    }
    Value::Object(out)
}

// ----------------------------------------------------------------- lifecycle

/// Bring every enabled server up, in the background.
///
/// Background because a server is a process to spawn and a handshake to wait
/// for, and neither should stand between the user and their first prompt. The
/// catalog fills in as servers answer; `advertised_tools` reads whatever is
/// there at the time, so an early turn simply sees fewer tools rather than
/// waiting for all of them.
pub fn connect_all(cfg: &Config, root: &Path) {
    if !cfg.mcp {
        return;
    }
    let servers: Vec<McpServer> = cfg
        .mcp_servers
        .iter()
        .filter(|s| s.enabled)
        .cloned()
        .collect();
    if servers.is_empty() {
        return;
    }
    {
        let mut started = registry().started.lock().expect("lock");
        if *started {
            return;
        }
        *started = true;
    }
    // Seed the catalog with a stub per server, so `/mcp` can show "connecting"
    // instead of an empty list while the handshakes are in flight.
    if let Ok(mut cat) = registry().catalog.write() {
        *cat = servers.iter().map(ServerInfo::stub).collect();
    }
    let root = root.to_path_buf();
    for s in servers {
        let root = root.clone();
        tokio::spawn(async move {
            let name = s.name.clone();
            match connect_one(&s, &root).await {
                Ok((conn, info)) => {
                    crate::tel_info!(
                        "mcp",
                        "server connected",
                        "server" => name,
                        "tools" => info.tools.len(),
                        "resources" => info.resources.len()
                    );
                    registry()
                        .conns
                        .lock()
                        .await
                        .insert(info.name.clone(), Arc::new(conn));
                    publish(info);
                }
                Err(e) => {
                    let why = format!("{e:#}");
                    crate::tel_warn!("mcp", format!("{name}: {why}"));
                    let mut info = ServerInfo::stub(&s);
                    info.error = Some(why);
                    publish(info);
                }
            }
        });
    }
}

/// Replace one server's entry in the catalog, keeping the configured order.
fn publish(info: ServerInfo) {
    if let Ok(mut cat) = registry().catalog.write() {
        match cat.iter().position(|s| s.name == info.name) {
            Some(i) => cat[i] = info,
            None => cat.push(info),
        }
    }
}

async fn connect_one(cfg: &McpServer, root: &Path) -> Result<(Conn, ServerInfo)> {
    let conn = if cfg.url.trim().is_empty() {
        if cfg.command.trim().is_empty() {
            bail!("needs either a `command` (stdio) or a `url` (HTTP)");
        }
        Conn::Stdio(StdioConn::spawn(cfg, root).await?)
    } else {
        Conn::Http(HttpConn::new(cfg)?)
    };

    let init = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        conn.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "roots": { "listChanged": false } },
                "clientInfo": { "name": "koda", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "the server did not answer `initialize` within {}s",
            HANDSHAKE_TIMEOUT.as_secs()
        )
    })??;

    // Only after the server has answered may it be told we are ready; sending
    // this first is what makes strict servers close the pipe.
    conn.notify("notifications/initialized", json!({})).await?;

    let mut info = ServerInfo::stub(cfg);
    info.connected = true;
    info.protocol = init
        .pointer("/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    info.server_name = init
        .pointer("/serverInfo/name")
        .and_then(Value::as_str)
        .unwrap_or(&cfg.name)
        .to_string();
    info.server_version = init
        .pointer("/serverInfo/version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    info.tools = list_tools(&conn, cfg).await;
    // Resources and prompts are optional halves of the protocol. A server that
    // does not implement them answers with an error, which is not a failure of
    // the connection — so they are collected best-effort and never fatal.
    info.resources = list_resources(&conn).await;
    info.prompts = list_prompts(&conn).await;
    Ok((conn, info))
}

/// Page through `tools/list`, applying the server's allow/deny lists.
async fn list_tools(conn: &Conn, cfg: &McpServer) -> Vec<ToolDef> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let Ok(page) = conn.request("tools/list", params).await else {
            break;
        };
        for t in page
            .get("tools")
            .and_then(Value::as_array)
            .unwrap_or(&vec![])
        {
            let Some(name) = t.get("name").and_then(Value::as_str) else {
                continue;
            };
            if !cfg.allows(name) {
                continue;
            }
            out.push(ToolDef {
                name: name.to_string(),
                description: t
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                schema: t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                // A server's own read-only claim, or the user's blanket trust
                // of this server. Trust is per-server on purpose: "my local
                // filesystem server may write without asking" is a sentence a
                // user can mean, "all MCP servers may" is not.
                read_only: cfg.trust
                    || t.pointer("/annotations/readOnlyHint")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
            });
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() || out.len() > 500 {
            break;
        }
    }
    out
}

async fn list_resources(conn: &Conn) -> Vec<ResourceDef> {
    let Ok(page) = conn.request("resources/list", json!({})).await else {
        return Vec::new();
    };
    page.get("resources")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .filter_map(|r| {
                    Some(ResourceDef {
                        uri: r.get("uri").and_then(Value::as_str)?.to_string(),
                        name: r
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        description: r
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        mime: r
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn list_prompts(conn: &Conn) -> Vec<PromptDef> {
    let Ok(page) = conn.request("prompts/list", json!({})).await else {
        return Vec::new();
    };
    page.get("prompts")
        .and_then(Value::as_array)
        .map(|ps| {
            ps.iter()
                .filter_map(|p| {
                    Some(PromptDef {
                        name: p.get("name").and_then(Value::as_str)?.to_string(),
                        description: p
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        arguments: p
                            .get("arguments")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.get("name").and_then(Value::as_str))
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn conn_for(server: &str) -> Option<Arc<Conn>> {
    registry().conns.lock().await.get(server).cloned()
}

// --------------------------------------------------------------------- calls

/// Call a tool by its qualified name and render the result as text for the
/// model. Errors come back as `Err` so the caller can shape the message.
pub async fn call_tool(qualified: &str, args: &Value) -> Result<String> {
    let (server, tool) =
        split(qualified).ok_or_else(|| anyhow!("`{qualified}` is not an MCP tool name"))?;
    let conn = conn_for(server)
        .await
        .ok_or_else(|| anyhow!("the `{server}` MCP server is not connected"))?;
    let args = if args.is_object() {
        args.clone()
    } else {
        json!({})
    };
    let body = tokio::time::timeout(
        REQUEST_TIMEOUT,
        conn.request("tools/call", json!({ "name": tool, "arguments": args })),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "`{qualified}` did not answer within {}s",
            REQUEST_TIMEOUT.as_secs()
        )
    })??;

    let text = render_content(&body);
    // MCP reports a tool's own failure inside a successful response, so this
    // has to be read out rather than inferred from the transport.
    if body.get("isError").and_then(Value::as_bool) == Some(true) {
        bail!(
            "{}",
            if text.trim().is_empty() {
                "the tool reported an error".into()
            } else {
                text
            }
        );
    }
    Ok(text)
}

/// Read one resource, by URI.
pub async fn read_resource(server: &str, uri: &str) -> Result<String> {
    let conn = conn_for(server)
        .await
        .ok_or_else(|| anyhow!("the `{server}` MCP server is not connected"))?;
    let body = tokio::time::timeout(
        REQUEST_TIMEOUT,
        conn.request("resources/read", json!({ "uri": uri })),
    )
    .await
    .map_err(|_| anyhow!("reading `{uri}` timed out"))??;
    let mut out = String::new();
    for c in body
        .get("contents")
        .and_then(Value::as_array)
        .unwrap_or(&vec![])
    {
        if let Some(t) = c.get("text").and_then(Value::as_str) {
            out.push_str(t);
            out.push('\n');
        } else if let Some(b) = c.get("blob").and_then(Value::as_str) {
            let _ = writeln!(
                out,
                "[binary resource, {} base64 bytes, mime {}]",
                b.len(),
                c.get("mimeType").and_then(Value::as_str).unwrap_or("?")
            );
        }
    }
    Ok(truncate(out))
}

/// Fetch a prompt template and render its messages as plain text.
pub async fn get_prompt(server: &str, name: &str, args: &Value) -> Result<String> {
    let conn = conn_for(server)
        .await
        .ok_or_else(|| anyhow!("the `{server}` MCP server is not connected"))?;
    let body = tokio::time::timeout(
        REQUEST_TIMEOUT,
        conn.request(
            "prompts/get",
            json!({ "name": name, "arguments": args.clone() }),
        ),
    )
    .await
    .map_err(|_| anyhow!("fetching prompt `{name}` timed out"))??;
    let mut out = String::new();
    if let Some(d) = body.get("description").and_then(Value::as_str) {
        let _ = writeln!(out, "{d}\n");
    }
    for m in body
        .get("messages")
        .and_then(Value::as_array)
        .unwrap_or(&vec![])
    {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        let text = m
            .pointer("/content/text")
            .and_then(Value::as_str)
            .unwrap_or("");
        let _ = writeln!(out, "[{role}] {text}");
    }
    Ok(truncate(out))
}

/// Flatten an MCP content array into something a language model can read.
fn render_content(body: &Value) -> String {
    let mut out = String::new();
    for c in body
        .get("content")
        .and_then(Value::as_array)
        .unwrap_or(&vec![])
    {
        match c.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                out.push_str(c.get("text").and_then(Value::as_str).unwrap_or(""));
                out.push('\n');
            }
            // koda's transcript has no place to put an inline image coming back
            // from a tool, and a base64 blob in the context is thousands of
            // wasted tokens. Naming it is more useful than pasting it.
            "image" => {
                let _ = writeln!(
                    out,
                    "[image, {}]",
                    c.get("mimeType").and_then(Value::as_str).unwrap_or("image")
                );
            }
            "resource" => {
                if let Some(t) = c.pointer("/resource/text").and_then(Value::as_str) {
                    out.push_str(t);
                    out.push('\n');
                } else if let Some(u) = c.pointer("/resource/uri").and_then(Value::as_str) {
                    let _ = writeln!(out, "[resource {u}]");
                }
            }
            other => {
                let _ = writeln!(out, "[{other} content]");
            }
        }
    }
    // Newer servers return machine-readable results alongside the prose. When
    // there is no prose at all, that structure is the answer.
    if out.trim().is_empty() {
        if let Some(s) = body.get("structuredContent").filter(|v| !v.is_null()) {
            out = serde_json::to_string_pretty(s).unwrap_or_default();
        }
    }
    truncate(out)
}

fn truncate(mut s: String) -> String {
    if s.len() > MAX_RESULT_BYTES {
        // Cut on a character boundary; MCP results are UTF-8 and slicing
        // blindly at a byte index panics on the first multi-byte character.
        let mut cut = MAX_RESULT_BYTES;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        let dropped = s.len() - cut;
        s.truncate(cut);
        let _ = write!(s, "\n… [{dropped} more bytes truncated by koda]");
    }
    s
}

/// Stop every server. Called on exit so a stdio child does not outlive koda.
pub async fn shutdown() {
    let conns: Vec<Arc<Conn>> = registry()
        .conns
        .lock()
        .await
        .drain()
        .map(|(_, c)| c)
        .collect();
    for c in conns {
        c.shutdown().await;
    }
}

// ----------------------------------------------------------------- transport

enum Conn {
    Stdio(StdioConn),
    Http(HttpConn),
}

impl Conn {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        match self {
            Conn::Stdio(c) => c.request(method, params).await,
            Conn::Http(c) => c.request(method, params).await,
        }
    }
    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        match self {
            Conn::Stdio(c) => c.notify(method, params).await,
            Conn::Http(c) => c.notify(method, params).await,
        }
    }
    async fn shutdown(&self) {
        if let Conn::Stdio(c) = self {
            c.shutdown().await;
        }
    }
}

/// Turn a JSON-RPC envelope into either the result or a readable error.
fn unwrap_rpc(msg: Value, method: &str) -> Result<Value> {
    if let Some(err) = msg.get("error").filter(|e| !e.is_null()) {
        let text = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        bail!("`{method}` failed ({code}): {text}");
    }
    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
}

// --- stdio ----------------------------------------------------------------

struct StdioConn {
    child: Mutex<tokio::process::Child>,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    seq: AtomicI64,
    pending: Arc<StdMutex<HashMap<i64, oneshot::Sender<Value>>>>,
}

impl StdioConn {
    async fn spawn(cfg: &McpServer, root: &Path) -> Result<Self> {
        let cwd: PathBuf = if cfg.cwd.trim().is_empty() {
            root.to_path_buf()
        } else {
            let p = PathBuf::from(&cfg.cwd);
            if p.is_absolute() {
                p
            } else {
                root.join(p)
            }
        };
        let mut cmd = tokio::process::Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Killed with koda rather than left behind. A user who quits and
            // restarts a few times should not accumulate orphaned servers.
            .kill_on_drop(true);
        for (k, v) in &cfg.env {
            cmd.env(k, expand_env(v));
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting `{}`", cfg.command))?;

        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("piped")));

        let pending: Arc<StdMutex<HashMap<i64, oneshot::Sender<Value>>>> = Arc::default();
        {
            let pending = pending.clone();
            let stdin = stdin.clone();
            let root = root.to_path_buf();
            let name = cfg.name.clone();
            tokio::spawn(read_loop(stdout, pending, stdin, root, name));
        }
        // A server's stderr is where its startup problems are explained — a
        // missing API key, a bad path. Losing it turns "server failed" into a
        // mystery, so it goes to the event log where /logs will show it.
        {
            let name = cfg.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    if !l.trim().is_empty() {
                        crate::tel_debug!("mcp", format!("[{name}] {l}"));
                    }
                }
            });
        }

        Ok(Self {
            child: Mutex::new(child),
            stdin,
            seq: AtomicI64::new(1),
            pending,
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("lock").insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = write_line(&self.stdin, &msg).await {
            self.pending.lock().expect("lock").remove(&id);
            return Err(e);
        }
        match rx.await {
            Ok(resp) => unwrap_rpc(resp, method),
            // The sender is only dropped when the reader loop ends, which means
            // the process is gone.
            Err(_) => bail!("`{method}`: the MCP server exited"),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        write_line(
            &self.stdin,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )
        .await
    }

    async fn shutdown(&self) {
        let _ = self.child.lock().await.kill().await;
    }
}

async fn write_line(stdin: &Arc<Mutex<tokio::process::ChildStdin>>, msg: &Value) -> Result<()> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    let mut w = stdin.lock().await;
    w.write_all(line.as_bytes())
        .await
        .context("writing to the MCP server")?;
    w.flush().await.context("flushing to the MCP server")?;
    Ok(())
}

/// Read newline-delimited JSON until the server's stdout closes.
async fn read_loop(
    stdout: tokio::process::ChildStdout,
    pending: Arc<StdMutex<HashMap<i64, oneshot::Sender<Value>>>>,
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    root: PathBuf,
    name: String,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            // Servers do print the odd banner to stdout despite the spec. One
            // unparseable line must not end the conversation.
            crate::tel_debug!("mcp", format!("[{name}] non-JSON on stdout: {line}"));
            continue;
        };
        let has_method = msg.get("method").is_some();
        let id = msg.get("id").cloned();
        match (has_method, id) {
            // A response to something we sent.
            (false, Some(Value::Number(n))) => {
                if let Some(id) = n.as_i64() {
                    if let Some(tx) = pending.lock().expect("lock").remove(&id) {
                        let _ = tx.send(msg);
                    }
                }
            }
            // A request *from* the server. Answering — even to refuse — matters:
            // a server waiting on a reply that never comes stops serving.
            (true, Some(id)) => {
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
                let reply = server_request_reply(method, &root, id);
                let _ = write_line(&stdin, &reply).await;
            }
            // A notification. Nothing here needs acting on yet; logging it
            // keeps `tools/list_changed` visible while it goes unhandled.
            _ => {
                if let Some(m) = msg.get("method").and_then(Value::as_str) {
                    crate::tel_debug!("mcp", format!("[{name}] notification {m}"));
                }
            }
        }
    }
    // Wake everyone still waiting; the process is gone.
    pending.lock().expect("lock").clear();
}

/// What koda answers when a server asks *it* something.
///
/// `roots/list` gets a real answer — the workspace — because a filesystem or
/// git server that knows the project root behaves far better than one guessing.
/// Everything else is declined properly, with the JSON-RPC code that means "I
/// do not implement that", which servers handle gracefully.
fn server_request_reply(method: &str, root: &Path, id: Value) -> Value {
    match method {
        "roots/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "roots": [{
                    "uri": path_to_uri(root),
                    "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("workspace")
                }]
            }
        }),
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("koda does not implement `{method}`") }
        }),
    }
}

/// `file://` URI for a local path. Shared with the LSP client, which needs the
/// same thing for every document it opens.
pub fn path_to_uri(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::from("file://");
    // Windows paths need the extra slash before the drive letter; on unix the
    // path already starts with one.
    if !s.starts_with('/') {
        out.push('/');
    }
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' | ':' => out.push(ch),
            '\\' => out.push('/'),
            _ => {
                let mut buf = [0u8; 4];
                for b in ch.encode_utf8(&mut buf).as_bytes() {
                    let _ = write!(out, "%{b:02X}");
                }
            }
        }
    }
    out
}

/// Turn a `file://` URI back into a path.
pub fn uri_to_path(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).to_string())
}

/// Expand `${VAR}` and `$VAR` from koda's own environment.
///
/// The point is that a config file can be committed: `token = "${GITHUB_TOKEN}"`
/// is shareable, the token pasted in literally is not.
fn expand_env(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let braced = i + 1 < chars.len() && chars[i + 1] == '{';
        let start = if braced { i + 2 } else { i + 1 };
        // An environment variable never starts with a digit, so `$5` in a
        // command line is five dollars and not an empty expansion.
        if start >= chars.len() || !(chars[start].is_ascii_alphabetic() || chars[start] == '_') {
            out.push('$');
            i += 1;
            continue;
        }
        let mut end = start;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        if end == start || (braced && (end >= chars.len() || chars[end] != '}')) {
            out.push('$');
            i += 1;
            continue;
        }
        let name: String = chars[start..end].iter().collect();
        out.push_str(&std::env::var(&name).unwrap_or_default());
        i = if braced { end + 1 } else { end };
    }
    out
}

// --- streamable HTTP ------------------------------------------------------

struct HttpConn {
    http: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    seq: AtomicI64,
    /// Servers that keep state hand out a session id in the `initialize`
    /// response headers and expect it back on every later message.
    session: StdMutex<Option<String>>,
}

impl HttpConn {
    fn new(cfg: &McpServer) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("koda/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            url: cfg.url.trim().to_string(),
            headers: cfg
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), expand_env(v)))
                .collect(),
            seq: AtomicI64::new(1),
            session: StdMutex::new(None),
        })
    }

    fn build(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            // Both are declared because the server picks: a simple server
            // answers with JSON, a streaming one with SSE, and refusing either
            // would make koda incompatible with half of them.
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(body);
        for (k, v) in &self.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Some(s) = self.session.lock().expect("lock").as_ref() {
            rb = rb.header("Mcp-Session-Id", s.as_str());
        }
        rb
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let resp = self
            .build(&body)
            .send()
            .await
            .with_context(|| format!("POST {}", self.url))?;
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().expect("lock") = Some(sid.to_string());
        }
        let status = resp.status();
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp.text().await.context("reading the response")?;
        if !status.is_success() {
            bail!("`{method}`: HTTP {status} — {}", first_line(&text));
        }
        let msg = if ct.contains("text/event-stream") {
            sse_message(&text, id)
                .ok_or_else(|| anyhow!("`{method}`: the SSE stream carried no reply"))?
        } else {
            serde_json::from_str::<Value>(&text).with_context(|| {
                format!("`{method}`: reply was not JSON — {}", first_line(&text))
            })?
        };
        unwrap_rpc(msg, method)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        // A notification has no reply, so a 202 with an empty body is the
        // normal, correct outcome; only the transport failing is an error.
        self.build(&body)
            .send()
            .await
            .with_context(|| format!("POST {}", self.url))?;
        Ok(())
    }
}

/// Pull the JSON-RPC message with this id out of an SSE body.
fn sse_message(text: &str, id: i64) -> Option<Value> {
    let mut last = None;
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        if v.get("id").and_then(Value::as_i64) == Some(id) {
            return Some(v);
        }
        last = Some(v);
    }
    last
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(200).collect()
}

// ------------------------------------------------------------------ reporting

/// The `/mcp` view and `koda mcp list`: every configured server, its state, and
/// what it brought.
pub fn status_report(cfg: &Config) -> String {
    let mut out = String::new();
    if !cfg.mcp {
        return "MCP is off. Set `mcp = true` in your config to turn it on.\n".into();
    }
    if cfg.mcp_servers.is_empty() {
        return "No MCP servers configured. Add one as an `[[mcp_server]]` table in \
                koda.toml — see docs/mcp.md.\n"
            .into();
    }
    let cat = catalog();
    for s in &cfg.mcp_servers {
        let info = cat.iter().find(|c| c.name == s.name);
        let state = match info {
            Some(i) if i.connected => "connected",
            Some(i) if i.error.is_some() => "failed",
            _ if !s.enabled => "disabled",
            _ => "connecting…",
        };
        // The live entry's origin when there is one, so a server whose
        // command was resolved differently than the config reads is reported
        // as it actually ran.
        let origin = info
            .map(|i| i.origin.clone())
            .filter(|o| !o.is_empty())
            .unwrap_or_else(|| s.origin());
        let _ = writeln!(out, "{} [{state}] — {origin}", s.name);
        if let Some(i) = info {
            if let Some(e) = &i.error {
                let _ = writeln!(out, "    error: {e}");
            }
            if i.connected {
                if !i.server_version.is_empty() {
                    let _ = writeln!(
                        out,
                        "    {} {} (protocol {})",
                        i.server_name, i.server_version, i.protocol
                    );
                }
                let _ = writeln!(
                    out,
                    "    {} tools, {} resources, {} prompts",
                    i.tools.len(),
                    i.resources.len(),
                    i.prompts.len()
                );
                for t in &i.tools {
                    let _ = writeln!(
                        out,
                        "      {}{}  {}",
                        t.qualified(&i.name),
                        if t.read_only { "" } else { " ●" },
                        first_line(&t.description)
                    );
                }
            }
        }
    }
    out.push_str("\n● asks for approval before it runs.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that speaks the parts of MCP koda uses, and gets the awkward
    /// details right on purpose: a paginated tool list, a tool failure reported
    /// inside a successful response, and a request sent *to* the client, which
    /// a client that only ever reads responses will hang on.
    const FAKE_SERVER: &str = r#"
import sys, json

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()

def reply(req, result):
    send({"jsonrpc": "2.0", "id": req["id"], "result": result})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    m = req.get("method")
    if m == "initialize":
        reply(req, {"protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "9.9"}})
    elif m == "notifications/initialized":
        # Ask the client something. A client that never answers leaves this
        # server waiting, so the rest of the session proves the reply landed.
        send({"jsonrpc": "2.0", "id": 9001, "method": "roots/list"})
    elif m == "tools/list":
        if req.get("params", {}).get("cursor") is None:
            reply(req, {"tools": [{"name": "echo",
                                   "description": "Echo the text back.",
                                   "inputSchema": {"type": "object",
                                                   "properties": {"text": {"type": "string"}},
                                                   "required": ["text"]},
                                   "annotations": {"readOnlyHint": True}}],
                        "nextCursor": "page2"})
        else:
            reply(req, {"tools": [
                {"name": "boom", "description": "Always fails.", "inputSchema": {}},
                {"name": "secret", "description": "Excluded by config.", "inputSchema": {}}]})
    elif m == "resources/list":
        reply(req, {"resources": [{"uri": "mem://note", "name": "note",
                                   "mimeType": "text/plain", "description": "a note"}]})
    elif m == "prompts/list":
        reply(req, {"prompts": [{"name": "review", "description": "Review it.",
                                 "arguments": [{"name": "path"}]}]})
    elif m == "resources/read":
        reply(req, {"contents": [{"uri": "mem://note", "text": "the note body"}]})
    elif m == "prompts/get":
        reply(req, {"description": "Review it.",
                    "messages": [{"role": "user",
                                  "content": {"type": "text", "text": "look at " +
                                              req["params"]["arguments"].get("path", "?")}}]})
    elif m == "tools/call":
        name = req["params"]["name"]
        if name == "boom":
            reply(req, {"isError": True,
                        "content": [{"type": "text", "text": "it exploded"}]})
        elif name == "roots_seen":
            reply(req, {"content": [{"type": "text", "text": ROOTS}]})
        else:
            reply(req, {"content": [{"type": "text",
                                     "text": "echo: " + req["params"]["arguments"]["text"]}]})
    elif "id" in req and "result" in req:
        # The client's answer to roots/list. Remembered so a later tool call
        # can prove koda replied with the real workspace.
        ROOTS = req["result"]["roots"][0]["name"]
    else:
        send({"jsonrpc": "2.0", "id": req.get("id"),
              "error": {"code": -32601, "message": "no"}})
"#;

    fn python() -> bool {
        crate::tools::which_in_path("python3").is_some()
    }

    fn fake_server_config(dir: &std::path::Path) -> McpServer {
        let script = dir.join("fake_mcp.py");
        std::fs::write(&script, FAKE_SERVER).expect("write server");
        McpServer {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![script.to_string_lossy().to_string()],
            enabled: true,
            exclude: vec!["secret".into()],
            ..Default::default()
        }
    }

    /// The whole handshake against a real process: initialize, initialized, a
    /// paginated tool list, the config's exclusions, and a tool call in both
    /// directions.
    #[tokio::test]
    async fn a_real_server_is_connected_listed_and_called() {
        if !python() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("koda-mcp-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let cfg = fake_server_config(&dir);

        let (conn, info) = connect_one(&cfg, &dir).await.expect("connect");
        assert!(info.connected);
        assert_eq!(info.server_name, "fake");
        assert_eq!(info.server_version, "9.9");

        // Both pages arrived, and `exclude` was applied to the second one.
        let names: Vec<&str> = info.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["echo", "boom"],
            "pagination or exclude is wrong"
        );
        // The server's read-only hint is honoured, and its absence means "ask".
        assert!(info.tools[0].read_only);
        assert!(!info.tools[1].read_only);
        assert_eq!(info.resources[0].uri, "mem://note");
        assert_eq!(info.prompts[0].arguments, vec!["path".to_string()]);

        // Register it so the public call path (which resolves by name) works.
        registry()
            .conns
            .lock()
            .await
            .insert("fake".into(), Arc::new(conn));
        publish(info);

        let out = call_tool("mcp__fake__echo", &json!({ "text": "hi" }))
            .await
            .expect("call");
        assert_eq!(out.trim(), "echo: hi");

        // A tool that fails inside a 200 response must surface as an error, not
        // as content the model reads as data.
        let err = call_tool("mcp__fake__boom", &json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("it exploded"), "{err}");

        // And koda answered the server's own `roots/list` with the workspace —
        // the server only knows the name because the reply reached it.
        let seen = call_tool("mcp__fake__roots_seen", &json!({}))
            .await
            .expect("roots");
        assert!(
            seen.contains(dir.file_name().unwrap().to_str().unwrap()),
            "koda did not answer the server's request: {seen}"
        );

        // Resources and prompts, which is the half of MCP that is not tools.
        let note = read_resource("fake", "mem://note").await.expect("resource");
        assert!(note.contains("the note body"), "{note}");
        let pr = get_prompt("fake", "review", &json!({ "path": "a.rs" }))
            .await
            .expect("prompt");
        assert!(pr.contains("look at a.rs"), "{pr}");

        shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A server that is configured but cannot start must be reported, not
    /// silently absent — "my tool is missing" with no explanation is the worst
    /// outcome this feature can have.
    #[tokio::test]
    async fn a_server_that_will_not_start_says_so() {
        let cfg = McpServer {
            name: "nope".into(),
            command: "koda-no-such-binary-at-all".into(),
            enabled: true,
            ..Default::default()
        };
        let e = match connect_one(&cfg, std::path::Path::new(".")).await {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("a missing binary must not connect"),
        };
        assert!(e.contains("koda-no-such-binary-at-all"), "{e}");

        // Neither transport configured is a config error stated plainly.
        let empty = McpServer {
            name: "empty".into(),
            enabled: true,
            ..Default::default()
        };
        let e = match connect_one(&empty, std::path::Path::new(".")).await {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("a server with no transport must not connect"),
        };
        assert!(e.contains("command") && e.contains("url"), "{e}");
    }

    /// Connecting changes the advertised tool list, and a changed list is a
    /// changed prompt -- which on a local model costs a full re-prefill of the
    /// preamble. Knowing when the list has stopped moving is what lets the
    /// cache be warmed once, for the shape the next request will actually use.
    #[test]
    fn a_catalog_is_settled_only_once_every_server_has_answered() {
        // Nothing configured: settled, so a session with no servers never
        // waits for one.
        if let Ok(mut c) = registry().catalog.write() {
            c.clear();
        }
        assert!(settled());

        // A stub is neither connected nor failed — it is still in flight.
        let cfg = McpServer {
            name: "pending".into(),
            ..Default::default()
        };
        publish(ServerInfo::stub(&cfg));
        assert!(
            !settled(),
            "a server still connecting must not count as settled"
        );

        // A failure settles it just as much as a success: koda is waiting to
        // know the shape of the tool list, not for the list to be non-empty.
        let mut failed = ServerInfo::stub(&cfg);
        failed.error = Some("nope".into());
        publish(failed);
        assert!(settled());

        let mut ok = ServerInfo::stub(&cfg);
        ok.connected = true;
        publish(ok);
        assert!(settled());

        if let Ok(mut c) = registry().catalog.write() {
            c.clear();
        }
    }

    #[test]
    fn a_servers_tool_list_can_be_narrowed_from_config() {
        let mut s = McpServer::default();
        assert!(s.allows("anything"), "empty config means every tool");
        s.tools = vec!["keep".into()];
        assert!(s.allows("keep"));
        assert!(!s.allows("drop"));
        // exclude wins over an explicit include, which is the safe direction.
        s.exclude = vec!["keep".into()];
        assert!(!s.allows("keep"));
    }

    #[test]
    fn qualified_names_round_trip() {
        let q = qualify("github", "create_issue");
        assert_eq!(q, "mcp__github__create_issue");
        assert_eq!(split(&q), Some(("github", "create_issue")));
        // A built-in must never be mistaken for an MCP call.
        assert_eq!(split("read_file"), None);
        assert_eq!(split("mcp__"), None);
        assert_eq!(split("mcp__server"), None);
        assert!(!is_mcp_tool("codegraph"));
        // A tool name that itself contains the separator still resolves to the
        // first server segment, which is the one koda routes on.
        assert_eq!(split("mcp__fs__read__file"), Some(("fs", "read__file")));
    }

    #[test]
    fn a_servers_schema_is_made_safe_to_advertise() {
        // The shapes servers actually send, none of which a strict endpoint
        // would accept verbatim.
        for bad in [json!({}), json!(null), json!("nonsense")] {
            let s = normalize_schema(&bad);
            assert_eq!(s["type"], "object");
            assert!(s["properties"].is_object());
        }
        let ok = normalize_schema(&json!({
            "type": "object",
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "additionalProperties": false,
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }));
        assert_eq!(ok["required"], json!(["path"]));
        assert!(ok.get("$schema").is_none(), "{ok}");
        assert!(ok.get("additionalProperties").is_none(), "{ok}");
    }

    #[test]
    fn tool_results_flatten_to_something_readable() {
        let body = json!({
            "content": [
                { "type": "text", "text": "line one" },
                { "type": "image", "mimeType": "image/png", "data": "AAAA" },
                { "type": "resource", "resource": { "uri": "file:///a", "text": "inner" } }
            ]
        });
        let out = render_content(&body);
        assert!(out.contains("line one"), "{out}");
        // The blob must not reach the context; its existence must.
        assert!(out.contains("[image, image/png]"), "{out}");
        assert!(!out.contains("AAAA"), "{out}");
        assert!(out.contains("inner"), "{out}");

        // A server that answers only in structured form still says something.
        let structured = json!({ "content": [], "structuredContent": { "count": 3 } });
        assert!(render_content(&structured).contains("\"count\""));
    }

    #[test]
    fn a_tool_failure_is_read_out_of_a_successful_response() {
        // MCP puts tool errors *inside* a 200 reply, which is the one thing a
        // naive client gets wrong: it reports success and hands the model an
        // error message as if it were data.
        let body =
            json!({ "isError": true, "content": [{ "type": "text", "text": "no such repo" }] });
        assert!(body.get("isError").and_then(Value::as_bool) == Some(true));
        assert!(render_content(&body).contains("no such repo"));
    }

    #[test]
    fn oversized_results_are_cut_on_a_character_boundary() {
        // A naive byte truncation panics here; the multi-byte character sits
        // exactly across the limit.
        let s = "é".repeat(MAX_RESULT_BYTES);
        let out = truncate(s);
        assert!(out.len() < MAX_RESULT_BYTES + 200);
        assert!(out.contains("truncated by koda"));
    }

    #[test]
    fn env_placeholders_come_from_the_environment() {
        std::env::set_var("KODA_TEST_MCP_TOKEN", "s3cret");
        assert_eq!(expand_env("${KODA_TEST_MCP_TOKEN}"), "s3cret");
        assert_eq!(expand_env("Bearer $KODA_TEST_MCP_TOKEN"), "Bearer s3cret");
        // An unset variable becomes empty rather than leaking the literal,
        // which would otherwise be sent as an API key.
        assert_eq!(expand_env("${KODA_TEST_MCP_ABSENT}"), "");
        // A lone dollar is not a placeholder.
        assert_eq!(expand_env("cost: $5"), "cost: $5");
    }

    #[test]
    fn paths_survive_the_trip_through_a_uri() {
        for p in ["/tmp/a b/c.rs", "/tmp/plain.rs", "/tmp/ünïcode/x.py"] {
            let uri = path_to_uri(Path::new(p));
            assert!(uri.starts_with("file:///"), "{uri}");
            assert!(!uri.contains(' '), "{uri}");
            assert_eq!(uri_to_path(&uri), PathBuf::from(p));
        }
    }

    #[test]
    fn the_reply_to_a_servers_own_request_is_always_well_formed() {
        let root = Path::new("/tmp/project");
        let ok = server_request_reply("roots/list", root, json!(7));
        assert_eq!(ok["id"], json!(7));
        assert_eq!(ok["result"]["roots"][0]["name"], "project");
        // Anything unimplemented must still get an answer, or the server hangs.
        let no = server_request_reply("sampling/createMessage", root, json!("x"));
        assert_eq!(no["error"]["code"], -32601);
        assert_eq!(no["id"], json!("x"));
    }

    #[test]
    fn an_sse_body_yields_the_reply_with_our_id() {
        let body = "event: message\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"a\":1}}\n\n\
                    event: message\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"b\":2}}\n\n";
        assert_eq!(sse_message(body, 2).unwrap()["result"]["b"], 2);
        assert_eq!(sse_message(body, 1).unwrap()["result"]["a"], 1);
    }

    #[test]
    fn a_json_rpc_error_becomes_a_message_naming_the_method() {
        let e = unwrap_rpc(
            json!({ "error": { "code": -32602, "message": "bad params" } }),
            "tools/call",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("tools/call"), "{e}");
        assert!(e.contains("bad params"), "{e}");
        assert!(e.contains("-32602"), "{e}");
    }
}
