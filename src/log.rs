//! Telemetry: a structured event log.
//!
//! Two sinks, one call site. Everything lands in an in-memory ring the TUI can
//! show with `/logs`, and (unless disabled) in a rotating file so a crash or a
//! closed session is still diagnosable. This is what makes it acceptable to
//! hide raw errors from the user: the detail is never lost, just moved.

use std::collections::VecDeque;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const RING_CAPACITY: usize = 1000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn label(&self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
    fn from_str(s: &str) -> Level {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" | "trace" => Level::Debug,
            "warn" | "warning" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Info,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Seconds since the process started, which is what you actually want when
    /// reading a session back.
    pub at: f64,
    pub level: Level,
    /// Coarse area: "http", "tool", "agent", "ui", "subagent".
    pub area: &'static str,
    pub message: String,
    /// Extra key=value context, kept out of the message so it stays scannable.
    pub fields: Vec<(String, String)>,
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:>8.3}  {:<5} {:<8} {}",
            self.at,
            self.level.label(),
            self.area,
            self.message
        )?;
        for (k, v) in &self.fields {
            write!(f, " {k}={v}")?;
        }
        Ok(())
    }
}

struct Sink {
    ring: VecDeque<Entry>,
    file: Option<std::fs::File>,
    path: Option<PathBuf>,
    written: u64,
    min: Level,
    started: std::time::Instant,
    /// Bumped on every push so the UI can tell whether to re-render.
    version: u64,
}

fn sink() -> &'static Mutex<Sink> {
    static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();
    SINK.get_or_init(|| {
        Mutex::new(Sink {
            ring: VecDeque::with_capacity(RING_CAPACITY),
            file: None,
            path: None,
            written: 0,
            min: Level::Info,
            started: std::time::Instant::now(),
            version: 0,
        })
    })
}

pub fn log_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("koda");
        }
    }
    dirs::home_dir()
        .map(|h| h.join(".local").join("state").join("koda"))
        .unwrap_or_else(|| PathBuf::from(".koda"))
}

pub fn log_path() -> PathBuf {
    log_dir().join("koda.log")
}

/// Open the file sink. Called once at startup; failures are non-fatal because
/// logging must never be the reason the app does not start.
pub fn init(level: &str, to_file: bool) {
    let mut s = sink().lock().unwrap();
    s.min = Level::from_str(level);
    s.started = std::time::Instant::now();
    if !to_file {
        return;
    }
    let dir = log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = log_path();
    // Rotate rather than grow without bound.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_FILE_BYTES {
            let _ = std::fs::rename(&path, path.with_extension("log.1"));
        }
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        s.written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        s.file = Some(f);
        s.path = Some(path);
    }
}

pub fn push(level: Level, area: &'static str, message: impl Into<String>, fields: Vec<(String, String)>) {
    let Ok(mut s) = sink().lock() else { return };
    if level < s.min {
        return;
    }
    let entry = Entry {
        at: s.started.elapsed().as_secs_f64(),
        level,
        area,
        message: message.into(),
        fields,
    };
    if let Some(f) = s.file.as_mut() {
        let line = format!("{} {entry}\n", wall_clock());
        if writeln!(f, "{}", line.trim_end()).is_ok() {
            s.written += line.len() as u64;
        }
    }
    if s.ring.len() == RING_CAPACITY {
        s.ring.pop_front();
    }
    s.ring.push_back(entry);
    s.version += 1;
}

