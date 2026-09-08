//! Debug Adapter Protocol client: breakpoints, stepping, and program state.
//!
//! koda could already capture what it sent a model (`debug.rs`); it could not
//! tell you what a *program* was doing. This is the other half — a real
//! debugger the model drives: set a breakpoint, run to it, look at the frames
//! and variables, evaluate an expression, step, continue.
//!
//! It speaks [DAP] to an off-the-shelf adapter (debugpy, lldb-dap, dlv, …) over
//! stdio, which is how every editor does it, so the debugger for a language is
//! whatever that language's community already ships.
//!
//! Shape, following the structure oh-my-pi arrived at (their `dap/client.ts` +
//! `dap/session.ts`): a [`Client`] owning the adapter process and its framing,
//! and a [`Session`] holding what the conversation needs to remember — the
//! capabilities, the breakpoints per file, where it stopped. One session at a
//! time, process-global, because "the debugger" is a singular thing to a user
//! and two of them stopped at once is a question nobody wants to answer.
//!
//! Threading: the reader is a plain thread and requests block, because koda's
//! sync tools already run inside `spawn_blocking`. No async here.
//!
//! [DAP]: https://microsoft.github.io/debug-adapter-protocol/

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long any single DAP request may take before we give up on it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for the program to come back to a stop after `continue` or
/// a step. Running past this is not an error — a program that is still running
/// is a fact to report, not a failure.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Cap on captured program output, oldest dropped first.
const MAX_OUTPUT_BYTES: usize = 128 * 1024;

// ------------------------------------------------------------------ adapters

/// A debug adapter koda knows how to start.
#[derive(Debug, Clone)]
pub struct Adapter {
    pub name: &'static str,
    /// The executable and its arguments.
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// File extensions this adapter debugs, for picking one automatically.
    pub extensions: &'static [&'static str],
    /// Merged under the caller's launch arguments.
    pub launch_defaults: &'static [(&'static str, Value)],
}

/// The built-in registry. Deliberately short: an adapter that is not installed
/// is worse than absent, because it fails at launch instead of at selection.
/// Every one of these is the standard adapter for its language, and every one
/// of them speaks DAP on **stdio** -- which is the only transport here.
///
/// That last point is why `codelldb` is not in this list even though it debugs
/// the same languages as `lldb-dap`: it is a TCP adapter (`--port`), and an
/// entry that spawns fine and then never answers is worse than no entry at
/// all. Add it, or anything else, in config once koda speaks TCP.
pub fn adapters() -> &'static [Adapter] {
    static REG: OnceLock<Vec<Adapter>> = OnceLock::new();
    REG.get_or_init(|| {
        vec![
            Adapter {
                name: "debugpy",
                command: "python3",
                args: &["-m", "debugpy.adapter"],
                extensions: &["py"],
                launch_defaults: &[],
            },
            Adapter {
                name: "lldb-dap",
                command: "lldb-dap",
                args: &[],
                extensions: &["rs", "c", "cc", "cpp", "m", "swift", "zig"],
                launch_defaults: &[],
            },
            Adapter {
                name: "dlv",
                command: "dlv",
                args: &["dap"],
                extensions: &["go"],
                launch_defaults: &[],
            },
            Adapter {
                name: "js-debug-adapter",
                command: "js-debug-adapter",
                args: &[],
                extensions: &["js", "mjs", "cjs", "ts"],
                launch_defaults: &[],
            },
        ]
    })
}

/// Whether an adapter's executable is actually on this machine.
fn installed(a: &Adapter) -> bool {
    if a.name == "debugpy" {
        // The command is python; what has to exist is the module — and *which*
        // python matters, since a machine can easily have several and only one
        // of them carrying debugpy. That is a real failure, met while setting
        // this up: installing a second python moved `python3` on PATH and the
        // adapter vanished, correctly reported as "not installed" but with no
        // hint as to which interpreter had been asked.
        return Command::new(a.command)
            .args(["-c", "import debugpy.adapter"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }
    which(a.command).is_some()
}

/// Where an adapter binary is, if anywhere.
///
/// A bare name goes through the shared PATH lookup, which also checks the
/// executable bit -- an adapter that is merely a readable file is not one koda
/// can start. A path with a separator in it is taken as given.
fn which(bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        let p = PathBuf::from(bin);
        return p.is_file().then_some(p);
    }
    crate::tools::which_in_path(bin)
}

/// Choose an adapter for a program: the caller's pick if it named one,
/// otherwise the first installed adapter that handles the file's extension.
pub fn pick_adapter(program: &str, named: Option<&str>) -> Result<&'static Adapter> {
    if let Some(name) = named.map(str::trim).filter(|s| !s.is_empty()) {
        return adapters()
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| anyhow!("no adapter named `{name}`. Known: {}", adapter_names()));
    }
    let ext = Path::new(program)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let matching = candidates(&ext);
    if matching.is_empty() {
        bail!(
            "no debug adapter for a `.{ext}` program. Pass `adapter` explicitly; known: {}",
            adapter_names()
        );
    }
    matching
        .iter()
        .find(|a| installed(a))
        .copied()
        .ok_or_else(|| {
            // Name the interpreter or binary that was actually probed. With two
            // pythons on PATH, "install debugpy" is advice the user may have
            // followed already — for the other one.
            let tried: Vec<String> = matching
                .iter()
                .map(|a| match which(a.command) {
                    Some(p) => format!("{} (found at {})", a.name, p.display()),
                    None => format!("{} (`{}` is not on PATH)", a.name, a.command),
                })
                .collect();
            anyhow!(
                "no debug adapter for a `.{ext}` program is installed. Tried: {}",
                tried.join("; ")
            )
        })
}

/// The adapters that handle a file extension, best first. A fact about the
/// language, separate from whether this machine has any of them.
pub fn candidates(ext: &str) -> Vec<&'static Adapter> {
    adapters()
        .iter()
        .filter(|a| a.extensions.contains(&ext))
        .collect()
}

fn adapter_names() -> String {
    adapters()
        .iter()
        .map(|a| a.name)
        .collect::<Vec<_>>()
        .join(", ")
}

// -------------------------------------------------------------------- client

