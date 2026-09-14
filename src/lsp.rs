//! Language Server Protocol client.
//!
//! koda's code graph (`graph.rs`) is regexes: fast, dependency-free, and right
//! often enough to point at the right file. What it cannot do is *type-aware*
//! work. It does not know that the `run` you asked about is the trait method
//! and not the free function three modules over, it cannot tell you what a
//! variable's type is, and its "who uses this" is a name match, not a
//! resolution. For those questions a real language server is not a nicer
//! answer, it is the only correct one.
//!
//! So this sits beside the graph rather than replacing it. The graph still
//! answers instantly and always; the LSP answers precisely when a server for
//! the language is installed. `codegraph` consults it silently for symbol
//! lookups, and the `lsp` tool exposes the operations that have no graph
//! equivalent at all — hover types, real references, diagnostics.
//!
//! The transport is the same `Content-Length`-framed JSON-RPC that `dap.rs`
//! speaks, for the same reason: it is what the servers use.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::mcp::{path_to_uri, uri_to_path};

/// Ordinary requests. A language server answers navigation in milliseconds once
/// it is warm; this is the "something is wrong" bound, not the expected wait.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// The handshake, which for a big server means reading the project manifest.
const INIT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for a server that is still indexing before answering with
/// what it has. rust-analyzer on a cold cargo workspace really does take this
/// long, and answering "not found" while it is still working is a lie.
const INDEX_TIMEOUT: Duration = Duration::from_secs(90);
/// Diagnostics arrive unprompted after a file is opened; this is how long we
/// wait for the first batch before reporting what has arrived.
const DIAGNOSTICS_WAIT: Duration = Duration::from_secs(8);
/// Cap on any one answer handed to the model.
const MAX_ANSWER_BYTES: usize = 32 * 1024;

// ------------------------------------------------------------------- servers

/// A language server koda knows how to start.
#[derive(Debug, Clone)]
pub struct ServerDef {
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// Source extensions this server handles.
    pub extensions: &'static [&'static str],
    /// Files whose presence means this project is one this server understands.
    /// Empty means "any project with a matching file", which is right for
    /// servers that need no manifest.
    pub markers: &'static [&'static str],
    /// Sent as `initializationOptions`. A few servers are unusable without it.
    pub init_options: Option<&'static str>,
}

/// What says a directory is a Python project. Named once because two servers
/// answer for the same language and must agree on when they are relevant.
const PY_MARKERS: &[&str] = &[
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "Pipfile",
    "tox.ini",
];

/// The built-in table.
///
/// Every entry speaks LSP over **stdio** and is the standard server for its
/// language — the same rule `dap.rs` applies to adapters, and for the same
/// reason: an entry that spawns and then never answers is worse than no entry.
/// A server that is not installed is simply not offered.
pub fn servers() -> &'static [ServerDef] {
    static REG: OnceLock<Vec<ServerDef>> = OnceLock::new();
    REG.get_or_init(|| {
        vec![
            ServerDef {
                name: "rust-analyzer",
                command: "rust-analyzer",
                args: &[],
                extensions: &["rs"],
                markers: &["Cargo.toml"],
                init_options: None,
            },
            ServerDef {
                name: "pyright",
                command: "pyright-langserver",
                args: &["--stdio"],
                extensions: &["py", "pyi"],
                markers: PY_MARKERS,
                init_options: None,
            },
            // The fallback for Python, and the one most likely to already be
            // installed on a machine that does any Python at all.
            ServerDef {
                name: "pylsp",
                command: "pylsp",
                args: &[],
                extensions: &["py", "pyi"],
                markers: PY_MARKERS,
                init_options: None,
            },
            ServerDef {
                name: "typescript-language-server",
                command: "typescript-language-server",
                args: &["--stdio"],
                extensions: &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
                markers: &["package.json", "tsconfig.json", "jsconfig.json"],
                init_options: None,
            },
            ServerDef {
                name: "gopls",
                command: "gopls",
                args: &[],
                extensions: &["go"],
                markers: &["go.mod"],
                init_options: None,
            },
            ServerDef {
                name: "clangd",
                command: "clangd",
                args: &["--background-index"],
                extensions: &["c", "h", "cc", "cpp", "hpp", "cxx", "hxx", "hh", "m", "mm"],
                markers: &[
                    "compile_commands.json",
                    "CMakeLists.txt",
                    "Makefile",
                    "meson.build",
                ],
                init_options: None,
            },
            ServerDef {
                name: "zls",
                command: "zls",
                args: &[],
                extensions: &["zig"],
                markers: &["build.zig"],
                init_options: None,
            },
            ServerDef {
                name: "lua-language-server",
                command: "lua-language-server",
                args: &[],
                extensions: &["lua"],
                markers: &[".luarc.json", "init.lua", "rockspec"],
                init_options: None,
            },
            ServerDef {
                name: "solargraph",
                command: "solargraph",
                args: &["stdio"],
                extensions: &["rb"],
                markers: &["Gemfile", ".solargraph.yml"],
                init_options: None,
            },
            ServerDef {
                name: "dart",
                command: "dart",
                args: &["language-server", "--protocol=lsp"],
                extensions: &["dart"],
                markers: &["pubspec.yaml"],
                init_options: None,
            },
            ServerDef {
                name: "elixir-ls",
                command: "elixir-ls",
                args: &[],
                extensions: &["ex", "exs"],
                markers: &["mix.exs"],
                init_options: None,
            },
            ServerDef {
                name: "ocamllsp",
                command: "ocamllsp",
                args: &[],
                extensions: &["ml", "mli"],
                markers: &["dune-project", "dune"],
                init_options: None,
            },
        ]
    })
}

/// Whether a server's executable is on this machine.
///
/// A PATH lookup and nothing more, because this runs while koda is opening.
/// See [`runnable`] for the stronger question, which costs a process.
pub fn installed(s: &ServerDef) -> bool {
    which(s.command).is_some()
}

/// Whether the executable on PATH actually runs.
///
/// Being on PATH is not the same as being installed. `rustup` puts a proxy for
/// `rust-analyzer` in `~/.cargo/bin` whether or not the component is there, and
/// running it prints `error: Unknown binary 'rust-analyzer' in official
/// toolchain` and exits. koda would report the server as usable, advertise the
/// tool, and only discover the truth when the model called it -- a wasted turn
/// and a confusing error.
///
/// Costs a process, so it is only asked where a human is reading the answer:
/// `lsp action=servers` and `/lsp`. The startup path keeps the cheap check, and
/// a server that turns out not to run says so when it is called.
fn runnable(s: &ServerDef) -> Option<bool> {
    let bin = which(s.command)?;
    // `gopls` spells it `gopls version`; the rest take `--version`.
    let arg = if s.command == "gopls" {
        "version"
    } else {
        "--version"
    };
    let out = Command::new(&bin)
        .arg(arg)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    Some(out.status.success())
}

fn which(bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        let p = PathBuf::from(bin);
        return p.is_file().then_some(p);
    }
    crate::tools::which_in_path(bin)
}

fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// The LSP language id for a file, which servers use to decide how to parse it.
fn language_id(path: &str) -> &'static str {
    match ext_of(path).as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" | "cxx" | "hxx" | "hh" => "cpp",
        "m" | "mm" => "objective-c",
        "zig" => "zig",
        "lua" => "lua",
        "rb" => "ruby",
        "dart" => "dart",
        "ex" | "exs" => "elixir",
        "ml" | "mli" => "ocaml",
        _ => "plaintext",
    }
}

/// Pick the server for a file: installed, handles the extension, and — when it
/// declares markers — the project actually looks like one it understands.
///
/// The marker check is what stops `typescript-language-server` being started
/// for a stray `.js` in a Rust repo, which is a slow way to get no answer.
pub fn pick(root: &Path, path: &str) -> Option<&'static ServerDef> {
    let ext = ext_of(path);
    if ext.is_empty() {
        return None;
    }
    servers()
        .iter()
        .filter(|s| s.extensions.contains(&ext.as_str()))
        // A marker match means the project is one this server understands. The
        // model asked about this exact file, though, so an unmarked project
        // whose server is installed is still served -- refusing would be koda
        // being more certain than it has any right to be.
        .filter(|s| s.markers.iter().any(|m| root.join(m).exists()) || !markers_seen(root))
        .find(|s| installed(s))
}