fn wall_clock() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Local wall clock is not worth a time crate here; UTC seconds-of-day is
    // enough to correlate with anything else on the machine.
    let sod = secs % 86_400;
    format!("{:02}:{:02}:{:02}", sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// Most recent entries at or above `min`, oldest first.
pub fn recent(min: Level, limit: usize) -> Vec<Entry> {
    let Ok(s) = sink().lock() else { return Vec::new() };
    let mut out: Vec<Entry> = s
        .ring
        .iter()
        .filter(|e| e.level >= min)
        .cloned()
        .collect();
    if out.len() > limit {
        out.drain(..out.len() - limit);
    }
    out
}

pub fn version() -> u64 {
    sink().lock().map(|s| s.version).unwrap_or(0)
}

pub fn counts() -> (usize, usize) {
    let Ok(s) = sink().lock() else { return (0, 0) };
    (
        s.ring.iter().filter(|e| e.level == Level::Warn).count(),
        s.ring.iter().filter(|e| e.level == Level::Error).count(),
    )
}

pub fn file_path() -> Option<PathBuf> {
    sink().lock().ok().and_then(|s| s.path.clone())
}

#[cfg(test)]
pub fn reset_for_test() {
    let mut s = sink().lock().unwrap();
    s.ring.clear();
    s.file = None;
    s.min = Level::Debug;
    s.version = 0;
}

/// `debug!("http", "sent request", "model" => m, "bytes" => n)`
#[macro_export]
macro_rules! tel {
    ($level:expr, $area:literal, $msg:expr) => {
        $crate::log::push($level, $area, $msg, Vec::new())
    };
    ($level:expr, $area:literal, $msg:expr, $($k:literal => $v:expr),+ $(,)?) => {
        $crate::log::push(
            $level,
            $area,
            $msg,
            vec![$(($k.to_string(), format!("{}", $v))),+],
        )
    };
}

#[macro_export]
macro_rules! tel_debug {
    ($area:literal, $msg:expr $(, $k:literal => $v:expr)* $(,)?) => {
        $crate::tel!($crate::log::Level::Debug, $area, $msg $(, $k => $v)*)
    };
}
#[macro_export]
macro_rules! tel_info {
    ($area:literal, $msg:expr $(, $k:literal => $v:expr)* $(,)?) => {
        $crate::tel!($crate::log::Level::Info, $area, $msg $(, $k => $v)*)
    };
}
#[macro_export]
macro_rules! tel_warn {
    ($area:literal, $msg:expr $(, $k:literal => $v:expr)* $(,)?) => {
        $crate::tel!($crate::log::Level::Warn, $area, $msg $(, $k => $v)*)
    };
}
#[macro_export]
macro_rules! tel_error {
    ($area:literal, $msg:expr $(, $k:literal => $v:expr)* $(,)?) => {
        $crate::tel!($crate::log::Level::Error, $area, $msg $(, $k => $v)*)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sink is process-global, so these tests must not interleave.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn ring_keeps_recent_entries_and_filters_by_level() {
        let _g = guard();
        reset_for_test();
        push(Level::Debug, "test", "quiet", vec![]);
        push(Level::Info, "test", "normal", vec![]);
        push(Level::Error, "test", "loud", vec![]);

        let all = recent(Level::Debug, 100);
        assert_eq!(all.len(), 3);
        let errors = recent(Level::Error, 100);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "loud");
    }

    #[test]
    fn limit_returns_the_newest() {
        let _g = guard();
        reset_for_test();
        for i in 0..10 {
            push(Level::Info, "test", format!("n{i}"), vec![]);
        }
        let last = recent(Level::Debug, 3);
        assert_eq!(last.len(), 3);
        assert_eq!(last[2].message, "n9");
    }

    #[test]
    fn fields_render_as_key_values() {
        let _g = guard();
        reset_for_test();
        push(
            Level::Warn,
            "http",
            "retrying",
            vec![("attempt".into(), "2".into())],
        );
        let e = &recent(Level::Warn, 1)[0];
        assert!(e.to_string().contains("attempt=2"), "{e}");
        assert!(e.to_string().contains("warn"));
    }

    #[test]
    fn counts_split_warnings_from_errors() {
        let _g = guard();
        reset_for_test();
        push(Level::Warn, "t", "w", vec![]);
        push(Level::Error, "t", "e1", vec![]);
        push(Level::Error, "t", "e2", vec![]);
        assert_eq!(counts(), (1, 2));
    }

    #[test]
    fn version_advances_so_the_ui_can_diff() {
        let _g = guard();
        reset_for_test();
        let before = version();
        push(Level::Info, "t", "x", vec![]);
        assert!(version() > before);
    }
}