/// A request that has been sent but not yet answered.
pub struct Pending {
    command: String,
    seq: i64,
    rx: std::sync::mpsc::Receiver<Value>,
}

/// Where the debuggee is.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Started, not stopped at anything.
    Running,
    Stopped {
        thread_id: i64,
        reason: String,
        text: Option<String>,
    },
    Exited(i64),
    Terminated,
}

/// State the reader thread writes and callers read.
struct Shared {
    status: Status,
    /// Bumped on every `stopped` event. A caller notes it *before* sending a
    /// continue or a step, so a stop that lands while the request is still in
    /// flight cannot be missed -- which is the race that otherwise burns the
    /// whole timeout on a program that already stopped.
    stop_gen: u64,
    output: VecDeque<String>,
    output_bytes: usize,
    /// Set when the adapter's stdout closes: nothing more is coming.
    closed: bool,
    /// The adapter has said it is ready to be configured.
    initialized: bool,
}

impl Shared {
    fn push_output(&mut self, s: String) {
        self.output_bytes += s.len();
        self.output.push_back(s);
        while self.output_bytes > MAX_OUTPUT_BYTES {
            match self.output.pop_front() {
                Some(dropped) => self.output_bytes -= dropped.len(),
                None => break,
            }
        }
    }
}

/// The adapter process and its message loop.
pub struct Client {
    child: Child,
    stdin: Mutex<ChildStdin>,
    seq: AtomicI64,
    pending: Arc<Mutex<BTreeMap<i64, SyncSender<Value>>>>,
    state: Arc<(Mutex<Shared>, Condvar)>,
}

impl Client {
    /// Start an adapter and its reader thread.
    pub fn spawn(adapter: &Adapter, cwd: &Path) -> Result<Client> {
        Client::spawn_cmd(adapter.command, adapter.args, cwd)
    }

    /// Start any process that speaks DAP on stdio. Separate from [`spawn`] so a
    /// test can drive the real client against a stand-in adapter.
    pub fn spawn_cmd(command: &str, args: &[&str], cwd: &Path) -> Result<Client> {
        let mut child = Command::new(command)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting debug adapter `{command}`"))?;

        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");

        let pending: Arc<Mutex<BTreeMap<i64, SyncSender<Value>>>> = Arc::default();
        let state = Arc::new((
            Mutex::new(Shared {
                status: Status::Running,
                stop_gen: 0,
                output: VecDeque::new(),
                output_bytes: 0,
                closed: false,
                initialized: false,
            }),
            Condvar::new(),
        ));

        // Reader: frames in, responses matched, events folded into state.
        {
            let pending = pending.clone();
            let state = state.clone();
            std::thread::spawn(move || read_loop(stdout, pending, state));
        }
        // The adapter's own stderr is diagnostics, not protocol. Keep it in the
        // output ring so a failed launch says why.
        {
            let state = state.clone();
            std::thread::spawn(move || {
                let mut lines = BufReader::new(stderr).lines();
                while let Some(Ok(l)) = lines.next() {
                    let (lock, cv) = &*state;
                    if let Ok(mut s) = lock.lock() {
                        s.push_output(format!("[adapter] {l}\n"));
                    }
                    cv.notify_all();
                }
            });
        }

        Ok(Client {
            child,
            stdin: Mutex::new(stdin),
            seq: AtomicI64::new(1),
            pending,
            state,
        })
    }

    /// Send a request and wait for its response body.
    pub fn request(&self, command: &str, arguments: Value) -> Result<Value> {
        self.request_timeout(command, arguments, REQUEST_TIMEOUT)
    }

