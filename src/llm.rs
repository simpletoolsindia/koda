//! Minimal OpenAI-compatible chat client: streaming SSE + native tool-call deltas.

use crate::log::Level;
use crate::{tel_debug, tel_warn};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    #[default]
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Raw JSON string, exactly as the API defines it.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

impl ToolCall {
    pub fn new(id: String, name: String, arguments: String) -> Self {
        Self {
            id,
            kind: "function".into(),
            function: FunctionCall { name, arguments },
        }
    }

    /// Parsed arguments, tolerating empty or slightly malformed payloads.
    pub fn args(&self) -> Value {
        let raw = self.function.arguments.trim();
        if raw.is_empty() {
            return Value::Object(Default::default());
        }
        serde_json::from_str(raw)
            .or_else(|_| serde_json::from_str(&repair_json(raw)))
            .unwrap_or(Value::Null)
    }
}

/// Local models often emit trailing commas or unterminated strings.
fn repair_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut in_str = false;
    let mut escaped = false;
    let mut stack: Vec<char> = Vec::new();
    for c in s.chars() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                stack.push(c);
                out.push(c);
            }
            '}' | ']' => {
                // Drop a trailing comma before the closing bracket.
                while out.ends_with(|c: char| c.is_whitespace()) {
                    out.pop();
                }
                if out.ends_with(',') {
                    out.pop();
                }
                stack.pop();
                out.push(c);
            }
            ',' => out.push(c),
            _ => out.push(c),
        }
    }
    if in_str {
        out.push('"');
    }
    // Drop a dangling comma before closing.
    let trimmed = out.trim_end();
    let mut out = trimmed.strip_suffix(',').unwrap_or(trimmed).to_string();
    while let Some(open) = stack.pop() {
        out.push(if open == '{' { '}' } else { ']' });
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Image data-URLs (`data:image/png;base64,…`) attached to a user message,
    /// for vision-capable models. Kept out of the wire `Message` shape and the
    /// session file: the request builder expands them into OpenAI multimodal
    /// `content` parts at send time, and history/token accounting use `content`
    /// (the text) as before.
    #[serde(skip)]
    pub images: Vec<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(content.into()),
            ..Default::default()
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            ..Default::default()
        }
    }
    /// A user message carrying one or more images (as `data:` URLs) alongside
    /// its text, for vision-capable models.
    pub fn user_with_images(content: impl Into<String>, images: Vec<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            images,
            ..Default::default()
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(content.into()),
            ..Default::default()
        }
    }
    pub fn assistant_calls(content: Option<String>, calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content,
            tool_calls: if calls.is_empty() { None } else { Some(calls) },
            ..Default::default()
        }
    }
    pub fn tool(
        call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_call_id: Some(call_id.into()),
            name: Some(name.into()),
            ..Default::default()
        }
    }

    /// Rough token estimate (~4 chars/token) used for history trimming.
    pub fn approx_tokens(&self) -> usize {
        let mut n = 4;
        if let Some(c) = &self.content {
            n += c.len() / 4;
        }
        if let Some(calls) = &self.tool_calls {
            for c in calls {
                n += (c.function.name.len() + c.function.arguments.len()) / 4 + 4;
            }
        }
        n
    }
}

/// Heuristic: does this model id look vision-capable? Local model names are not
/// standardized, so we match the substrings that reliably signal a multimodal
/// checkpoint. False negatives are safe here — they only trigger the OCR
/// fallback, which is a graceful downgrade, never a wrong answer.
pub fn model_is_vision(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    const HINTS: &[&str] = &[
        "vl",
        "vision",
        "llava",
        "bakllava",
        "moondream",
        "minicpm-v",
        "cogvlm",
        "qwen2-vl",
        "qwen2.5-vl",
        "qwen-vl",
        "internvl",
        "pixtral",
        "gemma-3",
        "gemma3",
        "llama-3.2",
        "llama3.2",
        "phi-3.5-vision",
        "phi-3-vision",
        "gpt-4o",
        "gpt-4.1",
        "gpt-4-vision",
        "gpt-4-turbo",
        "claude-3",
        "gemini",
        "smolvlm",
        "idefics",
        "florence",
        "janus",
        "glm-4v",
    ];
    HINTS.iter().any(|h| m.contains(h))
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub temperature: f64,
    pub top_p: f64,
    pub max_tokens: u32,
    pub tools: Option<Vec<Value>>,
    /// "off" | "low" | "medium" | "high". "off" omits the field.
    pub reasoning_effort: String,
}

