// LlmDebug.jsx — LLM request/response debug viewer with SSE reconstruction
function parseSseResponse(raw) {
  // Reconstruct assistant text, reasoning, and tool calls from raw SSE data: lines
  let text = '';
  let reasoning = '';
  const toolCalls = {}; // index -> {id, name, args}
  let finishReason = null;
  let done = false; // saw the [DONE] sentinel → the turn is complete

  const lines = (raw || '').split(/\r?\n/);
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed.startsWith('data:')) continue;
    const payload = trimmed.slice(5).trim();
    if (!payload) continue;
    if (payload === '[DONE]') { done = true; continue; }
    let obj;
    try { obj = JSON.parse(payload); } catch { continue; }
    const choices = obj.choices || [];
    for (const ch of choices) {
      const delta = ch.delta || ch.message || {};
      if (delta.content) text += delta.content;
      if (delta.reasoning_content) reasoning += delta.reasoning_content;
      if (delta.reasoning) reasoning += delta.reasoning;
      if (ch.finish_reason) finishReason = ch.finish_reason;
      const tcs = delta.tool_calls || [];
      for (const tc of tcs) {
        const idx = tc.index != null ? tc.index : (tc.id || 0);
        if (!toolCalls[idx]) toolCalls[idx] = { id: tc.id || '', name: '', args: '' };
        if (tc.id) toolCalls[idx].id = tc.id;
        if (tc.function) {
          if (tc.function.name) toolCalls[idx].name += tc.function.name;
          if (tc.function.arguments) toolCalls[idx].args += tc.function.arguments;
        }
      }
    }
  }
  // "Processing" = we have a response log but it hasn't hit [DONE]/finish yet.
  const processing = (raw && raw.length > 0) ? !(done || finishReason) : false;
  return { text, reasoning, toolCalls: Object.values(toolCalls), finishReason, done, processing };
}

// Copy text to the clipboard, tolerating older browsers / http origins.
function copyToClipboard(text) {
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text);
    }
  } catch (_) { /* fall through */ }
  return new Promise((resolve) => {
    const ta = document.createElement('textarea');
    ta.value = text; ta.style.position = 'fixed'; ta.style.opacity = '0';
    document.body.appendChild(ta); ta.select();
    try { document.execCommand('copy'); } catch (_) {}
    document.body.removeChild(ta); resolve();
  });
}