    /// Send a request and hand back the channel its response will arrive on.
    ///
    /// Needed because DAP's launch handshake is not request/response: an
    /// adapter answers `launch` only after the client has finished configuring
    /// (breakpoints, then `configurationDone`), and configuring only starts
    /// once the adapter says `initialized`. Waiting for the launch response
    /// before sending `configurationDone` deadlocks both sides -- which is
    /// exactly what real debugpy does when you get this wrong.
    pub fn send(&self, command: &str, arguments: Value) -> Result<Pending> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = sync_channel(1);
        self.pending.lock().expect("lock").insert(seq, tx);
        let mut msg = json!({ "seq": seq, "type": "request", "command": command });
        if !arguments.is_null() {
            msg["arguments"] = arguments;
        }
        if let Err(e) = self.write(&msg) {
            self.pending.lock().expect("lock").remove(&seq);
            return Err(e);
        }
        Ok(Pending {
            command: command.to_string(),
            seq,
            rx,
        })
    }

    fn request_timeout(&self, command: &str, arguments: Value, wait: Duration) -> Result<Value> {
        let pending = self.send(command, arguments)?;
        self.await_pending(pending, wait)
    }

    /// Wait for a response already in flight.
    pub fn await_pending(&self, pending: Pending, wait: Duration) -> Result<Value> {
        let Pending { command, seq, rx } = pending;
        match rx.recv_timeout(wait) {
            Ok(resp) => {
                if resp.get("success").and_then(Value::as_bool) == Some(true) {
                    Ok(resp.get("body").cloned().unwrap_or(Value::Null))
                } else {
                    let why = resp
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("request failed");
                    bail!("`{command}` failed: {why}")
                }
            }
            Err(_) => {
                self.pending.lock().expect("lock").remove(&seq);
                if self.state.0.lock().expect("lock").closed {
                    bail!("`{command}`: the debug adapter exited")
                }
                bail!("`{command}` timed out after {}s", wait.as_secs())
            }
        }
    }

    fn write(&self, msg: &Value) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        let mut w = self.stdin.lock().expect("lock");
        write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
        w.flush()?;
        Ok(())
    }

    pub fn status(&self) -> Status {
        self.state.0.lock().expect("lock").status.clone()
    }

    pub fn stop_generation(&self) -> u64 {
        self.state.0.lock().expect("lock").stop_gen
    }

    /// Wait for a stop newer than `from_gen`, or for the program to end.
    ///
    /// Returns the status either way: still running past the timeout is a
    /// normal answer ("it is running"), not an error.
    pub fn wait_for_stop(&self, from_gen: u64, wait: Duration) -> Status {
        let (lock, cv) = &*self.state;
        let deadline = Instant::now() + wait;
        let mut s = lock.lock().expect("lock");
        loop {
            let settled = s.stop_gen > from_gen
                || matches!(s.status, Status::Exited(_) | Status::Terminated)
                || s.closed;
            if settled {
                return s.status.clone();
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return s.status.clone();
            };
            let (guard, _) = cv.wait_timeout(s, left).expect("lock");
            s = guard;
        }
    }

    /// Wait for the adapter's `initialized` event, which is its invitation to
    /// send breakpoints. Returns false if it never came: a few adapters skip it,
    /// and refusing to launch over a missing courtesy helps nobody.
    pub fn wait_for_initialized(&self, wait: Duration) -> bool {
        let (lock, cv) = &*self.state;
        let deadline = Instant::now() + wait;
        let mut s = lock.lock().expect("lock");
        loop {
            if s.initialized || s.closed {
                return s.initialized;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (guard, _) = cv.wait_timeout(s, left).expect("lock");
            s = guard;
        }
    }

    /// Captured program output, oldest first.
    pub fn output(&self) -> String {
        let s = self.state.0.lock().expect("lock");
        s.output.iter().cloned().collect::<Vec<_>>().join("")
    }

    /// Stop the adapter, politely then not.
    pub fn shutdown(&mut self) {
        let _ = self.request_timeout("disconnect", json!({}), Duration::from_secs(2));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Parse `Content-Length` framed messages until the stream ends.
fn read_loop(
    stdout: std::process::ChildStdout,
    pending: Arc<Mutex<BTreeMap<i64, SyncSender<Value>>>>,
    state: Arc<(Mutex<Shared>, Condvar)>,
) {
    let mut r = BufReader::new(stdout);
    loop {
        // Headers, to the blank line.
        let mut len: Option<usize> = None;
        let mut header = String::new();
        loop {
            header.clear();
            match r.read_line(&mut header) {
                Ok(0) => {
                    close(&state);
                    return;
                }
                Ok(_) => {}
                Err(_) => {
                    close(&state);
                    return;
                }
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
        // A header block with no length is an adapter printing to stdout. Skip
        // it and resync rather than stalling on the same junk for ever.
        let Some(len) = len else { continue };
        let mut buf = vec![0u8; len];
        if r.read_exact(&mut buf).is_err() {
            close(&state);
            return;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&buf) else {
            // One malformed message must not kill the reader; the next is
            // still well framed.
            continue;
        };
        dispatch(msg, &pending, &state);
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
) {
    match msg.get("type").and_then(Value::as_str) {
        Some("response") => {
            let seq = msg.get("request_seq").and_then(Value::as_i64).unwrap_or(-1);
            if let Some(tx) = pending.lock().expect("lock").remove(&seq) {
                let _ = tx.send(msg);
            }
        }
        Some("event") => {
            let (lock, cv) = &**state;
            let event = msg.get("event").and_then(Value::as_str).unwrap_or("");
            let body = msg.get("body").cloned().unwrap_or(Value::Null);
            if let Ok(mut s) = lock.lock() {
                match event {
                    "stopped" => {
                        s.status = Status::Stopped {
                            thread_id: body.get("threadId").and_then(Value::as_i64).unwrap_or(1),
                            reason: body
                                .get("reason")
                                .and_then(Value::as_str)
                                .unwrap_or("stopped")
                                .to_string(),
                            text: body
                                .get("description")
                                .or_else(|| body.get("text"))
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        };
                        s.stop_gen += 1;
                    }
                    "initialized" => s.initialized = true,
                    "continued" => s.status = Status::Running,
                    "exited" => {
                        s.status = Status::Exited(
                            body.get("exitCode").and_then(Value::as_i64).unwrap_or(0),
                        )
                    }
                    "terminated" => s.status = Status::Terminated,
                    "output" => {
                        // Only what the program itself printed. Adapters also
                        // send `console` and `telemetry` output about
                        // themselves, which is noise in a program's log.
                        let category = body
                            .get("category")
                            .and_then(Value::as_str)
                            .unwrap_or("stdout");
                        if matches!(category, "stdout" | "stderr") {
                            if let Some(o) = body.get("output").and_then(Value::as_str) {
                                s.push_output(o.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
            cv.notify_all();
        }
        // Reverse requests. Nothing here needs a terminal, so decline rather
        // than leave the adapter waiting for an answer that never comes.
        Some("request") => {
            let seq = msg.get("seq").and_then(Value::as_i64).unwrap_or(0);
            let command = msg.get("command").and_then(Value::as_str).unwrap_or("");
            let _ = (seq, command);
        }
        _ => {}
    }
}

// ------------------------------------------------------------------- session

/// One source breakpoint, as the model asked for it.
#[derive(Debug, Clone)]
struct Bp {
    line: i64,
    /// Only stop when this expression is true.
    condition: Option<String>,
    /// Only stop on some of the hits: `>5`, `%10`, or a bare count. The adapter
    /// counts, which is the whole point -- a condition evaluated in-process is
    /// far cheaper than stopping and continuing a thousand times.
    hit_condition: Option<String>,
    /// A logpoint: print this instead of stopping. `{expr}` interpolates.
    /// The cheapest debugging there is -- a print statement you did not have to
    /// edit the file to add, or remove afterwards.
    log_message: Option<String>,
}

impl Bp {
    fn to_dap(&self) -> Value {
        let mut v = json!({ "line": self.line });
        if let Some(c) = &self.condition {
            v["condition"] = json!(c);
        }
        if let Some(h) = &self.hit_condition {
            v["hitCondition"] = json!(h);
        }
        if let Some(m) = &self.log_message {
            v["logMessage"] = json!(m);
        }
        v
    }
}

/// The live debug session: the adapter, and what the conversation has to
/// remember between tool calls.
pub struct Session {
    client: Client,
    adapter: String,
    program: String,
    /// The workspace, so paths can go back to the model in the short form it
    /// used to ask for them.
    root: PathBuf,
    /// Source breakpoints per file. DAP replaces a file's whole set on every
    /// `setBreakpoints`, so the set has to be kept here to add one without
    /// silently dropping the others.
    breakpoints: BTreeMap<String, Vec<Bp>>,
    /// Break on entry to these functions, by name. Kept for the same reason as
    /// the source set: DAP replaces the whole list on every call.
    function_breakpoints: Vec<String>,
}

/// The one session. A debugger is singular to a user, and two programs stopped
/// at once is a question nobody wants to be asked.
fn slot() -> &'static Mutex<Option<Session>> {
    static SLOT: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

impl Session {
    /// Attach to a program that is already running.
    ///
    /// The other half of `launch`: a server you started yourself, a process
    /// that is already wedged, something running under a supervisor. The
    /// arguments an adapter needs differ wildly -- a pid here, a port there --
    /// so anything the caller passes in `attach_args` is merged over the two
    /// koda knows how to name.
    fn attach(
        adapter: &Adapter,
        cwd: &Path,
        pid: Option<i64>,
        port: Option<i64>,
        extra: Value,
    ) -> Result<Session> {
        let mut args = json!({ "request": "attach" });
        if let Some(p) = pid {
            args["processId"] = json!(p);
        }
        if let Some(p) = port {
            // debugpy and js-debug both take the address this way.
            args["connect"] = json!({ "host": "127.0.0.1", "port": p });
        }
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                args[k] = v.clone();
            }
        }
        Session::start(adapter, cwd, "attach", args, "(attached)")
    }

    /// Start a program under a debugger and run it to its first stop.
    fn launch(
        adapter: &Adapter,
        program: &str,
        cwd: &Path,
        args: Vec<Value>,
        stop_on_entry: bool,
    ) -> Result<Session> {
        let mut launch = json!({
            // Several adapters read the request kind out of the arguments as
            // well as the command, and refuse the launch without it.
            "request": "launch",
            "program": program,
            "cwd": cwd.to_string_lossy(),
            "args": args,
            "stopOnEntry": stop_on_entry,
            "noDebug": false,
            // debugpy and js-debug both want to be told where output goes;
            // internalConsole keeps it on the DAP `output` event, which is the
            // only place koda can read it.
            "console": "internalConsole",
        });
        for (k, v) in adapter.launch_defaults {
            if launch.get(*k).is_none() {
                launch[*k] = v.clone();
            }
        }
        Session::start(adapter, cwd, "launch", launch, program)
    }

    /// The handshake both `launch` and `attach` go through.
    ///
    /// DAP's opening is not request/response, and getting that wrong is a
    /// deadlock rather than an error: an adapter answers `launch`/`attach` only
    /// once the client has finished configuring, and configuring may only start
    /// once the adapter has said `initialized`. So the request goes out and
    /// stays in flight while we wait to be invited, configure, and only then
    /// collect the response.
    fn start(
        adapter: &Adapter,
        cwd: &Path,
        command: &str,
        arguments: Value,
        program: &str,
    ) -> Result<Session> {
        let client = Client::spawn(adapter, cwd)?;
        let caps = client.request(
            "initialize",
            json!({
                "clientID": "koda",
                "clientName": "koda",
                "adapterID": adapter.name,
                "pathFormat": "path",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "supportsRunInTerminalRequest": false,
                "locale": "en",
            }),
        )?;
        let wants_configuration_done = caps
            .get("supportsConfigurationDoneRequest")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let s = Session {
            client,
            adapter: adapter.name.to_string(),
            program: program.to_string(),
            root: cwd.to_path_buf(),
            breakpoints: BTreeMap::new(),
            function_breakpoints: Vec::new(),
        };
        // Note the stop generation first: with `stopOnEntry` the stop can
        // arrive before the response does.
        let gen = s.client.stop_generation();
        let pending = s.client.send(command, arguments)?;
        s.client.wait_for_initialized(Duration::from_secs(10));
        if wants_configuration_done {
            let _ = s.client.request("configurationDone", json!({}));
        }
        s.client.await_pending(pending, REQUEST_TIMEOUT)?;
        s.client.wait_for_stop(gen, STOP_TIMEOUT);
        Ok(s)
    }

    /// Push a file's breakpoints to the adapter, replacing its set.
    fn sync_breakpoints(&mut self, path: &str) -> Result<Value> {
        let want = self.breakpoints.get(path).cloned().unwrap_or_default();
        let bps: Vec<Value> = want.iter().map(Bp::to_dap).collect();
        self.client.request(
            "setBreakpoints",
            json!({
                "source": { "path": path },
                "breakpoints": bps,
                "sourceModified": false,
            }),
        )
    }

    /// Push the function breakpoints, replacing the adapter's list.
    fn sync_function_breakpoints(&mut self) -> Result<Value> {
        let bps: Vec<Value> = self
            .function_breakpoints
            .iter()
            .map(|n| json!({ "name": n }))
            .collect();
        self.client
            .request("setFunctionBreakpoints", json!({ "breakpoints": bps }))
    }

    fn thread_id(&self) -> i64 {
        match self.client.status() {
            Status::Stopped { thread_id, .. } => thread_id,
            _ => 1,
        }
    }

    /// Where it is now, in one line.
    fn where_now(&self) -> String {
        match self.client.status() {
            Status::Stopped { reason, text, .. } => {
                let mut s = format!("stopped ({reason})");
                if let Some(t) = text {
                    s.push_str(&format!(": {t}"));
                }
                match self.top_frame() {
                    Some((name, file, line)) => {
                        format!("{s} at {}:{line} in {name}", rel(&self.root, &file))
                    }
                    None => s,
                }
            }
            Status::Running => "running".into(),
            // These read after "it is …", so they have to be predicates.
            Status::Exited(c) => format!("no longer running (exit code {c})"),
            Status::Terminated => "no longer running (the session ended)".into(),
        }
    }

    /// The innermost frame, as the adapter describes it.
    fn top_frame_value(&self) -> Option<Value> {
        let body = self
            .client
            .request(
                "stackTrace",
                json!({ "threadId": self.thread_id(), "startFrame": 0, "levels": 1 }),
            )
            .ok()?;
        arr(&body, "stackFrames").into_iter().next()
    }

    fn top_frame(&self) -> Option<(String, String, i64)> {
        let f = self.top_frame_value()?;
        Some((
            text_at(&f, "name").to_string(),
            source_path(&f).to_string(),
            int_at(&f, "line"),
        ))
    }
}

// --------------------------------------------------------------- the actions

/// Actions that only look at a stopped program. They ask for read approval
/// rather than exec, because reading a stack frame changes nothing.
pub const READONLY_ACTIONS: &[&str] = &[
    "status",
    "breakpoints",
    "threads",
    "stack_trace",
    "scopes",
    "variables",
    "evaluate",
    "output",
    "list_adapters",
];

/// Whether this `debug` call needs the loud approval.
pub fn action_is_mutating(action: &str) -> bool {
    !READONLY_ACTIONS.contains(&action)
}

/// How many of the breakpoints in a `setBreakpoints` response the adapter could
/// actually bind. The difference between "set" and "will ever hit".
fn verified_count(body: &Value) -> usize {
    arr(body, "breakpoints")
        .iter()
        .filter(|b| b.get("verified").and_then(Value::as_bool) == Some(true))
        .count()
}

/// The DAP response shapes, read the same way everywhere. Spelled out inline
/// they drifted -- three defaults of `"?"`, two of `""` -- for fields that mean
/// the same thing in every message.
/// A stack frame's file, which DAP nests one level down.
fn source_path(frame: &Value) -> &str {
    frame
        .get("source")
        .and_then(|s| s.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("?")
}

fn text_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn int_at(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn arr(v: &Value, key: &str) -> Vec<Value> {
    v.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn need(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("`{key}` is required for this action"))
}

/// Shorten a path for the model: relative to the workspace when it is inside.
fn rel(root: &Path, p: &str) -> String {
    Path::new(p)
        .strip_prefix(root)
        .map(|r| r.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string())
}

/// Resolve a path against the workspace so the model can pass a relative one.
fn abs(root: &Path, p: &str) -> String {
    let path = Path::new(p);
    if path.is_absolute() {
        p.to_string()
    } else {
        root.join(path).to_string_lossy().to_string()
    }
}

/// Run one `debug` action. Returns the text the model sees.
pub fn run(args: &Value, root: &Path) -> Result<String> {
    let action = need(args, "action")?;
    if action == "list_adapters" {
        let mut out = String::from("Debug adapters koda knows:\n");
        for a in adapters() {
            out.push_str(&format!(
                "- {:<18} .{:<22} {}\n",
                a.name,
                a.extensions.join(" ."),
                if installed(a) {
                    "installed"
                } else {
                    "not installed"
                }
            ));
        }
        return Ok(out);
    }

    let mut guard = slot().lock().expect("lock");

    if action == "launch" {
        if let Some(mut old) = guard.take() {
            old.client.shutdown();
        }
        let program = abs(root, &need(args, "program")?);
        if !Path::new(&program).exists() {
            bail!("no such program: {program}");
        }
        let adapter = pick_adapter(&program, args.get("adapter").and_then(Value::as_str))?;
        let prog_args: Vec<Value> = args
            .get("args")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // Stop at the first line unless told otherwise: a launch that runs to
        // completion before a breakpoint can be set is the common first mistake.
        let stop_on_entry = args
            .get("stop_on_entry")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let s = Session::launch(adapter, &program, root, prog_args, stop_on_entry)?;
        let where_now = s.where_now();
        let name = s.adapter.clone();
        *guard = Some(s);
        let shown = rel(root, &program);
        return Ok(format!(
            "Launched {shown} under {name}. It is {where_now}.\n\
             Set breakpoints with action=set_breakpoint, then continue."
        ));
    }

    if action == "attach" {
        if let Some(mut old) = guard.take() {
            old.client.shutdown();
        }
        let pid = args.get("pid").and_then(Value::as_i64);
        let port = args.get("port").and_then(Value::as_i64);
        if pid.is_none() && port.is_none() && args.get("attach_args").is_none() {
            bail!("attach needs a `pid`, a `port`, or adapter-specific `attach_args`.");
        }
        let adapter = pick_adapter(
            args.get("program").and_then(Value::as_str).unwrap_or(""),
            args.get("adapter").and_then(Value::as_str),
        )?;
        let extra = args.get("attach_args").cloned().unwrap_or(Value::Null);
        let s = Session::attach(adapter, root, pid, port, extra)?;
        let where_now = s.where_now();
        let name = s.adapter.clone();
        *guard = Some(s);
        return Ok(format!(
            "Attached with {name}. It is {where_now}.\n\
             Set breakpoints, then continue or pause."
        ));
    }

    let Some(session) = guard.as_mut() else {
        bail!("no debug session. Start one with action=launch, program=<file>.")
    };

    match action.as_str() {
        "set_breakpoint" => {
            let file = abs(root, &need(args, "file")?);
            let line = args
                .get("line")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("`line` is required"))?;
            let text = |k: &str| {
                args.get(k)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let bp = Bp {
                line,
                condition: text("condition"),
                hit_condition: text("hit_condition"),
                log_message: text("log_message"),
            };
            let logpoint = bp.log_message.is_some();
            let set = session.breakpoints.entry(file.clone()).or_default();
            set.retain(|b| b.line != line);
            set.push(bp);
            set.sort_by_key(|b| b.line);
            let body = session.sync_breakpoints(&file)?;
            // The adapter says whether it could bind each one; an unverified
            // breakpoint is the difference between "set" and "will ever hit".
            let verified = verified_count(&body);
            Ok(format!(
                "{} at {}:{line}. {} of {} breakpoints in this file are verified.",
                if logpoint { "Logpoint" } else { "Breakpoint" },
                rel(root, &file),
                verified,
                session.breakpoints.get(&file).map(Vec::len).unwrap_or(0)
            ))
        }
        "remove_breakpoint" => {
            let file = abs(root, &need(args, "file")?);
            let line = args.get("line").and_then(Value::as_i64);
            match line {
                Some(l) => {
                    session
                        .breakpoints
                        .entry(file.clone())
                        .or_default()
                        .retain(|b| b.line != l);
                }
                None => {
                    session.breakpoints.remove(&file);
                }
            }
            session.sync_breakpoints(&file)?;
            Ok(match line {
                Some(l) => format!("Removed the breakpoint at {file}:{l}."),
                None => format!("Removed every breakpoint in {file}."),
            })
        }
        "set_function_breakpoint" => {
            let name = need(args, "name")?;
            if !session.function_breakpoints.contains(&name) {
                session.function_breakpoints.push(name.clone());
            }
            let body = session.sync_function_breakpoints()?;
            let verified = verified_count(&body);
            Ok(format!(
                "Breaking on entry to `{name}`. {verified} of {} function breakpoints verified.",
                session.function_breakpoints.len()
            ))
        }
        "remove_function_breakpoint" => {
            let name = need(args, "name")?;
            session.function_breakpoints.retain(|n| n != &name);
            session.sync_function_breakpoints()?;
            Ok(format!("No longer breaking on `{name}`."))
        }
        "breakpoints" => {
            let mut out = String::new();
            for (file, set) in &session.breakpoints {
                for b in set {
                    let mut line = format!("- {}:{}", rel(&session.root, file), b.line);
                    if let Some(c) = &b.condition {
                        line.push_str(&format!(" when {c}"));
                    }
                    if let Some(h) = &b.hit_condition {
                        line.push_str(&format!(" hits {h}"));
                    }
                    if let Some(m) = &b.log_message {
                        line.push_str(&format!(" logs \"{m}\""));
                    }
                    out.push_str(&line);
                    out.push('\n');
                }
            }
            for f in &session.function_breakpoints {
                out.push_str(&format!("- fn {f}\n"));
            }
            Ok(if out.is_empty() {
                "No breakpoints set.".into()
            } else {
                format!("Breakpoints:\n{out}")
            })
        }
        "continue" | "step_over" | "step_in" | "step_out" | "pause" => {
            let tid = session.thread_id();
            // Note where the stop counter is *before* asking, so a stop that
            // lands while the request is in flight still counts.
            let gen = session.client.stop_generation();
            // Only the command differs; every one of these asks about a thread.
            let cmd = match action.as_str() {
                "step_over" => "next",
                "step_in" => "stepIn",
                "step_out" => "stepOut",
                other => other, // continue, pause
            };
            session.client.request(cmd, json!({ "threadId": tid }))?;
            session.client.wait_for_stop(gen, STOP_TIMEOUT);
            let out = session.client.output();
            let mut msg = format!("{}: it is {}.", action, session.where_now());
            if !out.trim().is_empty() {
                msg.push_str(&format!(
                    "\n\nProgram output so far:\n{}",
                    tail(&out, 4_000)
                ));
            }
            Ok(msg)
        }
        "status" => Ok(format!(
            "{} debugging {} — it is {}.",
            session.adapter,
            session.program,
            session.where_now()
        )),
        "threads" => {
            let body = session.client.request("threads", json!({}))?;
            let list = arr(&body, "threads");
            let mut out = format!("{} thread(s):\n", list.len());
            for t in list {
                out.push_str(&format!("- {} {}\n", int_at(&t, "id"), text_at(&t, "name")));
            }
            Ok(out)
        }
        "stack_trace" => {
            let levels = args.get("levels").and_then(Value::as_i64).unwrap_or(20);
            let body = session.client.request(
                "stackTrace",
                json!({ "threadId": session.thread_id(), "startFrame": 0, "levels": levels }),
            )?;
            let mut out = String::from("Stack (innermost first):\n");
            for f in arr(&body, "stackFrames") {
                out.push_str(&format!(
                    "- #{} {} at {}:{}\n",
                    int_at(&f, "id"),
                    text_at(&f, "name"),
                    rel(&session.root, source_path(&f)),
                    int_at(&f, "line"),
                ));
            }
            out.push_str("\nUse the frame id with action=scopes.");
            Ok(out)
        }
        "scopes" => {
            let frame = frame_id(session, args)?;
            let body = session
                .client
                .request("scopes", json!({ "frameId": frame }))?;
            let scopes = body
                .get("scopes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut out = format!("Scopes in frame {frame}:\n");
            for s in scopes {
                out.push_str(&format!(
                    "- {} (variables reference {})\n",
                    s.get("name").and_then(Value::as_str).unwrap_or("?"),
                    s.get("variablesReference")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                ));
            }
            out.push_str("\nUse a reference with action=variables.");
            Ok(out)
        }
        "variables" => {
            let reference = args
                .get("reference")
                .and_then(Value::as_i64)
                .ok_or_else(|| anyhow!("`reference` is required (from action=scopes)"))?;
            let body = session
                .client
                .request("variables", json!({ "variablesReference": reference }))?;
            let vars = body
                .get("variables")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut out = format!("{} variable(s):\n", vars.len());
            for v in vars {
                let child = v
                    .get("variablesReference")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                out.push_str(&format!(
                    "- {} = {}{}\n",
                    v.get("name").and_then(Value::as_str).unwrap_or("?"),
                    truncate(v.get("value").and_then(Value::as_str).unwrap_or(""), 300),
                    if child > 0 {
                        format!("   (expand with reference {child})")
                    } else {
                        String::new()
                    }
                ));
            }
            Ok(out)
        }
        "evaluate" => {
            let expr = need(args, "expression")?;
            let mut req = json!({ "expression": expr, "context": "repl" });
            if let Ok(f) = frame_id(session, args) {
                req["frameId"] = json!(f);
            }
            let body = session.client.request("evaluate", req)?;
            Ok(format!(
                "{expr} = {}",
                body.get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("(no value)")
            ))
        }
        "output" => {
            let out = session.client.output();
            if out.trim().is_empty() {
                Ok("The program has produced no output yet.".into())
            } else {
                Ok(format!("Program output:\n{}", tail(&out, 8_000)))
            }
        }
        "terminate" => {
            let mut s = guard.take().expect("checked");
            s.client.shutdown();
            Ok("Debug session ended.".into())
        }
        other => bail!(
            "unknown debug action `{other}`. Try: launch, attach, set_breakpoint, \
             set_function_breakpoint, remove_breakpoint, remove_function_breakpoint, \
             breakpoints, continue, step_over, step_in, step_out, pause, stack_trace, \
             threads, scopes, variables, evaluate, output, status, terminate, \
             list_adapters."
        ),
    }
}

/// The frame to act in: the one asked for, or the innermost.
fn frame_id(session: &Session, args: &Value) -> Result<i64> {
    if let Some(f) = args.get("frame_id").and_then(Value::as_i64) {
        return Ok(f);
    }
    session
        .top_frame_value()
        .map(|f| int_at(&f, "id"))
        .ok_or_else(|| anyhow!("no stack frame — the program is not stopped"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// The last `max` bytes: a program's most recent output is the interesting end.
fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let cut = s.len() - max;
    let mut start = cut;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…\n{}", &s[start..])
}

/// Shut any session down — called when koda exits.
pub fn shutdown() {
    if let Ok(mut g) = slot().lock() {
        if let Some(mut s) = g.take() {
            s.client.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_adapter_is_chosen_by_what_the_program_is() {
        // Which adapter handles a language is a fact about the language.
        assert_eq!(candidates("py").first().unwrap().name, "debugpy");
        assert_eq!(candidates("go").first().unwrap().name, "dlv");
        assert!(candidates("rs").iter().any(|a| a.name == "lldb-dap"));
        assert!(candidates("txt").is_empty());

        // An explicit choice wins, and a wrong one says so rather than
        // silently debugging with something else.
        assert_eq!(
            pick_adapter("main.py", Some("lldb-dap")).unwrap().name,
            "lldb-dap"
        );
        assert!(pick_adapter("main.py", Some("nonesuch")).is_err());

        // A file nothing can debug is a clear error, not a default…
        let err = pick_adapter("notes.txt", None).unwrap_err().to_string();
        assert!(err.contains(".txt"), "{err}");
        // When nothing is installed, the message has to name what was probed:
        // with two pythons on PATH, "install debugpy" may be advice the user
        // already followed, for the other one.
        if let Err(e) = pick_adapter("main.go", None) {
            let m = e.to_string();
            assert!(m.contains("dlv"), "{m}");
            assert!(m.contains("PATH") || m.contains("found at"), "{m}");
        }
        // …and so is one whose adapter is not on this machine: the message has
        // to name what to install, since that is the whole remedy.
        match pick_adapter("main.py", None) {
            Ok(a) => assert_eq!(a.name, "debugpy"),
            Err(e) => assert!(e.to_string().contains("python3"), "{e}"),
        }
    }

    /// Looking at a stopped program is not the same act as running one, and
    /// asking permission for every variable would make stepping a dialogue.
    #[test]
    fn reading_state_does_not_need_the_loud_approval() {
        for a in ["stack_trace", "variables", "evaluate", "output", "status"] {
            assert!(!action_is_mutating(a), "{a} should be read-only");
        }
        for a in [
            "launch",
            "continue",
            "step_over",
            "set_breakpoint",
            "terminate",
        ] {
            assert!(action_is_mutating(a), "{a} should need approval");
        }
    }

    #[test]
    fn the_output_ring_is_bounded_and_keeps_the_end() {
        let mut s = Shared {
            status: Status::Running,
            stop_gen: 0,
            output: VecDeque::new(),
            output_bytes: 0,
            closed: false,
            initialized: false,
        };
        for i in 0..30_000 {
            s.push_output(format!("line {i}\n"));
        }
        assert!(s.output_bytes <= MAX_OUTPUT_BYTES, "{}", s.output_bytes);
        let all: String = s.output.iter().cloned().collect();
        assert!(all.contains("line 29999"), "the newest output must survive");
        assert!(!all.contains("line 0\n"), "the oldest is what gets dropped");
    }

    #[test]
    fn tail_keeps_the_end_and_never_splits_a_character() {
        assert_eq!(tail("short", 100), "short");
        let long = "é".repeat(4_000);
        let out = tail(&long, 200);
        assert!(out.len() <= 205, "{}", out.len());
        assert!(out.ends_with('é'));
    }

    // ---------------------------------------------------------------- e2e

    /// A stand-in debug adapter: real DAP framing, scripted answers.
    ///
    /// It deliberately emits `stopped` *before* the `continue` response, which
    /// is the race the stop-generation counter exists for -- an adapter that
    /// answers that fast used to burn the whole timeout waiting for a stop that
    /// had already happened.
    const FAKE_ADAPTER: &str = r#"
import json, sys

def send(msg):
    body = json.dumps(msg)
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n{body}".encode())
    sys.stdout.buffer.flush()

def reply(req, body=None, success=True):
    send({"seq": 0, "type": "response", "request_seq": req["seq"],
          "success": success, "command": req["command"], "body": body or {}})

def event(name, body=None):
    send({"seq": 0, "type": "event", "event": name, "body": body or {}})

while True:
    header = b""
    while not header.endswith(b"\r\n\r\n"):
        c = sys.stdin.buffer.read(1)
        if not c: sys.exit(0)
        header += c
    n = int([l for l in header.decode().split("\r\n") if l.lower().startswith("content-length")][0].split(":")[1])
    req = json.loads(sys.stdin.buffer.read(n))
    cmd = req.get("command")
    if cmd == "initialize":
        reply(req, {"supportsConfigurationDoneRequest": True})
        event("initialized")
    elif cmd == "launch":
        event("output", {"category": "stdout", "output": "starting up\n"})
        event("stopped", {"reason": "entry", "threadId": 1, "description": "at the first line"})
        reply(req)
    elif cmd == "setBreakpoints":
        bps = req["arguments"]["breakpoints"]
        # Echo back what was asked for, so the client's own encoding is checked.
        sys.stderr.write(json.dumps(bps) + "\n"); sys.stderr.flush()
        reply(req, {"breakpoints": [{"verified": True, "line": b["line"]} for b in bps]})
    elif cmd == "setFunctionBreakpoints":
        names = [b["name"] for b in req["arguments"]["breakpoints"]]
        reply(req, {"breakpoints": [{"verified": True, "name": n} for n in names]})
    elif cmd == "attach":
        event("stopped", {"reason": "pause", "threadId": 1})
        reply(req)
    elif cmd == "continue":
        # The stop arrives BEFORE the response: the race the client must survive.
        event("output", {"category": "stdout", "output": "working\n"})
        event("stopped", {"reason": "breakpoint", "threadId": 1})
        reply(req, {"allThreadsContinued": True})
    elif cmd == "threads":
        reply(req, {"threads": [{"id": 1, "name": "main"}]})
    elif cmd == "stackTrace":
        reply(req, {"stackFrames": [
            {"id": 7, "name": "compute", "line": 12, "source": {"path": "/app/main.py"}},
            {"id": 8, "name": "main", "line": 30, "source": {"path": "/app/main.py"}}]})
    elif cmd == "scopes":
        reply(req, {"scopes": [{"name": "Locals", "variablesReference": 100}]})
    elif cmd == "variables":
        reply(req, {"variables": [
            {"name": "total", "value": "42", "variablesReference": 0},
            {"name": "rows", "value": "[...]", "variablesReference": 101}]})
    elif cmd == "evaluate":
        reply(req, {"result": "1764"})
    elif cmd in ("configurationDone", "next", "stepIn", "stepOut", "pause"):
        reply(req)
    elif cmd == "disconnect":
        reply(req); sys.exit(0)
    else:
        reply(req, success=False)
"#;

    fn python() -> Option<PathBuf> {
        which("python3")
    }

    fn fake_session(dir: &Path) -> Session {
        let script = dir.join("fake_dap.py");
        std::fs::write(&script, FAKE_ADAPTER).expect("write adapter");
        let client = Client::spawn_cmd("python3", &[script.to_str().unwrap()], dir)
            .expect("spawn fake adapter");
        client
            .request(
                "initialize",
                json!({ "adapterID": "fake", "linesStartAt1": true }),
            )
            .expect("initialize");
        let gen = client.stop_generation();
        client
            .request("launch", json!({ "program": "main.py" }))
            .expect("launch");
        let _ = client.request("configurationDone", json!({}));
        client.wait_for_stop(gen, Duration::from_secs(5));
        Session {
            client,
            adapter: "fake".into(),
            program: "main.py".into(),
            root: dir.to_path_buf(),
            breakpoints: BTreeMap::new(),
            function_breakpoints: Vec::new(),
        }
    }

    /// The whole loop against a real process: handshake, breakpoint, continue
    /// into a stop, and read the frame -- which is the thing the model does.
    #[test]
    fn a_session_launches_breaks_continues_and_reports_where_it_is() {
        let Some(_) = python() else {
            eprintln!("skipping: no python3 to run the stand-in adapter");
            return;
        };
        let dir = std::env::temp_dir().join(format!("koda-dap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp");
        let mut s = fake_session(&dir);

        // The launch handshake left it stopped, and the output event was kept.
        assert!(
            matches!(s.client.status(), Status::Stopped { .. }),
            "{:?}",
            s.client.status()
        );
        assert!(s.client.output().contains("starting up"));
        assert!(
            s.where_now().contains("/app/main.py:12"),
            "{}",
            s.where_now()
        );

        // A breakpoint round-trips and comes back verified.
        s.breakpoints.insert(
            "/app/main.py".into(),
            vec![Bp {
                line: 12,
                condition: None,
                hit_condition: None,
                log_message: None,
            }],
        );
        let body = s.sync_breakpoints("/app/main.py").expect("setBreakpoints");
        assert_eq!(body["breakpoints"][0]["verified"], json!(true), "{body}");

        // Continue, where the adapter reports the stop before the response.
        let gen = s.client.stop_generation();
        s.client
            .request("continue", json!({ "threadId": 1 }))
            .expect("continue");
        let status = s.client.wait_for_stop(gen, Duration::from_secs(5));
        match status {
            Status::Stopped { reason, .. } => assert_eq!(reason, "breakpoint"),
            other => panic!("expected a breakpoint stop, got {other:?}"),
        }
        assert!(s.client.output().contains("working"));

        // And the state a model would ask for.
        let frame = frame_id(&s, &json!({})).expect("frame");
        assert_eq!(frame, 7, "the innermost frame is the default");
        let vars = s
            .client
            .request("variables", json!({ "variablesReference": 100 }))
            .expect("variables");
        assert_eq!(vars["variables"][0]["name"], json!("total"));

        s.client.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The three ways to say "stop here" all have to reach the adapter in the
    /// shape DAP expects, and survive the whole-list replace on every call.
    #[test]
    fn breakpoints_carry_conditions_hit_counts_and_logpoints() {
        let Some(_) = python() else { return };
        let dir = std::env::temp_dir().join(format!("koda-dap-bp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp");
        let mut s = fake_session(&dir);

        s.breakpoints.insert(
            "/app/main.py".into(),
            vec![
                Bp {
                    line: 12,
                    condition: Some("total > 40".into()),
                    hit_condition: None,
                    log_message: None,
                },
                Bp {
                    line: 20,
                    condition: None,
                    hit_condition: Some(">5".into()),
                    log_message: Some("total is {total}".into()),
                },
            ],
        );
        let body = s.sync_breakpoints("/app/main.py").expect("setBreakpoints");
        assert_eq!(body["breakpoints"].as_array().map(Vec::len), Some(2));

        // What actually went over the wire, echoed back by the adapter.
        let sent = s.client.output();
        assert!(sent.contains("\"condition\": \"total > 40\""), "{sent}");
        assert!(sent.contains("\"hitCondition\": \">5\""), "{sent}");
        assert!(sent.contains("\"logMessage\""), "{sent}");

        // Function breakpoints are a separate DAP list, kept separately.
        s.function_breakpoints.push("average".into());
        let body = s
            .sync_function_breakpoints()
            .expect("setFunctionBreakpoints");
        assert_eq!(body["breakpoints"][0]["name"], json!("average"));

        s.client.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A timeout must be a timeout, not a hang: an adapter that never answers
    /// has to give the caller an error it can report.
    #[test]
    fn a_silent_adapter_times_out_rather_than_hanging() {
        let Some(_) = python() else { return };
        let dir = std::env::temp_dir().join(format!("koda-dap-mute-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp");
        let script = dir.join("mute.py");
        std::fs::write(&script, "import time\ntime.sleep(30)\n").expect("write");
        let client =
            Client::spawn_cmd("python3", &[script.to_str().unwrap()], &dir).expect("spawn");
        let started = Instant::now();
        let err = client
            .request_timeout("initialize", json!({}), Duration::from_millis(400))
            .unwrap_err()
            .to_string();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