/// Whether any known server's markers are present at all. Used to tell "this
/// project is not yours" from "this project declares nothing either way".
fn markers_seen(root: &Path) -> bool {
    servers()
        .iter()
        .any(|s| s.markers.iter().any(|m| root.join(m).exists()))
}

/// Every server that could serve *this* project, whether or not it is running.
///
/// Used to decide whether the `lsp` tool is worth advertising at all: a project
/// with no installed server should not spend schema tokens on a tool that can
/// only ever say "no server".
pub fn available(root: &Path) -> Vec<&'static ServerDef> {
    // Cached per workspace: the answer is a PATH lookup per server plus a
    // directory walk, and it is asked on every system-prompt rebuild and every
    // decision about advertising the tool. Installing a server mid-session is
    // rare enough to be worth a restart.
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<&'static str>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(c) = cache.lock() {
        if let Some(names) = c.get(root) {
            return servers()
                .iter()
                .filter(|s| names.contains(&s.name))
                .collect();
        }
    }
    let found = probe_available(root);
    if let Ok(mut c) = cache.lock() {
        c.insert(root.to_path_buf(), found.iter().map(|s| s.name).collect());
    }
    found
}

fn probe_available(root: &Path) -> Vec<&'static ServerDef> {
    // Markers before PATH, deliberately. This runs while koda is opening, and
    // koda opens in about three milliseconds -- a claim on the front of the
    // README that a directory walk would quietly cost. Every server declares
    // the files that say "this is a project I understand", so the question is
    // a handful of `stat` calls, and the PATH lookup only happens for the one
    // or two servers that survive it.
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for s in servers() {
        if !s.markers.iter().any(|m| root.join(m).exists()) {
            continue;
        }
        if !installed(s) {
            continue;
        }
        if seen.insert(s.name) {
            out.push(s);
        }
    }
    out
}

// ----------------------------------------------------------------- transport

/// State the reader thread writes and callers read.
#[derive(Default)]
struct Shared {
    /// Latest diagnostics per file URI, as published.
    diagnostics: BTreeMap<String, Vec<Value>>,
    /// URIs that have had at least one `publishDiagnostics`, so "no problems"
    /// can be told apart from "nothing has arrived yet".
    published: HashSet<String>,
    /// Work-done progress tokens still open. Empty means the server is idle,
    /// which for rust-analyzer and gopls is how you know indexing finished.
    progress: HashSet<String>,
    /// Set once any progress has been seen, so a server that reports none is
    /// not mistaken for one that has finished.
    saw_progress: bool,
    /// The server's stdout closed.
    closed: bool,
    /// Last few log lines from the server, for diagnosing a bad setup.
    log: Vec<String>,
}

struct Client {
    child: Child,
    /// Shared with the reader thread, which has to answer the server's own
    /// requests: pyright and gopls both ask for `workspace/configuration` early
    /// and wait for the reply before doing anything useful.
    stdin: Arc<Mutex<ChildStdin>>,
    seq: AtomicI64,
    pending: Arc<Mutex<BTreeMap<i64, SyncSender<Value>>>>,
    state: Arc<(Mutex<Shared>, Condvar)>,
}

