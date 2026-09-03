//! Developer debug capture — koda's take on oh-my-pi's request-debug feature.
//!
//! When debug mode is on (config `debug = true`, `/debug`, or `KODA_DEBUG=1`),
//! every LLM request body and its raw streamed response are written to
//! `<state>/koda/debug/rr-session-N.json` and `rr-session-N.res.log`. That is
//! exactly enough to reproduce a bad turn: the precise JSON we sent and the raw
//! SSE frames we got back, with nothing redacted except the bearer token.
//!
//! It is a global switch (an atomic) so any code path — the main agent, a
//! subagent, a one-off models call — records without threading a flag through.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(1);

/// Turn debug capture on or off at runtime (settings toggle, `/debug`).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether capture is active. Honours `KODA_DEBUG=1` even if config said off.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
        || matches!(
            std::env::var("KODA_DEBUG").ok().as_deref(),
            Some("1") | Some("true")
        )
}

/// The directory debug artifacts are written to: `<state>/koda/debug`.
pub fn dir() -> PathBuf {
    crate::log::log_dir().join("debug")
}

/// A single captured request/response pair. Created per request when enabled;
/// `None` when disabled, so callers pay nothing on the hot path.
pub struct Capture {
    res_path: PathBuf,
}

impl Capture {
    /// Reserve a session id and write the request body. Returns `None` when
    /// debug is off or the artifact directory can't be created — capture must
    /// never be the reason a request fails.
    pub fn start(endpoint: &str, body: &serde_json::Value) -> Option<Capture> {
        if !enabled() {
            return None;
        }
        let dir = dir();
        std::fs::create_dir_all(&dir).ok()?;
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        let req_path = dir.join(format!("rr-session-{id}.json"));
        let res_path = dir.join(format!("rr-session-{id}.res.log"));

        let dump = serde_json::json!({
            "id": id,
            "endpoint": endpoint,
            "path": "/chat/completions",
            "method": "POST",
            "body": body,
        });
        if let Ok(text) = serde_json::to_string_pretty(&dump) {
            let _ = std::fs::write(&req_path, format!("{text}\n"));
        }
        crate::tel_debug!("debug", "capturing request", "id" => id, "file" => req_path.display());
        Some(Capture { res_path })
    }

    /// Append a raw response chunk (verbatim SSE bytes) to the response log.
    pub fn write_chunk(&self, bytes: &[u8]) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.res_path)
        {
            let _ = f.write_all(bytes);
        }
    }
}

/// A short human report for `/debug`: whether capture is on and where artifacts
/// and logs live, plus a count of captured sessions this run.
pub fn report() -> String {
    let d = dir();
    let count = std::fs::read_dir(&d)
        .map(|it| {
            it.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
                .count()
        })
        .unwrap_or(0);
    let log = crate::log::log_path();
    format!(
        "debug capture: {}\n  artifacts: {}\n  captured sessions: {count}\n  event log: {}\n  env override: KODA_DEBUG=1",
        if enabled() { "on" } else { "off" },
        d.display(),
        log.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // These tests share the process-global switch and env, so serialize them.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn env_override_forces_enabled() {
        let _g = guard();
        set_enabled(false);
        std::env::set_var("KODA_DEBUG", "1");
        assert!(enabled());
        std::env::remove_var("KODA_DEBUG");
        assert!(!enabled());
    }

    #[test]
    fn toggle_flips_state() {
        let _g = guard();
        std::env::remove_var("KODA_DEBUG");
        set_enabled(true);
        assert!(enabled());
        set_enabled(false);
        assert!(!enabled());
    }

    #[test]
    fn capture_is_none_when_disabled() {
        let _g = guard();
        std::env::remove_var("KODA_DEBUG");
        set_enabled(false);
        assert!(Capture::start("http://x", &serde_json::json!({})).is_none());
    }
}