// A small copy button; shows a transient ✓ after a successful copy.
function CopyButton({ text, label = 'Copy', className = '' }) {
  const [copied, setCopied] = React.useState(false);
  if (text == null || text === '') return null;
  return (
    <button type="button"
      onClick={(e) => { e.stopPropagation(); copyToClipboard(String(text)).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1200); }); }}
      className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium border transition-all ${copied ? 'bg-emerald-500/20 border-emerald-500/40 text-emerald-300' : 'bg-white/5 border-white/10 text-gray-400 hover:text-gray-200 hover:bg-white/10'} ${className}`}
      title="Copy to clipboard" aria-label={copied ? 'Copied' : label}>
      {copied ? (
        <><svg className="w-3 h-3" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2.5} d="M5 13l4 4L19 7" /></svg>Copied</>
      ) : (
        <><svg className="w-3 h-3" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.8} d="M8 5H6a2 2 0 00-2 2v12a2 2 0 002 2h8a2 2 0 002-2v-2m-6-12h6a2 2 0 012 2v6m-8-8V3m0 2h.01" /></svg>{label}</>
      )}
    </button>
  );
}

function parseRequest(raw) {
  try { return typeof raw === 'string' ? JSON.parse(raw) : raw; }
  catch { return null; }
}

function LlmDebug({ debug, loading, error }) {
  const [selectedId, setSelectedId] = React.useState(null);
  const [followLatest, setFollowLatest] = React.useState(true);

  const sessions = (debug && debug.sessions) || [];

  // Which sessions are still streaming (prompt sent, response not yet [DONE]).
  const statusById = React.useMemo(() => {
    const m = {};
    for (const s of sessions) m[s.id] = parseSseResponse(s.response);
    return m;
  }, [sessions]);

  // Follow the newest session so the prompt "currently being processed" is what
  // you see by default; stop following once the user picks a session manually.
  React.useEffect(() => {
    if (sessions.length === 0) return;
    const newest = sessions[sessions.length - 1].id;
    if (followLatest || !selectedId) setSelectedId(newest);
  }, [sessions.map(s => s.id).join(','), followLatest]);

  if (loading && !debug) {
    return <div className="flex items-center justify-center h-full text-gray-500">Loading debug sessions…</div>;
  }

  if (debug && !debug.enabled) {
    return (
      <div className="flex items-center justify-center h-full p-8">
        <div className="max-w-md text-center p-8 rounded-2xl bg-surface border border-amber-500/25 shadow-panel">
          <div className="w-14 h-14 mx-auto mb-4 rounded-xl bg-amber-500/20 flex items-center justify-center">
            <svg className="w-7 h-7 text-amber-400" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" /></svg>
          </div>
          <h3 className="text-lg font-semibold text-amber-200 mb-2">Debug capture is disabled</h3>
          <p className="text-sm text-gray-400 leading-relaxed">
            Turn on <code className="px-1.5 py-0.5 rounded bg-white/10 text-indigo-300 font-mono text-xs">debug</code> capture in koda — the <code className="px-1.5 py-0.5 rounded bg-white/10 text-indigo-300 font-mono text-xs">/debug</code> command, the <code className="px-1.5 py-0.5 rounded bg-white/10 text-indigo-300 font-mono text-xs">/settings</code> page, or <code className="px-1.5 py-0.5 rounded bg-white/10 text-indigo-300 font-mono text-xs">KODA_DEBUG=1</code> — to watch the exact prompt koda is processing and the model's response as it streams back.
          </p>
        </div>
      </div>
    );
  }

  const selected = sessions.find(s => s.id === selectedId);
  const anyProcessing = sessions.some(s => statusById[s.id] && statusById[s.id].processing);

  return (
    <div className="flex flex-col md:flex-row h-full min-h-0">
      {/* Session list */}
      <aside className="w-full h-36 md:w-64 md:h-full shrink-0 border-b md:border-b-0 md:border-r border-line overflow-y-auto bg-surface">
        <div className="p-3 sticky top-0 bg-[#15171d]/95 border-b border-line backdrop-blur-sm z-10">
          <div className="flex items-center gap-2 text-[10px] uppercase tracking-wider text-gray-500 font-semibold">
            <span>Sessions · {sessions.length}</span>
            {anyProcessing && (
              <span className="ml-auto inline-flex items-center gap-1 text-emerald-400 normal-case tracking-normal">
                <span className="relative flex h-2 w-2"><span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-70" /><span className="relative inline-flex rounded-full h-2 w-2 bg-emerald-400" /></span>
                live
              </span>
            )}
          </div>
          <label className="mt-2 flex items-center gap-1.5 text-[10px] text-gray-500 normal-case tracking-normal cursor-pointer select-none">
            <input type="checkbox" checked={followLatest} onChange={e => setFollowLatest(e.target.checked)} className="accent-indigo-500" />
            Follow latest (currently processing)
          </label>
        </div>
        {debug && debug.dir && (
          <div className="px-3 pb-2 text-[10px] text-gray-600 truncate" title={debug.dir}>📁 {debug.dir}</div>
        )}
        {sessions.length === 0 && <div className="p-3 text-xs text-gray-600">No prompts captured yet. Send a message in koda and it will appear here live.</div>}
        {sessions.map(s => {
          const st = statusById[s.id] || {};
          return (
            <button key={s.id} onClick={() => { setSelectedId(s.id); setFollowLatest(false); }}
              className={`w-full text-left px-3 py-2.5 text-xs border-l-2 transition-all ${
                selectedId === s.id ? 'border-indigo-400 bg-indigo-500/10 text-indigo-200' : 'border-transparent text-gray-400 hover:bg-white/5'
              }`} aria-current={selectedId === s.id}>
              <div className="flex items-center gap-1.5">
                <span className="font-mono font-medium truncate">{s.id}</span>
                {st.processing ? (
                  <span className="ml-auto inline-flex items-center gap-1 text-[9px] text-emerald-400 shrink-0">
                    <span className="relative flex h-1.5 w-1.5"><span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-70" /><span className="relative inline-flex rounded-full h-1.5 w-1.5 bg-emerald-400" /></span>
                    processing
                  </span>
                ) : st.finishReason ? (
                  <span className="ml-auto text-[9px] text-gray-600 shrink-0">{st.finishReason}</span>
                ) : null}
              </div>
            </button>
          );
        })}
      </aside>

      {/* Detail */}
      <div className="flex-1 min-h-0 overflow-y-auto bg-canvas p-4 md:p-5 lg:p-6">
        {!selected && <div className="text-gray-500 text-sm">Select a session to inspect.</div>}
        {selected && <SessionDetail session={selected} status={statusById[selected.id]} />}
      </div>
    </div>
  );
}

function SessionDetail({ session, status }) {
  const req = parseRequest(session.request);
  const resp = status || parseSseResponse(session.response);
  const processing = resp.processing;
  const [tab, setTab] = React.useState(processing ? 'response' : 'request');
  // When a session flips to processing, jump to the response so the stream is
  // visible; the user can still switch back to the request freely.
  const wasProcessing = React.useRef(processing);
  React.useEffect(() => {
    if (processing && !wasProcessing.current) setTab('response');
    wasProcessing.current = processing;
  }, [processing]);

  return (
    <div className="space-y-4">
      {/* Live status banner */}
      <div className={`flex items-center gap-2 px-3 py-2 rounded-xl border text-xs ${
        processing ? 'bg-emerald-500/10 border-emerald-500/30 text-emerald-300' : 'bg-white/[0.03] border-white/10 text-gray-400'
      }`}>
        {processing ? (
          <>
            <span className="relative flex h-2.5 w-2.5"><span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-70" /><span className="relative inline-flex rounded-full h-2.5 w-2.5 bg-emerald-400" /></span>
            <span className="font-medium">Processing — koda is streaming this turn from the model…</span>
          </>
        ) : (
          <>
            <svg className="w-3.5 h-3.5 text-gray-500" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5 13l4 4L19 7" /></svg>
            <span>Completed{resp.finishReason ? ` · finish_reason: ${resp.finishReason}` : ''}</span>
          </>
        )}
        <span className="ml-auto font-mono text-gray-600">{session.id}</span>
      </div>

      <div className="flex gap-2 items-center">
        {['request','response'].map(t => (
          <button key={t} onClick={() => setTab(t)}
            className={`px-3 py-1.5 rounded-lg text-xs font-medium transition-all ${
              tab === t ? 'bg-indigo-500/20 text-indigo-300 border border-indigo-500/30' : 'bg-white/5 text-gray-400 border border-white/10 hover:bg-white/10'
            }`}>
            {t === 'request' ? '→ Prompt (our request)' : '← Response (LLM)'}
            {t === 'response' && processing && <span className="ml-1.5 inline-block w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse align-middle" />}
          </button>
        ))}
        <div className="ml-auto">
          <CopyButton text={tab === 'request' ? session.request : session.response}
            label={tab === 'request' ? 'Copy prompt' : 'Copy raw response'} />
        </div>
      </div>

      {tab === 'request' && (
        <div className="space-y-3">
          {req ? (
            <>
              {req.model && (
                <Panel title="Model" accent="purple">
                  <code className="text-purple-300 font-mono text-sm">{req.model}</code>
                </Panel>
              )}
              {Array.isArray(req.messages) && (
                <Panel title={`Messages (${req.messages.length})`} accent="indigo">
                  <div className="space-y-2">
                    {req.messages.map((m, i) => (
                      <div key={i} className="rounded-lg bg-black/30 border border-white/5 overflow-hidden">
                        <div className="px-2.5 py-1 text-[10px] uppercase tracking-wider font-semibold flex items-center gap-2 border-b border-white/5"
                          style={{ color: roleColor(m.role) }}>
                          <span className="w-1.5 h-1.5 rounded-full" style={{ background: roleColor(m.role) }} />
                          {m.role}
                          <span className="ml-auto"><CopyButton text={msgPlainText(m)} label="" /></span>
                        </div>
                        <MessageContent m={m} />
                      </div>
                    ))}
                  </div>
                </Panel>
              )}
              {Array.isArray(req.tools) && req.tools.length > 0 && (
                <Panel title={`Advertised Tools (${req.tools.length})`} accent="green">
                  <div className="flex flex-wrap gap-1.5">
                    {req.tools.map((t, i) => {
                      const fn = t.function || t;
                      return (
                        <span key={i} className="px-2 py-1 rounded-md bg-emerald-500/10 border border-emerald-500/20 text-emerald-300 text-[11px] font-mono"
                          title={fn.description || ''}>{fn.name}</span>
                      );
                    })}
                  </div>
                </Panel>
              )}
              <details className="rounded-xl bg-white/[0.02] border border-white/5">
                <summary className="px-3 py-2 text-xs text-gray-500 cursor-pointer hover:text-gray-300 flex items-center gap-2">
                  <span>Raw request JSON</span>
                  <span className="ml-auto"><CopyButton text={JSON.stringify(req, null, 2)} /></span>
                </summary>
                <pre className="p-3 text-[10px] text-gray-500 overflow-x-auto font-mono max-h-96 overflow-y-auto">{JSON.stringify(req, null, 2)}</pre>
              </details>
            </>
          ) : (
            <Panel title="Raw request"><pre className="text-[11px] text-gray-400 whitespace-pre-wrap font-mono">{session.request}</pre></Panel>
          )}
        </div>
      )}

      {tab === 'response' && (
        <div className="space-y-3">
          {resp.text && (
            <Panel title="Assistant Text" accent="indigo" copy={resp.text}>
              <div className="text-sm text-gray-200 whitespace-pre-wrap leading-relaxed">
                {resp.text}
                {processing && <span className="inline-block w-2 h-4 ml-0.5 bg-emerald-400/80 align-text-bottom animate-pulse" />}
              </div>
            </Panel>
          )}
          {resp.reasoning && (
            <Panel title="Reasoning" accent="purple" copy={resp.reasoning}>
              <div className="text-[13px] text-purple-200/80 whitespace-pre-wrap leading-relaxed italic font-light">{resp.reasoning}</div>
            </Panel>
          )}
          {resp.toolCalls.length > 0 && (
            <Panel title={`Tools Executed (${resp.toolCalls.length})`} accent="green">
              <div className="space-y-2">
                {resp.toolCalls.map((tc, i) => (
                  <div key={i} className="rounded-lg bg-black/30 border border-emerald-500/20 overflow-hidden">
                    <div className="px-2.5 py-1.5 flex items-center gap-2 border-b border-white/5 bg-emerald-500/5">
                      <svg className="w-3.5 h-3.5 text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M14.7 6.3a1 1 0 000 1.4l1.6 1.6a1 1 0 001.4 0l3.77-3.77a6 6 0 01-7.94 7.94l-6.91 6.91a2.12 2.12 0 01-3-3l6.91-6.91a6 6 0 017.94-7.94l-3.76 3.76z" /></svg>
                      <span className="text-emerald-300 font-mono text-xs font-semibold">{tc.name || '(pending)'}</span>
                      <span className="ml-auto"><CopyButton text={prettyArgs(tc.args)} label="" /></span>
                    </div>
                    <pre className="p-2.5 text-[11px] text-gray-300 whitespace-pre-wrap break-words font-mono max-h-48 overflow-y-auto">{prettyArgs(tc.args)}</pre>
                  </div>
                ))}
              </div>
            </Panel>
          )}
          {!resp.text && !resp.reasoning && resp.toolCalls.length === 0 && (
            <Panel title={processing ? 'Waiting for the first tokens…' : 'Raw response'}>
              {processing
                ? <div className="text-xs text-emerald-300/80 flex items-center gap-2"><span className="relative flex h-2 w-2"><span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-70" /><span className="relative inline-flex rounded-full h-2 w-2 bg-emerald-400" /></span>The model has the prompt and hasn't returned tokens yet.</div>
                : <pre className="text-[10px] text-gray-500 whitespace-pre-wrap font-mono max-h-96 overflow-y-auto">{session.response}</pre>}
            </Panel>
          )}
          <details className="rounded-xl bg-white/[0.02] border border-white/5">
            <summary className="px-3 py-2 text-xs text-gray-500 cursor-pointer hover:text-gray-300 flex items-center gap-2">
              <span>Raw SSE response</span>
              <span className="ml-auto"><CopyButton text={session.response} /></span>
            </summary>
            <pre className="p-3 text-[10px] text-gray-500 overflow-x-auto font-mono max-h-96 overflow-y-auto whitespace-pre-wrap">{session.response}</pre>
          </details>
        </div>
      )}
    </div>
  );
}

// Render one message's content: strings as text, and multimodal arrays with
// image_url parts shown as thumbnails, text/doc parts shown readably — instead
// of dumping raw JSON.
function MessageContent({ m }) {
  const c = m.content;
  if (typeof c === 'string') {
    return <pre className="p-2.5 text-[11px] text-gray-300 whitespace-pre-wrap break-words max-h-64 overflow-y-auto font-mono">{c || (m.tool_calls ? JSON.stringify(m.tool_calls, null, 2) : '(empty)')}</pre>;
  }
  if (Array.isArray(c)) {
    return (
      <div className="p-2.5 space-y-2">
        {c.map((part, i) => {
          const type = part.type || (part.image_url ? 'image_url' : part.text != null ? 'text' : 'unknown');
          if (type === 'image_url') {
            const url = (part.image_url && (part.image_url.url || part.image_url)) || part.url || '';
            return (
              <div key={i} className="flex items-start gap-2">
                <span className="text-[9px] uppercase tracking-wider text-indigo-400/70 pt-1 shrink-0">image</span>
                {url ? <img src={url} alt="attached image" className="max-h-40 rounded-lg border border-white/10 object-contain bg-black/30" />
                     : <span className="text-[11px] text-gray-500">(no url)</span>}
              </div>
            );
          }
          const text = part.text != null ? part.text : (typeof part === 'string' ? part : JSON.stringify(part, null, 2));
          return (
            <div key={i} className="flex items-start gap-2">
              <span className="text-[9px] uppercase tracking-wider text-gray-500 pt-1 shrink-0">{type}</span>
              <pre className="text-[11px] text-gray-300 whitespace-pre-wrap break-words max-h-64 overflow-y-auto font-mono flex-1">{text}</pre>
            </div>
          );
        })}
      </div>
    );
  }
  return <pre className="p-2.5 text-[11px] text-gray-300 whitespace-pre-wrap break-words max-h-64 overflow-y-auto font-mono">{JSON.stringify(c ?? m, null, 2)}</pre>;
}

function Panel({ title, accent = 'indigo', children, copy }) {
  const accentColors = {
    indigo: 'border-indigo-500/20', purple: 'border-purple-500/20', green: 'border-emerald-500/20',
  };
  const dotColors = { indigo: 'bg-indigo-400', purple: 'bg-purple-400', green: 'bg-emerald-400' };
  return (
    <section className={`rounded-xl bg-surface border ${accentColors[accent]} shadow-panel overflow-hidden`}>
      <h3 className="px-3 py-2 text-xs font-semibold text-gray-300 border-b border-white/5 flex items-center gap-2">
        <span className={`w-1.5 h-1.5 rounded-full ${dotColors[accent]}`} />{title}
        {copy != null && copy !== '' && <span className="ml-auto"><CopyButton text={copy} label="" /></span>}
      </h3>
      <div className="p-3">{children}</div>
    </section>
  );
}

function roleColor(role) {
  return { system: '#a78bfa', user: '#22d3ee', assistant: '#34d399', tool: '#fbbf24' }[role] || '#9ca3af';
}
// Flatten a message to plain text for the clipboard (images become a marker).
function msgPlainText(m) {
  if (typeof m.content === 'string') return m.content;
  if (Array.isArray(m.content)) {
    return m.content.map(c => {
      if (c.type === 'image_url' || c.image_url) return '[image]';
      return c.text != null ? c.text : (typeof c === 'string' ? c : JSON.stringify(c));
    }).join('\n');
  }
  if (m.tool_calls) return JSON.stringify(m.tool_calls, null, 2);
  return JSON.stringify(m.content ?? m, null, 2);
}
function prettyArgs(args) {
  try { return JSON.stringify(JSON.parse(args), null, 2); } catch { return args; }
}