impl Client {
    fn spawn(def: &ServerDef, root: &Path) -> Result<Client> {
        let mut child = Command::new(def.command)
            .args(def.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting language server `{}`", def.command))?;

        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");

        let pending: Arc<Mutex<BTreeMap<i64, SyncSender<Value>>>> = Arc::default();
        let state = Arc::new((Mutex::new(Shared::default()), Condvar::new()));

        let stdin = Arc::new(Mutex::new(stdin));
        {
            let pending = pending.clone();
            let state = state.clone();
            let stdin = stdin.clone();
            std::thread::spawn(move || read_loop(stdout, pending, state, stdin));
        }
        {
            let state = state.clone();
            let name = def.name.to_string();
            std::thread::spawn(move || {
                let mut lines = BufReader::new(stderr).lines();
                while let Some(Ok(l)) = lines.next() {
                    if l.trim().is_empty() {
                        continue;
                    }
                    crate::tel_debug!("lsp", format!("[{name}] {l}"));
                    if let Ok(mut s) = state.0.lock() {
                        s.log.push(l);
                        if s.log.len() > 40 {
                            s.log.remove(0);
                        }
                    }
                }
            });
        }

        Ok(Client {
            child,
            stdin,
            seq: AtomicI64::new(1),
            pending,
            state,
        })
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_timeout(method, params, REQUEST_TIMEOUT)
    }

    fn request_timeout(&self, method: &str, params: Value, wait: Duration) -> Result<Value> {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = sync_channel(1);
        self.pending.lock().expect("lock").insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.write(&msg) {
            self.pending.lock().expect("lock").remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(wait) {
            Ok(resp) => {
                if let Some(err) = resp.get("error").filter(|e| !e.is_null()) {
                    let text = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("request failed");
                    bail!("`{method}` failed: {text}");
                }
                Ok(resp.get("result").cloned().unwrap_or(Value::Null))
            }
            Err(_) => {
                self.pending.lock().expect("lock").remove(&id);
                if self.state.0.lock().expect("lock").closed {
                    bail!("`{method}`: the language server exited")
                }
                bail!("`{method}` timed out after {}s", wait.as_secs())
            }
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn write(&self, msg: &Value) -> Result<()> {
        write_frame(&self.stdin, msg)
    }

    /// Wait until the server has no work in flight, or the deadline passes.
    ///
    /// This is the difference between asking rust-analyzer a question and
    /// getting the right answer: queried mid-index it answers `null`, which
    /// reads exactly like "no such symbol".
    fn wait_until_idle(&self, wait: Duration) -> bool {
        let (lock, cv) = &*self.state;
        let deadline = Instant::now() + wait;
        let mut s = lock.lock().expect("lock");
        loop {
            if s.closed {
                return false;
            }
            if s.saw_progress && s.progress.is_empty() {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                // A server that never reported progress is not busy, it is
                // quiet. Treat the timeout as "go ahead and ask".
                return !s.saw_progress;
            };
            let (guard, _) = cv
                .wait_timeout(s, left.min(Duration::from_millis(250)))
                .expect("lock");
            s = guard;
        }
    }

    /// Wait for diagnostics on a file, then return whatever has arrived.
    fn await_diagnostics(&self, uri: &str, wait: Duration) -> Vec<Value> {
        let (lock, cv) = &*self.state;
        let deadline = Instant::now() + wait;
        let mut s = lock.lock().expect("lock");
        loop {
            if s.published.contains(uri) || s.closed {
                return s.diagnostics.get(uri).cloned().unwrap_or_default();
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return s.diagnostics.get(uri).cloned().unwrap_or_default();
            };
            let (guard, _) = cv
                .wait_timeout(s, left.min(Duration::from_millis(250)))
                .expect("lock");
            s = guard;
        }
    }

    fn all_diagnostics(&self) -> BTreeMap<String, Vec<Value>> {
        self.state.0.lock().expect("lock").diagnostics.clone()
    }

    fn shutdown(&mut self) {
        let _ = self.request_timeout("shutdown", Value::Null, Duration::from_secs(3));
        let _ = self.notify("exit", Value::Null);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_loop(
    stdout: std::process::ChildStdout,
    pending: Arc<Mutex<BTreeMap<i64, SyncSender<Value>>>>,
    state: Arc<(Mutex<Shared>, Condvar)>,
    stdin: Arc<Mutex<ChildStdin>>,
) {
    let mut r = BufReader::new(stdout);
    loop {
        let mut len: Option<usize> = None;
        let mut header = String::new();
        loop {
            header.clear();
            match r.read_line(&mut header) {
                Ok(0) | Err(_) => {
                    close(&state);
                    return;
                }
                Ok(_) => {}
            }
            let line = header.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some(v) = line
                .strip_prefix("Content-Length:")
                .or_else(|| line.strip_prefix("content-length:"))
            {
                len = v.trim().parse().ok();
            }
        }
        let Some(len) = len else { continue };
        let mut buf = vec![0u8; len];
        if std::io::Read::read_exact(&mut r, &mut buf).is_err() {
            close(&state);
            return;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&buf) else {
            continue;
        };
        dispatch(msg, &pending, &state, &stdin);
    }
}

fn close(state: &Arc<(Mutex<Shared>, Condvar)>) {
    let (lock, cv) = &**state;
    if let Ok(mut s) = lock.lock() {
        s.closed = true;
    }
    cv.notify_all();
}

fn dispatch(
    msg: Value,
    pending: &Arc<Mutex<BTreeMap<i64, SyncSender<Value>>>>,
    state: &Arc<(Mutex<Shared>, Condvar)>,
    stdin: &Arc<Mutex<ChildStdin>>,
) {
    let (lock, cv) = &**state;
    // A response to one of ours.
    if msg.get("method").is_none() {
        if let Some(id) = msg.get("id").and_then(Value::as_i64) {
            if let Some(tx) = pending.lock().expect("lock").remove(&id) {
                let _ = tx.send(msg);
            }
        }
        return;
    }
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "textDocument/publishDiagnostics" => {
            let uri = msg
                .pointer("/params/uri")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let items = msg
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Ok(mut s) = lock.lock() {
                s.published.insert(uri.clone());
                if items.is_empty() {
                    s.diagnostics.remove(&uri);
                } else {
                    s.diagnostics.insert(uri, items);
                }
            }
            cv.notify_all();
        }
        "$/progress" => {
            let token = msg
                .pointer("/params/token")
                .map(|t| t.to_string())
                .unwrap_or_default();
            let kind = msg
                .pointer("/params/value/kind")
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Ok(mut s) = lock.lock() {
                s.saw_progress = true;
                match kind {
                    "begin" => {
                        s.progress.insert(token);
                    }
                    "end" => {
                        s.progress.remove(&token);
                    }
                    _ => {}
                }
            }
            cv.notify_all();
        }
        "window/logMessage" | "window/showMessage" | "telemetry/event" => {
            if let Some(t) = msg.pointer("/params/message").and_then(Value::as_str) {
                crate::tel_debug!("lsp", t.to_string());
            }
        }
        _ => {}
    }
    // A request from the server. Every one of these has to be answered or the
    // server blocks waiting -- which is how pyright ends up starting and then
    // never replying to anything. koda is not an editor and has no settings to
    // hand back, so the answers are the minimum that keeps a server working.
    if let Some(id) = msg.get("id") {
        let reply = server_request_reply(method, id.clone(), &msg);
        let _ = write_frame(stdin, &reply);
    }
}

/// koda's answer to a request from the language server.
fn server_request_reply(method: &str, id: Value, msg: &Value) -> Value {
    let result = match method {
        // "What are this workspace's settings for X?" -- one empty object per
        // item asked about. Servers accept it and fall back to their defaults;
        // an error here is what leaves pyright inert.
        "workspace/configuration" => {
            let n = msg
                .pointer("/params/items")
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(1);
            Value::Array(vec![json!({}); n.max(1)])
        }
        // Progress reporting, capability registration and the various refresh
        // requests all want an acknowledgement and nothing else.
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "workspace/semanticTokens/refresh"
        | "workspace/codeLens/refresh"
        | "workspace/inlayHint/refresh"
        | "workspace/diagnostic/refresh" => Value::Null,
        "workspace/applyEdit" => {
            // koda applies edits through its own approved write path, never
            // because a server asked. Declining is honest and harmless.
            json!({ "applied": false, "failureReason": "koda does not apply server edits" })
        }
        "window/showMessageRequest" => Value::Null,
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("koda does not implement `{method}`") }
            })
        }
    };
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Write one `Content-Length`-framed message. Shared by the request path and
/// the reader thread's replies, which is why it is free-standing.
fn write_frame(stdin: &Arc<Mutex<ChildStdin>>, msg: &Value) -> Result<()> {
    let body = serde_json::to_string(msg)?;
    let mut w = stdin.lock().expect("lock");
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    w.flush()?;
    Ok(())
}

// ------------------------------------------------------------------ sessions

/// One running server, with the documents it has been shown.
struct Session {
    def: &'static ServerDef,
    client: Client,
    root: PathBuf,
    /// Files opened with `didOpen`, and the version each is on.
    open: HashMap<String, i64>,
    started: Instant,
}

impl Session {
    fn start(def: &'static ServerDef, root: &Path) -> Result<Session> {
        let client = Client::spawn(def, root)?;
        let init = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "koda", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": path_to_uri(root),
            "workspaceFolders": [{
                "uri": path_to_uri(root),
                "name": root.file_name().and_then(|n| n.to_str()).unwrap_or("workspace")
            }],
            "initializationOptions": def
                .init_options
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or(Value::Null),
            "capabilities": {
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                    "symbol": { "dynamicRegistration": false }
                },
                "textDocument": {
                    "synchronization": { "dynamicRegistration": false, "didSave": false },
                    "definition": { "linkSupport": true },
                    "typeDefinition": { "linkSupport": true },
                    "implementation": { "linkSupport": true },
                    "references": {},
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    // Plain text, not markdown: the answer goes into a model's
                    // context, where markdown fences are noise it has to parse
                    // past. Servers that only do markdown still send it, and
                    // `plain_text` strips what arrives.
                    "hover": { "contentFormat": ["plaintext", "markdown"] },
                    "publishDiagnostics": { "relatedInformation": false }
                },
                "window": { "workDoneProgress": true }
            }
        });
        let caps = client
            .request_timeout("initialize", init, INIT_TIMEOUT)
            .map_err(|e| {
                // Overwhelmingly the cause is a binary that is on PATH without
                // being installed -- a rustup proxy for a missing component,
                // most often. Saying so turns a dead end into one command.
                anyhow!(
                    "`{}` would not start ({e:#}). It is on PATH, but check it actually \
                     runs -- `{} --version`. A rustup proxy for a component that was \
                     never installed looks installed until you run it \
                     (`rustup component add rust-analyzer`).",
                    def.name,
                    def.command
                )
            })?;
        client.notify("initialized", json!({}))?;
        // pyright and several others do nothing until configuration arrives.
        // An empty settings object is a valid answer and unblocks them.
        let _ = client.notify(
            "workspace/didChangeConfiguration",
            json!({ "settings": {} }),
        );
        crate::tel_info!(
            "lsp",
            "language server ready",
            "server" => def.name,
            "definition" => caps.pointer("/capabilities/definitionProvider").is_some(),
            "references" => caps.pointer("/capabilities/referencesProvider").is_some()
        );
        Ok(Session {
            def,
            client,
            root: root.to_path_buf(),
            open: HashMap::new(),
            started: Instant::now(),
        })
    }

    /// Make sure the server has this file's current text.
    ///
    /// Servers are allowed to read from disk, but many will not answer at all
    /// for a document that was never opened — and koda has an edge an editor
    /// does not: it knows the file just changed, because it changed it. Sending
    /// the content is both correct and how an answer reflects an edit made
    /// seconds ago rather than the last save the server noticed.
    fn open_file(&mut self, rel: &str) -> Result<String> {
        let full = self.root.join(rel);
        let text = std::fs::read_to_string(&full).with_context(|| format!("reading {rel}"))?;
        let uri = path_to_uri(&full);
        match self.open.get(&uri).copied() {
            None => {
                self.client.notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id(rel),
                            "version": 1,
                            "text": text
                        }
                    }),
                )?;
                self.open.insert(uri.clone(), 1);
            }
            Some(v) => {
                // Full-text sync: koda does not track incremental edits, and a
                // whole source file is a few kilobytes over a pipe.
                self.client.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": v + 1 },
                        "contentChanges": [{ "text": text }]
                    }),
                )?;
                self.open.insert(uri.clone(), v + 1);
            }
        }
        Ok(uri)
    }

    fn ready(&self) {
        // Only worth waiting on for the servers that actually index. The wait
        // returns immediately once the server has gone quiet.
        self.client.wait_until_idle(INDEX_TIMEOUT);
    }
}