impl ChatRequest {
    /// The exact wire body. `pub(crate)` so the trace can record what was sent,
    /// byte for byte, rather than a reconstruction of it.
    pub(crate) fn to_json(&self) -> Value {
        // Expand any image-bearing user message into OpenAI's multimodal
        // `content` array (text part + image_url parts). Messages without images
        // serialize exactly as before, so this is invisible to text-only models.
        let messages: Vec<Value> = self
            .messages
            .iter()
            .map(|m| {
                if m.images.is_empty() {
                    return serde_json::to_value(m).unwrap_or(Value::Null);
                }
                let mut parts: Vec<Value> = Vec::with_capacity(m.images.len() + 1);
                if let Some(text) = &m.content {
                    if !text.is_empty() {
                        parts.push(serde_json::json!({"type": "text", "text": text}));
                    }
                }
                for url in &m.images {
                    parts.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": url }
                    }));
                }
                serde_json::json!({ "role": m.role, "content": parts })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "temperature": self.temperature,
            "top_p": self.top_p,
        });
        if self.max_tokens > 0 {
            body["max_tokens"] = self.max_tokens.into();
        }
        // Thinking-model effort hint. "off" (the default) omits it so servers
        // that don't understand the field see an unchanged request.
        let effort = self.reasoning_effort.trim().to_ascii_lowercase();
        if matches!(effort.as_str(), "low" | "medium" | "high") {
            body["reasoning_effort"] = Value::String(effort);
        }
        if let Some(tools) = &self.tools {
            if !tools.is_empty() {
                body["tools"] = Value::Array(tools.clone());
                body["tool_choice"] = Value::String("auto".into());
            }
        }
        body
    }
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Text(String),
    /// `reasoning_content` from thinking models; shown dimmed, never sent back.
    Reasoning(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        args: String,
    },
    Finish(Option<String>),
}

/// Whether another attempt could plausibly succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// Transport hiccup, rate limit, or a 5xx: worth another go.
    Transient,
    /// A bad request, unknown model, or auth failure: retrying changes nothing.
    Permanent,
}

/// An API failure, carrying both a short line for the user and the full detail
/// for the log.
#[derive(Debug)]
pub struct ApiError {
    pub retry: Retry,
    /// One sentence, plain language, no stack or JSON.
    pub user: String,
    /// Everything we know, for the event log.
    pub detail: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    fn transient(user: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            retry: Retry::Transient,
            user: user.into(),
            detail: detail.into(),
        }
    }
    fn permanent(user: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            retry: Retry::Permanent,
            user: user.into(),
            detail: detail.into(),
        }
    }
}

/// Turn a transport error into something a person can act on.
fn classify_transport(endpoint: &str, e: &reqwest::Error) -> ApiError {
    let detail = format!("{e:?}");
    if e.is_connect() {
        return ApiError::transient(
            format!("can't reach the model server at {endpoint}"),
            detail,
        );
    }
    if e.is_timeout() {
        return ApiError::transient(format!("{endpoint} timed out"), detail);
    }
    if e.is_body() || e.is_decode() {
        return ApiError::transient("the reply from the server was cut short", detail);
    }
    ApiError::transient(format!("the request to {endpoint} failed"), detail)
}

/// Turn an HTTP status into something a person can act on.
fn classify_status(status: reqwest::StatusCode, body: &str, model: &str) -> ApiError {
    let detail = format!("HTTP {status}: {body}");
    let msg = snippet(body);
    match status.as_u16() {
        429 => ApiError::transient("the server is rate limiting; easing off", detail),
        500..=599 => ApiError::transient(
            format!("the model server hit an internal error ({status})"),
            detail,
        ),
        401 | 403 => ApiError::permanent(
            "the server rejected the API key — check api_key in your config",
            detail,
        ),
        404 => ApiError::permanent(
            format!("`{model}` is not available on this server — try /models"),
            detail,
        ),
        413 => ApiError::permanent(
            "the request was too large for the server — try /compact",
            detail,
        ),
        400 | 422 => ApiError::permanent(
            if msg.is_empty() {
                "the server rejected the request".to_string()
            } else {
                format!("the server rejected the request: {msg}")
            },
            detail,
        ),
        _ => ApiError::permanent(format!("the server replied {status}"), detail),
    }
}

