// LlmDebug.jsx — LLM request/response debug viewer with SSE reconstruction
function parseSseResponse(raw) {
  // Reconstruct assistant text, reasoning, and tool calls from raw SSE data: lines
  let text = '';
  let reasoning = '';
  const toolCalls = {}; // index -> {id, name, args}
  let finishReason = null;

  const lines = (raw || '').split(/\r?\n/);
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed.startsWith('data:')) continue;
    const payload = trimmed.slice(5).trim();
    if (!payload || payload === '[DONE]') continue;
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
  return { text, reasoning, toolCalls: Object.values(toolCalls), finishReason };
}

function parseRequest(raw) {
  try { return typeof raw === 'string' ? JSON.parse(raw) : raw; }
  catch { return null; }
}

function LlmDebug({ debug, loading, error }) {
  const [selectedId, setSelectedId] = React.useState(null);

  React.useEffect(() => {
    if (debug && debug.sessions && debug.sessions.length > 0 && !selectedId) {
      setSelectedId(debug.sessions[debug.sessions.length - 1].id);
    }
  }, [debug]);

  if (loading && !debug) {
    return <div className="flex items-center justify-center h-full text-gray-500">Loading debug sessions…</div>;
  }

  if (debug && !debug.enabled) {
    return (
      <div className="flex items-center justify-center h-full p-8">
        <div className="max-w-md text-center p-8 rounded-2xl bg-gradient-to-br from-amber-500/10 to-orange-500/5 border border-amber-500/20 backdrop-blur-sm">
          <div className="w-14 h-14 mx-auto mb-4 rounded-xl bg-amber-500/20 flex items-center justify-center">
            <svg className="w-7 h-7 text-amber-400" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z" /></svg>
          </div>
          <h3 className="text-lg font-semibold text-amber-200 mb-2">Debug capture is disabled</h3>
          <p className="text-sm text-gray-400 leading-relaxed">
            Enable LLM request/response recording in koda via the <code className="px-1.5 py-0.5 rounded bg-white/10 text-cyan-300 font-mono text-xs">/settings</code> command to inspect the exact payloads exchanged with the model.
          </p>
        </div>
      </div>
    );
  }

  const sessions = (debug && debug.sessions) || [];
  const selected = sessions.find(s => s.id === selectedId);

  return (
    <div className="flex h-full">
      {/* Session list */}
      <aside className="w-56 shrink-0 border-r border-white/5 overflow-y-auto bg-white/[0.02]">
        <div className="p-3 text-[10px] uppercase tracking-wider text-gray-500 font-semibold sticky top-0 bg-[#0a0b12]/80 backdrop-blur-sm">
          Sessions · {sessions.length}
        </div>
        {debug && debug.dir && (
          <div className="px-3 pb-2 text-[10px] text-gray-600 truncate" title={debug.dir}>📁 {debug.dir}</div>
        )}
        {sessions.length === 0 && <div className="p-3 text-xs text-gray-600">No sessions recorded yet.</div>}
        {sessions.map(s => (
          <button key={s.id} onClick={() => setSelectedId(s.id)}
            className={`w-full text-left px-3 py-2.5 text-xs border-l-2 transition-all ${
              selectedId === s.id ? 'border-cyan-400 bg-cyan-500/10 text-cyan-200' : 'border-transparent text-gray-400 hover:bg-white/5'
            }`} aria-current={selectedId === s.id}>
            <div className="font-mono font-medium truncate">{s.id}</div>
          </button>
        ))}
      </aside>

      {/* Detail */}
      <div className="flex-1 overflow-y-auto p-4">
        {!selected && <div className="text-gray-500 text-sm">Select a session to inspect.</div>}
        {selected && <SessionDetail session={selected} />}
      </div>
    </div>
  );
}