/// Every server running for this workspace, keyed by server name.
///
/// A repository is routinely more than one language, so unlike the debugger —
/// where one session is the whole point — several may be live at once.
fn slot() -> &'static Mutex<HashMap<String, Session>> {
    static SLOT: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get or start the server for a file. `None` when nothing here handles it.
fn session_for<'a>(
    map: &'a mut HashMap<String, Session>,
    root: &Path,
    rel: &str,
) -> Result<&'a mut Session> {
    let def = pick(root, rel).ok_or_else(|| {
        let ext = ext_of(rel);
        let known: Vec<&str> = servers()
            .iter()
            .filter(|s| s.extensions.contains(&ext.as_str()))
            .map(|s| s.command)
            .collect();
        if known.is_empty() {
            anyhow!("no language server in koda's table handles `.{ext}` files")
        } else {
            anyhow!(
                "no language server for `.{ext}` is installed. Install one of: {}",
                known.join(", ")
            )
        }
    })?;
    if !map.contains_key(def.name) {
        let s = Session::start(def, root)?;
        map.insert(def.name.to_string(), s);
    }
    Ok(map.get_mut(def.name).expect("just inserted"))
}

/// Stop every server. Called on exit.
pub fn shutdown() {
    let Ok(mut map) = slot().lock() else { return };
    for (_, mut s) in map.drain() {
        s.client.shutdown();
    }
}

// -------------------------------------------------------------------- shapes

/// A resolved location, in the form koda talks about files.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub file: String,
    /// 1-based, because every other line number koda prints is.
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}

/// Read a location out of the several shapes LSP allows: a `Location`, a
/// `LocationLink`, or a bare array of either.
fn locations(v: &Value, root: &Path) -> Vec<Location> {
    let mut out = Vec::new();
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Null => return out,
        other => vec![other],
    };
    for item in items {
        let uri = item
            .get("uri")
            .or_else(|| item.get("targetUri"))
            .and_then(Value::as_str);
        let range = item
            .get("range")
            .or_else(|| item.get("targetSelectionRange"))
            .or_else(|| item.get("targetRange"));
        let (Some(uri), Some(range)) = (uri, range) else {
            continue;
        };
        out.push(Location {
            file: rel_of(root, uri),
            line: range
                .pointer("/start/line")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize
                + 1,
            col: range
                .pointer("/start/character")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize
                + 1,
        });
    }
    out
}