/// Exponential backoff with a cap, so a flapping server is not hammered.
fn backoff(attempt: u32) -> Duration {
    let ms = 300u64.saturating_mul(1 << attempt.min(4));
    Duration::from_millis(ms.min(4_000))
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl Client {
    /// Build a client, optionally accepting certificates that cannot be
    /// verified.
    ///
    /// `insecure` exists for an internal endpoint behind a proxy that re-signs
    /// TLS with a private CA. It turns off the check that the server is who it
    /// says it is, so anything in the path can read and alter the traffic --
    /// the API key included. It is never the default and is not inferred from
    /// a failure; the user has to ask for it.
    pub fn with_tls(endpoint: String, api_key: String, insecure: bool) -> Result<Self> {
        let mut b = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // No total timeout: local generation can be slow.
            .pool_idle_timeout(Duration::from_secs(90))
            .user_agent(concat!("koda/", env!("CARGO_PKG_VERSION")));
        if insecure {
            b = b
                .danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true);
        }
        let http = b.build().context("building HTTP client")?;
        Ok(Self {
            http,
            endpoint,
            api_key,
        })
    }

    pub fn set_endpoint(&mut self, endpoint: String) {
        self.endpoint = endpoint;
    }

    /// Swap the credential, for when the endpoint changes with it — a role
    /// agent pointed at a different provider needs that provider's key, not the
    /// session's.
    pub fn set_api_key(&mut self, api_key: String) {
        self.api_key = api_key;
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.endpoint, path);
        let mut rb = self.http.request(method, url);
        if !self.api_key.is_empty() {
            rb = rb.bearer_auth(&self.api_key);
        }
        rb
    }

    pub async fn models(&self) -> Result<Vec<String>> {
        let resp = self
            .req(reqwest::Method::GET, "/models")
            .send()
            .await
            .map_err(|e| classify_transport(&self.endpoint, &e))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(classify_status(status, &text, "").into());
        }
        let v: Value = serde_json::from_str(&text).context("parsing /models response")?;
        let mut out = Vec::new();
        if let Some(items) = v.get("data").and_then(|d| d.as_array()) {
            for it in items {
                if let Some(id) = it.get("id").and_then(|i| i.as_str()) {
                    out.push(id.to_string());
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Stream a completion, retrying transient failures with backoff.
    ///
    /// `attempts` counts total tries. A retry only happens when nothing has
    /// been emitted yet: re-sending after partial output would duplicate text
    /// on screen and corrupt the transcript.
    pub async fn stream_with_retry(
        &self,
        req: &ChatRequest,
        tx: &UnboundedSender<StreamEvent>,
        attempts: u32,
    ) -> Result<()> {
        self.stream_traced(req, tx, attempts, None).await
    }

    /// As `stream_with_retry`, but mirroring the raw response bytes and the
    /// retry count into an open trace step so the web UI can show exactly what
    /// came back off the wire.
    pub async fn stream_traced(
        &self,
        req: &ChatRequest,
        tx: &UnboundedSender<StreamEvent>,
        attempts: u32,
        trace: Option<crate::trace::StepRef>,
    ) -> Result<()> {
        let attempts = attempts.max(1);
        let mut last: Option<anyhow::Error> = None;

        for attempt in 0..attempts {
            if attempt > 0 {
                let wait = backoff(attempt - 1);
                crate::trace::set_retries(trace, attempt);
                tel_warn!(
                    "http",
                    "retrying request",
                    "attempt" => attempt + 1,
                    "of" => attempts,
                    "wait_ms" => wait.as_millis(),
                );
                tokio::time::sleep(wait).await;
            }
            // Forward events as they arrive — buffering them until the attempt
            // finished would turn streaming back into a single lump — while
            // counting them, because a retry is only safe before any output.
            let (probe_tx, mut probe_rx) = tokio::sync::mpsc::unbounded_channel();
            let counter = Arc::new(AtomicUsize::new(0));
            let forwarder = {
                let out = tx.clone();
                let seen = counter.clone();
                tokio::spawn(async move {
                    while let Some(ev) = probe_rx.recv().await {
                        seen.fetch_add(1, Ordering::Relaxed);
                        if out.send(ev).is_err() {
                            break;
                        }
                    }
                })
            };
            let started = std::time::Instant::now();
            let result = self.stream_once(req, &probe_tx, trace).await;
            drop(probe_tx);
            let _ = forwarder.await;
            let emitted = counter.load(Ordering::Relaxed);

            match result {
                Ok(()) if emitted == 0 => {
                    // A 200 with nothing in it is usually a broken template.
                    let e = ApiError::transient(
                        format!(
                            "`{}` returned an empty response — this usually means the \
                             model's chat template is broken in the server",
                            req.model
                        ),
                        format!("empty stream after {:?}", started.elapsed()),
                    );
                    tel_warn!("http", "empty stream", "attempt" => attempt + 1);
                    last = Some(e.into());
                    continue;
                }
                Ok(()) => {
                    tel_debug!(
                        "http",
                        "stream complete",
                        "events" => emitted,
                        "ms" => started.elapsed().as_millis(),
                    );
                    return Ok(());
                }
                Err(e) => {
                    let retryable = e
                        .downcast_ref::<ApiError>()
                        .map(|a| a.retry == Retry::Transient)
                        .unwrap_or(false);
                    if let Some(api) = e.downcast_ref::<ApiError>() {
                        crate::log::push(
                            if retryable { Level::Warn } else { Level::Error },
                            "http",
                            api.user.clone(),
                            vec![("detail".into(), api.detail.replace('\n', " "))],
                        );
                    } else {
                        tel_warn!("http", format!("request failed: {e:#}"));
                    }
                    // A transient drop *after* some output already streamed (e.g.
                    // an unexpected mid-stream EOF) is salvageable: the partial
                    // reply and any completed tool calls are already on screen and
                    // usable. Retrying would duplicate that output, so instead we
                    // end the turn gracefully with a note rather than failing it.
                    if emitted > 0 {
                        if retryable {
                            tel_warn!(
                                "http",
                                "stream cut short; keeping the partial reply",
                                "events" => emitted,
                            );
                            return Ok(());
                        }
                        return Err(e);
                    }
                    // Nothing produced yet: retry transient errors, fail permanent.
                    if !retryable {
                        return Err(e);
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| {
            ApiError::transient("the model server did not respond", "no attempts made").into()
        }))
    }

    async fn stream_once(
        &self,
        req: &ChatRequest,
        tx: &UnboundedSender<StreamEvent>,
        trace: Option<crate::trace::StepRef>,
    ) -> Result<()> {
        let body = req.to_json();
        // Developer debug: record the exact request body and, below, the raw
        // response bytes. `None` (and zero cost) unless debug mode is on.
        let capture = crate::debug::Capture::start(&self.endpoint, &body);
        let resp = self
            .req(reqwest::Method::POST, "/chat/completions")
            .json(&body)
            .send()
            .await
            .map_err(|e| classify_transport(&self.endpoint, &e))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let framed = format!("HTTP {status}\n{body}");
            if let Some(cap) = &capture {
                cap.write_chunk(framed.as_bytes());
            }
            crate::trace::append_sse(trace, framed.as_bytes());
            return Err(classify_status(status, &body, &req.model).into());
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut finished = false;

        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(b) => b,
                // The connection dropped mid-stream (a common failure with local
                // servers under load: "unexpected EOF during chunk"). Classify it
                // as transient so the caller can retry when nothing was produced,
                // or salvage the partial turn when some output already arrived.
                Err(e) => {
                    return Err(ApiError::transient(
                        "the reply from the server was cut short",
                        format!("reading stream chunk: {e}"),
                    )
                    .into());
                }
            };
            if let Some(cap) = &capture {
                cap.write_chunk(&bytes);
            }
            crate::trace::append_sse(trace, &bytes);
            buf.push_str(&String::from_utf8_lossy(&bytes));

            // SSE frames are newline-delimited; process complete lines only.
            while let Some(nl) = buf.find('\n') {
                let line: String = buf.drain(..=nl).collect();
                let line = line.trim_end_matches(['\n', '\r']);
                let Some(payload) = line.strip_prefix("data:") else {
                    continue; // ignore comments, `event:`, blank separators
                };
                let payload = payload.trim();
                if payload.is_empty() {
                    continue;
                }
                if payload == "[DONE]" {
                    finished = true;
                    break;
                }
                match serde_json::from_str::<Value>(payload) {
                    Ok(v) => emit(&v, tx),
                    // Some servers split large JSON across frames; skip unparseable ones.
                    Err(_) => continue,
                }
            }
            if finished {
                break;
            }
        }
        Ok(())
    }
}

fn emit(v: &Value, tx: &UnboundedSender<StreamEvent>) {
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| err.to_string());
        let _ = tx.send(StreamEvent::Text(format!("\n[server error] {msg}\n")));
        return;
    }
    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
        return;
    };
    // Streaming servers use `delta`; a few non-conforming ones reuse `message`.
    let delta = choice.get("delta").or_else(|| choice.get("message"));
    if let Some(d) = delta {
        if let Some(r) = d
            .get("reasoning_content")
            .or_else(|| d.get("reasoning"))
            .and_then(|r| r.as_str())
        {
            if !r.is_empty() {
                let _ = tx.send(StreamEvent::Reasoning(r.to_string()));
            }
        }
        match d.get("content") {
            Some(Value::String(s)) if !s.is_empty() => {
                let _ = tx.send(StreamEvent::Text(s.clone()));
            }
            // Vision-style content arrays.
            Some(Value::Array(parts)) => {
                for p in parts {
                    if let Some(s) = p.get("text").and_then(|t| t.as_str()) {
                        let _ = tx.send(StreamEvent::Text(s.to_string()));
                    }
                }
            }
            _ => {}
        }
        if let Some(calls) = d.get("tool_calls").and_then(|t| t.as_array()) {
            for (pos, call) in calls.iter().enumerate() {
                let index = call
                    .get("index")
                    .and_then(|i| i.as_u64())
                    .map(|i| i as usize)
                    .unwrap_or(pos);
                let id = call
                    .get("id")
                    .and_then(|i| i.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                let f = call.get("function");
                let name = f
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                let args = f
                    .and_then(|f| f.get("arguments"))
                    .map(|a| match a {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                let _ = tx.send(StreamEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    args,
                });
            }
        }
    }
    if let Some(reason) = choice.get("finish_reason") {
        if !reason.is_null() {
            let _ = tx.send(StreamEvent::Finish(reason.as_str().map(|s| s.to_string())));
        }
    }
}

fn snippet(s: &str) -> String {
    let s = s.trim();
    // Prefer the API error message when present.
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        if let Some(m) = v
            .pointer("/error/message")
            .and_then(|m| m.as_str())
            .or_else(|| v.get("message").and_then(|m| m.as_str()))
        {
            return m.chars().take(400).collect();
        }
    }
    s.chars().take(400).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_truncated_json() {
        let v: Value = serde_json::from_str(&repair_json(r#"{"path":"a.rs","old":"x"#)).unwrap();
        assert_eq!(v["path"], "a.rs");
    }

    #[test]
    fn parses_args_with_trailing_comma() {
        let c = ToolCall::new("1".into(), "read_file".into(), r#"{"path":"a.rs",}"#.into());
        assert_eq!(c.args()["path"], "a.rs");
    }

    #[test]
    fn image_messages_become_multimodal_content() {
        let req = ChatRequest {
            model: "vl".into(),
            messages: vec![
                Message::user("plain text"),
                Message::user_with_images(
                    "what is this?",
                    vec!["data:image/png;base64,AAAA".into()],
                ),
            ],
            temperature: 0.2,
            top_p: 0.95,
            max_tokens: 0,
            tools: None,
            reasoning_effort: "off".into(),
        };
        let body = req.to_json();
        let msgs = body["messages"].as_array().unwrap();
        // A message without images stays a plain string.
        assert!(msgs[0]["content"].is_string());
        // A message with images becomes an array of typed parts.
        let parts = msgs[1]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn detects_vision_models_by_name() {
        assert!(model_is_vision("qwen2.5-vl:7b"));
        assert!(model_is_vision("llava:13b"));
        assert!(model_is_vision("llama-3.2-vision"));
        assert!(model_is_vision("gpt-4o"));
        assert!(model_is_vision("minicpm-v"));
        // Text-only coders are not vision.
        assert!(!model_is_vision("qwen2.5-coder:14b"));
        assert!(!model_is_vision("devstral-small"));
        assert!(!model_is_vision("deepseek-coder"));
    }

    #[test]
    fn reasoning_effort_is_emitted_only_when_set() {
        let mk = |effort: &str| ChatRequest {
            model: "m".into(),
            messages: vec![Message::user("hi")],
            temperature: 0.2,
            top_p: 0.95,
            max_tokens: 0,
            tools: None,
            reasoning_effort: effort.into(),
        };
        // "off" (and anything unrecognised) omits the field entirely.
        assert!(mk("off").to_json().get("reasoning_effort").is_none());
        assert!(mk("").to_json().get("reasoning_effort").is_none());
        // A real level is passed through, lowercased.
        assert_eq!(mk("HIGH").to_json()["reasoning_effort"], "high");
        assert_eq!(mk("low").to_json()["reasoning_effort"], "low");
    }
}