function SessionDetail({ session }) {
  const req = parseRequest(session.request);
  const resp = parseSseResponse(session.response);
  const [tab, setTab] = React.useState('request');

  return (
    <div className="animate-fade-in space-y-4">
      <div className="flex gap-2">
        {['request','response'].map(t => (
          <button key={t} onClick={() => setTab(t)}
            className={`px-3 py-1.5 rounded-lg text-xs font-medium transition-all ${
              tab === t ? 'bg-cyan-500/20 text-cyan-300 border border-cyan-500/30' : 'bg-white/5 text-gray-400 border border-white/10 hover:bg-white/10'
            }`}>
            {t === 'request' ? '→ Our Request' : '← LLM Response'}
          </button>
        ))}
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
                <Panel title={`Messages (${req.messages.length})`} accent="cyan">
                  <div className="space-y-2">
                    {req.messages.map((m, i) => (
                      <div key={i} className="rounded-lg bg-black/30 border border-white/5 overflow-hidden">
                        <div className="px-2.5 py-1 text-[10px] uppercase tracking-wider font-semibold flex items-center gap-2 border-b border-white/5"
                          style={{ color: roleColor(m.role) }}>
                          <span className="w-1.5 h-1.5 rounded-full" style={{ background: roleColor(m.role) }} />
                          {m.role}
                        </div>
                        <pre className="p-2.5 text-[11px] text-gray-300 whitespace-pre-wrap break-words max-h-64 overflow-y-auto font-mono">{msgContent(m)}</pre>
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
                <summary className="px-3 py-2 text-xs text-gray-500 cursor-pointer hover:text-gray-300">Raw request JSON</summary>
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
            <Panel title="Assistant Text" accent="cyan">
              <div className="text-sm text-gray-200 whitespace-pre-wrap leading-relaxed">{resp.text}</div>
            </Panel>
          )}
          {resp.reasoning && (
            <Panel title="Reasoning" accent="purple">
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
                    </div>
                    <pre className="p-2.5 text-[11px] text-gray-300 whitespace-pre-wrap break-words font-mono max-h-48 overflow-y-auto">{prettyArgs(tc.args)}</pre>
                  </div>
                ))}
              </div>
            </Panel>
          )}
          {!resp.text && !resp.reasoning && resp.toolCalls.length === 0 && (
            <Panel title="Raw response">
              <pre className="text-[10px] text-gray-500 whitespace-pre-wrap font-mono max-h-96 overflow-y-auto">{session.response}</pre>
            </Panel>
          )}
          {resp.finishReason && <div className="text-[10px] text-gray-600">finish_reason: <span className="text-gray-400">{resp.finishReason}</span></div>}
          <details className="rounded-xl bg-white/[0.02] border border-white/5">
            <summary className="px-3 py-2 text-xs text-gray-500 cursor-pointer hover:text-gray-300">Raw SSE response</summary>
            <pre className="p-3 text-[10px] text-gray-500 overflow-x-auto font-mono max-h-96 overflow-y-auto whitespace-pre-wrap">{session.response}</pre>
          </details>
        </div>
      )}
    </div>
  );
}

function Panel({ title, accent = 'cyan', children }) {
  const accentColors = {
    cyan: 'border-cyan-500/20', purple: 'border-purple-500/20', green: 'border-emerald-500/20',
  };
  const dotColors = { cyan: 'bg-cyan-400', purple: 'bg-purple-400', green: 'bg-emerald-400' };
  return (
    <section className={`rounded-xl bg-white/[0.03] border ${accentColors[accent]} backdrop-blur-sm overflow-hidden`}>
      <h3 className="px-3 py-2 text-xs font-semibold text-gray-300 border-b border-white/5 flex items-center gap-2">
        <span className={`w-1.5 h-1.5 rounded-full ${dotColors[accent]}`} />{title}
      </h3>
      <div className="p-3">{children}</div>
    </section>
  );
}

function roleColor(role) {
  return { system: '#a78bfa', user: '#22d3ee', assistant: '#34d399', tool: '#fbbf24' }[role] || '#9ca3af';
}
function msgContent(m) {
  if (typeof m.content === 'string') return m.content;
  if (Array.isArray(m.content)) return m.content.map(c => c.text || c.content || JSON.stringify(c)).join('\n');
  if (m.tool_calls) return JSON.stringify(m.tool_calls, null, 2);
  return JSON.stringify(m.content ?? m, null, 2);
}
function prettyArgs(args) {
  try { return JSON.stringify(JSON.parse(args), null, 2); } catch { return args; }
}