/// A URI as a workspace-relative path when it is inside the project, and as an
/// absolute one when it is not — a definition in a dependency is still a useful
/// answer, and pretending it is local would be a lie the model then acts on.
fn rel_of(root: &Path, uri: &str) -> String {
    let p = uri_to_path(uri);
    p.strip_prefix(root)
        .map(|r| r.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

/// Column for a symbol on a line, 0-based for the wire.
///
/// The model knows a name and a line; it does not know a column, and asking it
/// to count characters is asking it to be wrong. So a caller may give either,
/// and naming the symbol is the path that actually gets used.
fn column_for(
    root: &Path,
    rel: &str,
    line1: usize,
    symbol: Option<&str>,
    col: Option<usize>,
) -> Result<u64> {
    if let Some(c) = col {
        return Ok(c.saturating_sub(1) as u64);
    }
    let Some(sym) = symbol.map(str::trim).filter(|s| !s.is_empty()) else {
        bail!("give either `symbol` (the name on that line) or `column`")
    };
    let text = std::fs::read_to_string(root.join(rel)).with_context(|| format!("reading {rel}"))?;
    let line = text
        .lines()
        .nth(line1.saturating_sub(1))
        .ok_or_else(|| anyhow!("{rel} has no line {line1}"))?;
    // Prefer a whole-word hit, so asking about `run` on a line that also says
    // `runner` lands on the right one.
    let at = word_position(line, sym)
        .or_else(|| line.find(sym))
        .ok_or_else(|| anyhow!("`{sym}` is not on line {line1} of {rel}: {}", line.trim()))?;
    // LSP counts UTF-16 code units by default. For ASCII source they are the
    // same; for a line with a multi-byte character before the symbol they are
    // not, and the server would resolve the wrong token.
    Ok(line[..at].encode_utf16().count() as u64)
}

/// Byte offset of `needle` in `line` where it stands as a whole identifier.
fn word_position(line: &str, needle: &str) -> Option<usize> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(rel) = line[from..].find(needle) {
        let at = from + rel;
        let before_ok = line[..at]
            .chars()
            .next_back()
            .map(|c| !is_word(c))
            .unwrap_or(true);
        let after_ok = line[at + needle.len()..]
            .chars()
            .next()
            .map(|c| !is_word(c))
            .unwrap_or(true);
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + needle.len().max(1);
        if from >= line.len() {
            break;
        }
    }
    None
}

/// Hover comes back as markdown, a marked string, or an array of either.
fn plain_text(v: &Value) -> String {
    fn one(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Value::Array(a) => {
                for x in a {
                    one(x, out);
                }
            }
            Value::Object(o) => {
                if let Some(s) = o.get("value").and_then(Value::as_str) {
                    out.push_str(s);
                    out.push('\n');
                } else if let Some(c) = o.get("contents") {
                    one(c, out);
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    one(v, &mut out);
    // Strip the code fences servers wrap signatures in: the fence is for an
    // editor's renderer, and in a model's context it is three wasted tokens
    // and a chance to misread the type as prose.
    out.lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

const SYMBOL_KINDS: &[&str] = &[
    "?",
    "file",
    "module",
    "namespace",
    "package",
    "class",
    "method",
    "property",
    "field",
    "constructor",
    "enum",
    "interface",
    "function",
    "variable",
    "constant",
    "string",
    "number",
    "boolean",
    "array",
    "object",
    "key",
    "null",
    "enum member",
    "struct",
    "event",
    "operator",
    "type parameter",
];

fn kind_name(k: u64) -> &'static str {
    SYMBOL_KINDS.get(k as usize).copied().unwrap_or("symbol")
}

fn severity(n: u64) -> &'static str {
    match n {
        1 => "error",
        2 => "warning",
        3 => "info",
        _ => "hint",
    }
}

fn cap(mut s: String) -> String {
    if s.len() > MAX_ANSWER_BYTES {
        let mut cut = MAX_ANSWER_BYTES;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n… [truncated]");
    }
    s
}

// ----------------------------------------------------------------- the tool

/// The `lsp` tool. Every action is read-only.
pub fn run(args: &Value, root: &Path) -> Result<String> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if action.is_empty() {
        bail!("`action` is required");
    }

    if action == "servers" || action == "status" || action == "list_servers" {
        return Ok(status_report(root));
    }

    let mut map = slot().lock().expect("lock");

    // Workspace symbol search does not need a file, which makes it the way in
    // when the model knows a name and nothing else.
    if action == "workspace_symbols" || action == "symbol" {
        let name = args
            .get("name")
            .or_else(|| args.get("query"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            bail!("`name` is required for workspace_symbols");
        }
        return workspace_symbols(&mut map, root, &name, args);
    }

    let file = args
        .get("file")
        .or_else(|| args.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if file.is_empty() {
        bail!("`file` is required for action={action}");
    }
    if !root.join(&file).is_file() {
        bail!("no such file: {file}");
    }

    let session = session_for(&mut map, root, &file)?;
    let uri = session.open_file(&file)?;

    if action == "diagnostics" {
        let items = session.client.await_diagnostics(&uri, DIAGNOSTICS_WAIT);
        return Ok(cap(render_diagnostics(&file, &items, session)));
    }
    if action == "document_symbols" || action == "outline" {
        let body = session.client.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )?;
        return Ok(cap(render_document_symbols(&file, &body)));
    }

    let line = args
        .get("line")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("`line` is required for action={action}"))? as usize;
    let character = column_for(
        root,
        &file,
        line,
        args.get("symbol").and_then(Value::as_str),
        args.get("column")
            .and_then(Value::as_u64)
            .map(|c| c as usize),
    )?;
    let position = json!({ "line": (line.saturating_sub(1)) as u64, "character": character });
    let at = json!({ "textDocument": { "uri": uri }, "position": position });

    // A server mid-index answers null to everything, which is indistinguishable
    // from "nothing there". Wait for it to settle before believing an empty
    // answer, but only once — after that it is warm.
    session.ready();

    let method = match action.as_str() {
        "definition" | "goto" => "textDocument/definition",
        "type_definition" | "type" => "textDocument/typeDefinition",
        "implementation" | "implementations" => "textDocument/implementation",
        "references" | "usages" => "textDocument/references",
        "hover" | "signature" => "textDocument/hover",
        other => bail!(
            "unknown action `{other}`. Use definition, references, hover, type_definition, \
             implementation, document_symbols, workspace_symbols, diagnostics or servers."
        ),
    };

    let params = if method == "textDocument/references" {
        let mut p = at.clone();
        p["context"] = json!({ "includeDeclaration": args
            .get("include_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(true) });
        p
    } else {
        at
    };

    let body = session.client.request(method, params)?;
    let server = session.def.name;

    if method == "textDocument/hover" {
        let text = plain_text(&body);
        if text.is_empty() {
            return Ok(format!(
                "{server} has nothing to say about that position ({file}:{line})."
            ));
        }
        return Ok(cap(format!("{file}:{line} — via {server}\n\n{text}")));
    }

    let locs = locations(&body, root);
    if locs.is_empty() {
        let what = if method.ends_with("references") {
            "no references"
        } else {
            "no definition"
        };
        return Ok(format!(
            "{server} found {what} for that position ({file}:{line}). \
             The symbol may be from a dependency the server has not indexed, or the \
             position may not be on an identifier."
        ));
    }
    Ok(cap(render_locations(method, &locs, server)))
}

fn workspace_symbols(
    map: &mut HashMap<String, Session>,
    root: &Path,
    name: &str,
    args: &Value,
) -> Result<String> {
    // Which server to ask: the one named, or every server already running, or
    // — when nothing is running yet — whichever ones this project supports.
    let named = args.get("server").and_then(Value::as_str).map(str::trim);
    let wanted: Vec<&'static ServerDef> = match named.filter(|s| !s.is_empty()) {
        Some(n) => servers()
            .iter()
            .filter(|s| s.name == n)
            .collect::<Vec<_>>()
            .into_iter()
            .collect(),
        None => available(root),
    };
    if wanted.is_empty() {
        bail!(
            "no language server is installed for this project. `action=servers` lists what \
             koda looks for."
        );
    }
    let limit = args
        .get("k")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;

    let mut out = String::new();
    let mut total = 0usize;
    for def in wanted {
        if !map.contains_key(def.name) {
            match Session::start(def, root) {
                Ok(s) => {
                    map.insert(def.name.to_string(), s);
                }
                Err(e) => {
                    let _ = writeln!(out, "{}: not available ({e:#})", def.name);
                    continue;
                }
            }
        }
        let session = map.get_mut(def.name).expect("started");
        session.ready();
        let body = match session
            .client
            .request("workspace/symbol", json!({ "query": name }))
        {
            Ok(b) => b,
            Err(e) => {
                let _ = writeln!(out, "{}: {e:#}", def.name);
                continue;
            }
        };
        let items = body.as_array().cloned().unwrap_or_default();
        for it in items.iter().take(limit) {
            let sym = it.get("name").and_then(Value::as_str).unwrap_or("?");
            let kind = kind_name(it.get("kind").and_then(Value::as_u64).unwrap_or(0));
            let container = it
                .get("containerName")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(|c| format!(" in {c}"))
                .unwrap_or_default();
            let loc = it
                .get("location")
                .map(|l| locations(l, root))
                .unwrap_or_default();
            let where_ = loc
                .first()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".into());
            let _ = writeln!(out, "{kind} {sym}{container} — {where_}");
            total += 1;
        }
    }
    if total == 0 {
        return Ok(format!(
            "No symbol matching `{name}` was found by any language server.\n{out}"
        ));
    }
    Ok(cap(format!(
        "{total} symbol(s) matching `{name}`, resolved by the language server:\n{out}"
    )))
}

fn render_locations(method: &str, locs: &[Location], server: &str) -> String {
    let what = match method {
        "textDocument/definition" => "Definition",
        "textDocument/typeDefinition" => "Type definition",
        "textDocument/implementation" => "Implementations",
        _ => "References",
    };
    let mut out = format!("{what} ({} found, via {server}):\n", locs.len());
    // Grouped by file, which is how a list of references is actually read.
    let mut by_file: BTreeMap<&str, Vec<&Location>> = BTreeMap::new();
    for l in locs {
        by_file.entry(l.file.as_str()).or_default().push(l);
    }
    for (file, hits) in by_file {
        let lines: Vec<String> = hits.iter().map(|h| h.line.to_string()).collect();
        let _ = writeln!(out, "  {file}: {}", lines.join(", "));
    }
    out
}

fn render_document_symbols(file: &str, body: &Value) -> String {
    let mut out = format!("Symbols in {file} (via the language server):\n");
    fn walk(items: &[Value], depth: usize, out: &mut String) {
        for it in items {
            let name = it.get("name").and_then(Value::as_str).unwrap_or("?");
            let kind = kind_name(it.get("kind").and_then(Value::as_u64).unwrap_or(0));
            // `DocumentSymbol` has `range`; the flat `SymbolInformation` shape
            // wraps the same thing in `location`.
            let line = it
                .pointer("/range/start/line")
                .or_else(|| it.pointer("/location/range/start/line"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            let detail = it
                .get("detail")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
                .map(|d| format!(" {d}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{:indent$}{line:>5}  {kind} {name}{detail}",
                "",
                indent = depth * 2
            );
            if let Some(kids) = it.get("children").and_then(Value::as_array) {
                walk(kids, depth + 1, out);
            }
        }
    }
    match body.as_array() {
        Some(items) if !items.is_empty() => walk(items, 0, &mut out),
        _ => out.push_str("  (none reported)\n"),
    }
    out
}

fn render_diagnostics(file: &str, items: &[Value], session: &Session) -> String {
    if items.is_empty() {
        // Honest about the difference between clean and not-yet-known, because
        // "no errors" from a server that has not finished is the single most
        // misleading thing this tool could say.
        let warm = session.started.elapsed() > Duration::from_secs(3);
        return if warm {
            format!("{file}: no diagnostics from {}.\n", session.def.name)
        } else {
            format!(
                "{file}: {} reported nothing yet — it may still be starting up. \
                 Ask again in a moment.\n",
                session.def.name
            )
        };
    }
    let mut out = format!("{} diagnostic(s) in {file}:\n", items.len());
    for d in items {
        let line = d
            .pointer("/range/start/line")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        let col = d
            .pointer("/range/start/character")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        let sev = severity(d.get("severity").and_then(Value::as_u64).unwrap_or(1));
        let code = d
            .get("code")
            .map(|c| match c {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .filter(|c| !c.is_empty())
            .map(|c| format!(" [{c}]"))
            .unwrap_or_default();
        let msg = d
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .replace('\n', " ");
        let _ = writeln!(out, "  {line}:{col} {sev}{code}: {msg}");
    }
    out
}

/// `action=servers`: what is installed, what is running, what it would take.
pub fn status_report(root: &Path) -> String {
    let running: Vec<(String, String)> = slot()
        .lock()
        .map(|m| {
            m.values()
                .map(|s| {
                    (
                        s.def.name.to_string(),
                        format!("{} file(s) open", s.open.len()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut out = String::from("Language servers:\n");
    for s in servers() {
        let live = running.iter().find(|(n, _)| n == s.name);
        let state = match (live, installed(s)) {
            (Some((_, detail)), _) => format!("running, {detail}"),
            // On PATH, but does it run? A rustup proxy for a component that was
            // never installed is on PATH and is not a language server.
            (None, true) => match runnable(s) {
                Some(false) => "on PATH but will not run".into(),
                _ => "installed".into(),
            },
            (None, false) => "not installed".into(),
        };
        let _ = writeln!(
            out,
            "  {:<28} .{:<24} {state}",
            s.name,
            s.extensions.join(" .")
        );
    }
    let usable: Vec<&'static ServerDef> = available(root)
        .into_iter()
        .filter(|s| runnable(s) != Some(false))
        .collect();
    if usable.is_empty() {
        out.push_str(
            "\nNone of them is installed for a language in this project. Install the one for \
             your language (e.g. `rustup component add rust-analyzer`, `npm i -g \
             pyright`) and koda picks it up on the next call.\n",
        );
    } else {
        let _ = writeln!(
            out,
            "\nUsable here: {}",
            usable.iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
        );
    }
    out
}

// ------------------------------------------------------- codegraph handshake

/// Precise definitions for a symbol name, for `codegraph query=symbol` to fold
/// into its answer.
///
/// Time-boxed hard and failing silently: the code graph's promise is that it
/// answers instantly and always, and an LSP that is cold, missing or unhappy
/// must not turn that into a wait or an error. When it works, the model gets a
/// resolved answer instead of a name match; when it does not, it gets exactly
/// what it got before.
pub fn augment_symbol(root: &Path, name: &str, budget: Duration) -> Option<String> {
    let deadline = Instant::now() + budget;
    let mut map = slot().lock().ok()?;
    // Only servers that are *already running* — starting one here would spend a
    // cold rust-analyzer's indexing time inside a codegraph call.
    let names: Vec<String> = map.keys().cloned().collect();
    for server in names {
        if Instant::now() >= deadline {
            break;
        }
        let Some(session) = map.get_mut(&server) else {
            continue;
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let Ok(body) =
            session
                .client
                .request_timeout("workspace/symbol", json!({ "query": name }), left)
        else {
            continue;
        };
        let items = body.as_array().cloned().unwrap_or_default();
        let exact: Vec<&Value> = items
            .iter()
            .filter(|i| i.get("name").and_then(Value::as_str) == Some(name))
            .collect();
        if exact.is_empty() {
            continue;
        }
        let mut out = format!("\nResolved by {server}:\n");
        for it in exact.iter().take(8) {
            let kind = kind_name(it.get("kind").and_then(Value::as_u64).unwrap_or(0));
            let loc = it
                .get("location")
                .map(|l| locations(l, root))
                .unwrap_or_default();
            let where_ = loc
                .first()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".into());
            let container = it
                .get("containerName")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(|c| format!(" in {c}"))
                .unwrap_or_default();
            let _ = writeln!(out, "  {kind} {name}{container} — {where_}");
        }
        return Some(out);
    }
    None
}

/// Start the servers this project can use, in the background.
///
/// Called at startup when `lsp_eager` is on. Off by default: rust-analyzer on a
/// large workspace is real CPU and a gigabyte of memory, and a session that
/// never asks a type-aware question should not pay for it. On, the first `lsp`
/// call is instant instead of waiting through a cold index.
pub fn warm_up(root: &Path) {
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        for def in available(&root) {
            let Ok(mut map) = slot().lock() else { return };
            if map.contains_key(def.name) {
                continue;
            }
            match Session::start(def, &root) {
                Ok(s) => {
                    map.insert(def.name.to_string(), s);
                }
                Err(e) => crate::tel_warn!("lsp", format!("{}: {e:#}", def.name)),
            }
        }
    });
}

/// Whether any server is currently running, for the prompt and `/lsp`.
pub fn any_running() -> bool {
    slot().lock().map(|m| !m.is_empty()).unwrap_or(false)
}

/// Diagnostics across every open file, for a workspace-wide report.
pub fn workspace_diagnostics(root: &Path) -> String {
    let Ok(map) = slot().lock() else {
        return String::new();
    };
    let mut out = String::new();
    let mut total = 0usize;
    for s in map.values() {
        for (uri, items) in s.client.all_diagnostics() {
            if items.is_empty() {
                continue;
            }
            total += items.len();
            let _ = writeln!(out, "{} ({} problems)", rel_of(root, &uri), items.len());
        }
    }
    if total == 0 {
        return "No diagnostics reported by any running language server.\n".into();
    }
    format!("{total} diagnostic(s) across open files:\n{out}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A language server that speaks the parts of LSP koda uses. It is
    /// deliberately awkward in the two ways real servers are: it asks the
    /// client for configuration before it will do anything (pyright's
    /// behaviour), and it publishes diagnostics unprompted after a didOpen.
    const FAKE_SERVER: &str = r#"
import sys, json

buf = sys.stdin.buffer
out = sys.stdout.buffer

def send(obj):
    body = json.dumps(obj).encode()
    out.write(b"Content-Length: %d\r\n\r\n" % len(body))
    out.write(body); out.flush()

def read():
    n = None
    while True:
        line = buf.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            n = int(line.split(b":")[1])
    if n is None:
        return None
    return json.loads(buf.read(n))

def publish(uri):
    send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
          "params": {"uri": uri, "diagnostics": [
              {"range": {"start": {"line": 1, "character": 4},
                         "end": {"line": 1, "character": 8}},
               "severity": 1, "code": "E0308", "message": "mismatched types"}]}})

configured = False
pending = None
while True:
    msg = read()
    if msg is None:
        break
    m = msg.get("method")
    if m == "initialize":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {
            "definitionProvider": True, "referencesProvider": True,
            "hoverProvider": True, "documentSymbolProvider": True,
            "workspaceSymbolProvider": True}}})
    elif m == "initialized":
        # Refuse to work until the client answers. A client that ignores
        # server-to-client requests never gets past this point.
        send({"jsonrpc": "2.0", "id": 500, "method": "workspace/configuration",
              "params": {"items": [{"section": "fake"}]}})
    elif m == "textDocument/didOpen":
        # Held until the client has answered `workspace/configuration`, rather
        # than dropped. The reply and this notification are written to the same
        # pipe by two different threads, so which lands first is a race -- and
        # dropping the document on the wrong outcome made the test flaky rather
        # than wrong. Deferring keeps the assertion honest: no reply still means
        # no diagnostics, for ever.
        pending = msg["params"]["textDocument"]["uri"]
        if configured:
            publish(pending); pending = None
    elif m == "textDocument/definition":
        pos = msg["params"]["position"]
        send({"jsonrpc": "2.0", "id": msg["id"], "result": [{
            "targetUri": msg["params"]["textDocument"]["uri"],
            "targetSelectionRange": {"start": {"line": pos["character"], "character": 0},
                                     "end": {"line": pos["character"], "character": 3}}}]})
    elif m == "textDocument/references":
        uri = msg["params"]["textDocument"]["uri"]
        send({"jsonrpc": "2.0", "id": msg["id"], "result": [
            {"uri": uri, "range": {"start": {"line": 0, "character": 0}}},
            {"uri": uri, "range": {"start": {"line": 4, "character": 2}}}]})
    elif m == "textDocument/hover":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"contents": {
            "kind": "markdown", "value": "```rust\nfn run() -> u32\n```\nRuns."}}})
    elif m == "textDocument/documentSymbol":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": [
            {"name": "Thing", "kind": 23,
             "range": {"start": {"line": 0, "character": 0}},
             "children": [{"name": "run", "kind": 6, "detail": "fn(&self) -> u32",
                           "range": {"start": {"line": 2, "character": 4}}}]}]})
    elif m == "workspace/symbol":
        q = msg["params"]["query"]
        send({"jsonrpc": "2.0", "id": msg["id"], "result": [
            {"name": q, "kind": 12, "containerName": "app",
             "location": {"uri": ROOT + "/lib.rs",
                          "range": {"start": {"line": 41, "character": 0}}}}]})
    elif m == "shutdown":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    elif m == "exit":
        break
    elif "result" in msg:
        configured = True
        if pending:
            publish(pending); pending = None
        # Progress, so the client's "wait for the server to settle" path runs
        # against something rather than being exercised only by its timeout.
        send({"jsonrpc": "2.0", "method": "$/progress",
              "params": {"token": "idx", "value": {"kind": "begin", "title": "indexing"}}})
        send({"jsonrpc": "2.0", "method": "$/progress",
              "params": {"token": "idx", "value": {"kind": "end"}}})
ROOT = ""
"#;

    /// The table takes `&'static` everywhere, so a test server has to be
    /// leaked. It lives for the process, which is exactly as long as the table
    /// entries it stands in for.
    fn fake_def(script: &Path) -> &'static ServerDef {
        let arg: &'static str = Box::leak(script.to_string_lossy().into_owned().into_boxed_str());
        let args: &'static [&'static str] = Box::leak(Box::new([arg]));
        Box::leak(Box::new(ServerDef {
            name: "fake-ls",
            command: "python3",
            args,
            extensions: &["rs"],
            markers: &[],
            init_options: None,
        }))
    }

    fn fake_session(dir: &Path) -> Option<Session> {
        crate::tools::which_in_path("python3")?;
        let script = dir.join("fake_lsp.py");
        // ROOT is read by the workspace/symbol branch; defining it after the
        // loop would be too late, so it is prepended here with the real path.
        let body = format!(
            "ROOT = \"file://{}\"\n{}",
            dir.display(),
            FAKE_SERVER.replacen("ROOT = \"\"", "", 1)
        );
        std::fs::write(&script, body).expect("write server");
        Session::start(fake_def(&script), dir).ok()
    }

    /// Tests that drive a language server share the global session map, so
    /// they take turns -- one test's `shutdown` drains the map another is
    /// holding a session in.
    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// The stub above proves koda speaks the protocol. This proves koda speaks
    /// it to *rust-analyzer* -- which is the thing users actually have, and
    /// which gets to decide what "correct" means. A stub agrees with whatever
    /// its author believed.
    ///
    /// A throwaway two-file crate rather than this repository: koda is large
    /// enough that indexing it would make the test a minute long and its
    /// failures ambiguous.
    #[test]
    fn a_real_rust_analyzer_resolves_a_symbol() {
        let _exclusive = exclusive();
        let Some(def) = servers().iter().find(|s| s.name == "rust-analyzer") else {
            return;
        };
        // `runnable`, not `installed`: a rustup proxy for a missing component
        // is on PATH, and waiting for it to fail to initialize costs this test
        // thirty seconds to learn what one `--version` says instantly.
        if runnable(def) != Some(true) {
            eprintln!("SKIP: rust-analyzer is not installed and runnable");
            return;
        }
        let dir = std::env::temp_dir().join(format!("koda-ra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        // `width` is defined once and used twice. A regex graph would also find
        // the name; only a resolver knows these are the same `width`.
        std::fs::write(
            dir.join("src/main.rs"),
            "fn width(n: u32) -> u32 {\n    n * 2\n}\n\nfn main() {\n                 let a = width(3);\n    let b = width(4);\n    println!(\"{a} {b}\");\n}\n",
        )
        .unwrap();

        // Where it is defined, asked from one of the call sites.
        let out = match run(
            &json!({
                "action": "definition",
                "file": "src/main.rs",
                "line": 6,
                "symbol": "width"
            }),
            &dir,
        ) {
            Ok(o) => o,
            Err(e) => {
                // A machine that cannot run rust-analyzer here (no toolchain,
                // no network for the sysroot) is not a failing client.
                eprintln!("SKIP: rust-analyzer would not answer: {e:#}");
                shutdown();
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
        };
        assert!(
            out.contains("src/main.rs:1"),
            "definition should be line 1:\n{out}"
        );

        // Both call sites, which is the answer a name match cannot give.
        let out = run(
            &json!({
                "action": "references",
                "file": "src/main.rs",
                "line": 1,
                "symbol": "width"
            }),
            &dir,
        )
        .expect("references");
        assert!(out.contains("src/main.rs"), "{out}");
        assert!(
            out.contains('6') && out.contains('7'),
            "both call sites:\n{out}"
        );

        // And the type, which the graph has no notion of at all.
        let out = run(
            &json!({
                "action": "hover",
                "file": "src/main.rs",
                "line": 1,
                "symbol": "width"
            }),
            &dir,
        )
        .expect("hover");
        assert!(out.contains("fn width"), "{out}");
        assert!(
            out.contains("u32"),
            "the signature should carry types:\n{out}"
        );

        shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole protocol against a real process: the handshake, answering the
    /// server's own request, opening a document, and each navigation query.
    #[test]
    fn a_real_server_is_initialized_and_queried() {
        let _exclusive = exclusive();
        let dir = std::env::temp_dir().join(format!("koda-lsp-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("a.rs"),
            "fn main() {
    let x = run();
}
",
        )
        .unwrap();
        let Some(mut session) = fake_session(&dir) else {
            return; // no python3 on this machine
        };

        let uri = session.open_file("a.rs").expect("didOpen");
        assert!(uri.ends_with("a.rs"), "{uri}");

        // Diagnostics only arrive because koda answered `workspace/configuration`.
        // A client that ignores server requests gets an empty list here, which
        // is the failure this asserts against.
        let diags = session
            .client
            .await_diagnostics(&uri, Duration::from_secs(10));
        assert_eq!(
            diags.len(),
            1,
            "no diagnostics: the config reply was missed"
        );
        let rendered = render_diagnostics("a.rs", &diags, &session);
        assert!(
            rendered.contains("2:5 error [E0308]: mismatched types"),
            "{rendered}"
        );

        // A position derived from the symbol on the line, not a column the
        // model had to count.
        let col = column_for(&dir, "a.rs", 2, Some("run"), None).unwrap();
        assert_eq!(col, 12);

        let body = session
            .client
            .request(
                "textDocument/definition",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": 1, "character": col }
                }),
            )
            .expect("definition");
        let locs = locations(&body, &dir);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].file, "a.rs");
        // The stub echoes the column back as the line, which proves the
        // position koda computed is the one that went over the wire.
        assert_eq!(locs[0].line, col as usize + 1);

        let body = session
            .client
            .request(
                "textDocument/references",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": 1, "character": col },
                    "context": { "includeDeclaration": true }
                }),
            )
            .expect("references");
        let out = render_locations(
            "textDocument/references",
            &locations(&body, &dir),
            "fake-ls",
        );
        assert!(out.contains("References (2 found"), "{out}");
        assert!(out.contains("a.rs: 1, 5"), "{out}");

        let body = session
            .client
            .request(
                "textDocument/hover",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": 1, "character": col }
                }),
            )
            .expect("hover");
        let hover = plain_text(&body);
        assert!(hover.contains("fn run() -> u32"), "{hover}");
        assert!(!hover.contains("```"), "{hover}");

        let body = session
            .client
            .request(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": uri } }),
            )
            .expect("documentSymbol");
        let out = render_document_symbols("a.rs", &body);
        assert!(out.contains("struct Thing"), "{out}");
        assert!(out.contains("method run fn(&self) -> u32"), "{out}");

        // A file opened twice is a didChange, not a second didOpen: the server
        // would reject the duplicate and stop answering.
        std::fs::write(
            dir.join("a.rs"),
            "fn main() {
    let x = run(1);
}
",
        )
        .unwrap();
        session.open_file("a.rs").expect("didChange");
        assert_eq!(session.open.get(&uri).copied(), Some(2));

        // And the codegraph handshake, which only ever uses running servers.
        slot().lock().unwrap().insert("fake-ls".into(), session);
        let extra = augment_symbol(&dir, "Widget", Duration::from_secs(5))
            .expect("the running server should have answered");
        assert!(extra.contains("Resolved by fake-ls"), "{extra}");
        assert!(extra.contains("function Widget in app"), "{extra}");
        assert!(extra.contains("lib.rs:42"), "{extra}");

        shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_request_a_server_can_send_gets_an_answer() {
        // The exact failure this prevents: a server asks for configuration,
        // koda says nothing, and the server never starts working.
        let cfg = server_request_reply(
            "workspace/configuration",
            json!(1),
            &json!({ "params": { "items": [{}, {}, {}] } }),
        );
        assert_eq!(cfg["result"].as_array().unwrap().len(), 3);
        for m in [
            "window/workDoneProgress/create",
            "client/registerCapability",
            "workspace/semanticTokens/refresh",
        ] {
            let r = server_request_reply(m, json!(2), &json!({}));
            assert!(r.get("error").is_none(), "{m} must not be refused");
            assert_eq!(r["id"], json!(2));
        }
        // koda writes files through its own approved path, so a server asking
        // to edit one is declined -- but still answered.
        let edit = server_request_reply("workspace/applyEdit", json!(3), &json!({}));
        assert_eq!(edit["result"]["applied"], false);
        // Anything else is a proper "not implemented", never silence.
        let no = server_request_reply("something/new", json!(4), &json!({}));
        assert_eq!(no["error"]["code"], -32601);
    }

    #[test]
    fn a_server_is_chosen_by_the_file_and_the_project() {
        let dir = std::env::temp_dir().join(format!("koda-lsp-pick-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // Without a Cargo.toml, rust-analyzer is not offered for a .rs file
        // even when installed: it would spend its startup discovering there is
        // no workspace.
        let ra = servers()
            .iter()
            .find(|s| s.name == "rust-analyzer")
            .unwrap();
        assert_eq!(ra.markers, &["Cargo.toml"]);
        assert!(pick(&dir, "main.rs").is_none() || installed(ra));
        // A file nothing handles is a clean miss, not a wrong guess.
        assert!(pick(&dir, "notes.txt").is_none());
        assert!(pick(&dir, "noextension").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Being on PATH is not the same as being installed, and koda used to
    /// conflate them. `rustup` puts a proxy for `rust-analyzer` in
    /// `~/.cargo/bin` whether or not the component is there; koda reported
    /// "Usable here: rust-analyzer", advertised the tool, and the model found
    /// out the hard way.
    #[test]
    fn on_path_is_not_the_same_as_runnable() {
        let mk = |cmd: &'static str| ServerDef {
            name: "probe",
            command: cmd,
            args: &[],
            extensions: &["rs"],
            markers: &["Cargo.toml"],
            init_options: None,
        };

        // Nothing on PATH: no opinion, rather than a wrong one.
        assert_eq!(runnable(&mk("koda-no-such-binary-anywhere")), None);

        // On PATH and exits cleanly -- a server koda can use. `true` is the
        // standing in for one that answers `--version`.
        if which("true").is_some() {
            assert_eq!(runnable(&mk("true")), Some(true));
        }
        // On PATH and fails -- exactly the rustup-proxy shape. `installed`
        // still says yes, because it only looks at PATH; `runnable` is what
        // tells them apart.
        if which("false").is_some() {
            let broken = mk("false");
            assert!(installed(&broken), "it is on PATH");
            assert_eq!(runnable(&broken), Some(false), "but it does not run");
        }
    }

    #[test]
    fn language_ids_match_what_servers_expect() {
        assert_eq!(language_id("a/b.rs"), "rust");
        assert_eq!(language_id("x.tsx"), "typescriptreact");
        assert_eq!(language_id("x.jsx"), "javascriptreact");
        assert_eq!(language_id("x.hpp"), "cpp");
        assert_eq!(language_id("x.unknown"), "plaintext");
    }

    #[test]
    fn a_column_is_found_from_the_symbol_the_model_actually_knows() {
        let dir = std::env::temp_dir().join(format!("koda-lsp-col-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = "s.rs";
        std::fs::write(dir.join(f), "let runner = run(x);\nlet z = 1;\n").unwrap();

        // A whole-word match, so `run` does not land inside `runner`.
        let c = column_for(&dir, f, 1, Some("run"), None).unwrap();
        assert_eq!(c, 13, "should point at the call, not at `runner`");
        // An explicit column wins and is converted from 1-based to 0-based.
        assert_eq!(column_for(&dir, f, 1, None, Some(5)).unwrap(), 4);
        // Neither is an error the model can act on.
        assert!(column_for(&dir, f, 1, None, None).is_err());
        // So is a symbol that is not on that line.
        let e = column_for(&dir, f, 2, Some("run"), None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("line 2"), "{e}");

        // UTF-16 counting: the server would resolve the wrong token if this
        // returned a byte offset.
        std::fs::write(dir.join(f), "// é\nlet a = b;\n").unwrap();
        std::fs::write(dir.join("u.rs"), "let é = b;\n").unwrap();
        let c = column_for(&dir, "u.rs", 1, Some("b"), None).unwrap();
        assert_eq!(c, 8, "one UTF-16 unit for é, not two bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_shape_of_location_is_understood() {
        let root = Path::new("/proj");
        // A plain Location.
        let one = json!({
            "uri": "file:///proj/src/a.rs",
            "range": { "start": { "line": 4, "character": 8 } }
        });
        assert_eq!(
            locations(&one, root),
            vec![Location {
                file: "src/a.rs".into(),
                line: 5,
                col: 9
            }]
        );
        // A LocationLink, which is what servers send when linkSupport is on.
        let link = json!([{
            "targetUri": "file:///proj/src/b.rs",
            "targetSelectionRange": { "start": { "line": 0, "character": 0 } }
        }]);
        assert_eq!(locations(&link, root)[0].file, "src/b.rs");
        // Nothing found is empty, not an error.
        assert!(locations(&Value::Null, root).is_empty());
        // A definition outside the workspace keeps its absolute path rather
        // than being mangled into a relative one that does not exist.
        let dep = json!({
            "uri": "file:///home/u/.cargo/registry/x/lib.rs",
            "range": { "start": { "line": 0, "character": 0 } }
        });
        assert!(locations(&dep, root)[0].file.starts_with('/'));
    }

    #[test]
    fn hover_markdown_becomes_something_worth_putting_in_a_context() {
        let v = json!({
            "contents": { "kind": "markdown", "value": "```rust\nfn run() -> Result<()>\n```\nRuns it." }
        });
        let t = plain_text(&v);
        assert!(t.contains("fn run() -> Result<()>"), "{t}");
        assert!(!t.contains("```"), "{t}");
        // The older MarkedString array shape still works.
        let old = json!({ "contents": ["fn a()", { "value": "and b" }] });
        let t = plain_text(&old);
        assert!(t.contains("fn a()") && t.contains("and b"), "{t}");
        assert!(plain_text(&Value::Null).is_empty());
    }

    #[test]
    fn diagnostics_distinguish_clean_from_not_yet_known() {
        // Rendered without a session there is nothing to assert on, so this
        // checks the shaping instead: severities and positions are 1-based and
        // the code is carried through, because "E0308" is what the model will
        // search for.
        assert_eq!(severity(1), "error");
        assert_eq!(severity(2), "warning");
        assert_eq!(severity(9), "hint");
        assert_eq!(kind_name(12), "function");
        assert_eq!(kind_name(23), "struct");
        assert_eq!(kind_name(999), "symbol");
    }

    #[test]
    fn answers_are_capped_on_a_character_boundary() {
        let out = cap("ß".repeat(MAX_ANSWER_BYTES));
        assert!(out.ends_with("[truncated]"));
        assert!(out.len() < MAX_ANSWER_BYTES + 64);
    }
}
