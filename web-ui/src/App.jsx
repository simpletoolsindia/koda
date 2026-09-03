// App.jsx — koda web control center: one page that traces every turn end to end
// and drives the running session from the same surface.
async function fetchJson(path) {
  const res = await fetch(path);
  if (res.status === 404) {
    throw new Error(`${path} is missing — your running koda is older than this UI. Rebuild and restart koda (cargo build --release) to enable this.`);
  }
  if (!res.ok) throw new Error(`${path} returned ${res.status}`);
  return res.json();
}

async function postJson(path, body) {
  const res = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body || {}),
  });
  if (res.status === 404) {
    throw new Error(`${path} is missing — restart koda after rebuilding to enable this control.`);
  }
  const data = await res.json().catch(() => ({}));
  if (!res.ok || data.ok === false) throw new Error(data.error || `${path} returned ${res.status}`);
  return data;
}

const MANAGE_TABS = [
  { id: 'graph', label: 'Code Graph' },
  { id: 'skills', label: 'Agents & Skills' },
  { id: 'prompt', label: 'System Prompt' },
  { id: 'debug', label: 'Raw Captures' },
];

function downloadJson(name, data) {
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  document.body.appendChild(a); a.click(); document.body.removeChild(a);
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function App() {
  // ---- live streams ----
  const [logs, setLogs] = React.useState([]);
  const [logVersion, setLogVersion] = React.useState(0);
  const [connected, setConnected] = React.useState(false);
  const [trace, setTrace] = React.useState(null);
  const [traceError, setTraceError] = React.useState(null);

  // ---- selection ----
  const [selectedId, setSelectedId] = React.useState(null);
  const [detail, setDetail] = React.useState(null);
  const [selectedSeq, setSelectedSeq] = React.useState(null);
  const [pinLive, setPinLive] = React.useState(true);

  // ---- panes ----
  const [rightTab, setRightTab] = React.useState('inspect');
  const [mobilePane, setMobilePane] = React.useState('trace');
  const [showLogs, setShowLogs] = React.useState(false);
  const [manage, setManage] = React.useState(null);
  const [paletteOpen, setPaletteOpen] = React.useState(false);
  const [toasts, setToasts] = React.useState([]);

  // ---- control data ----
  const [config, setConfig] = React.useState(null);
  const [memory, setMemory] = React.useState(null);
  const [learning, setLearning] = React.useState(null);
  const [sessions, setSessions] = React.useState(null);
  const [busy, setBusy] = React.useState(false);

  // ---- manage data (lazy) ----
  const [graph, setGraph] = React.useState(null);
  const [graphState, setGraphState] = React.useState({ loading: false, error: null });
  const [skills, setSkills] = React.useState(null);
  const [skillsState, setSkillsState] = React.useState({ loading: false, error: null });
  const [settings, setSettings] = React.useState(null);
  const [settingsState, setSettingsState] = React.useState({ loading: false, error: null });
  const [debugData, setDebugData] = React.useState(null);
  const [debugState, setDebugState] = React.useState({ loading: false, error: null });

  const seenSeqRef = React.useRef(-1);
  const logsRef = React.useRef([]);

  const pushToast = React.useCallback((message, kind = 'info') => {
    const id = Math.random().toString(36).slice(2);
    setToasts(items => [...items, { id, message, kind }]);
    setTimeout(() => setToasts(items => items.filter(i => i.id !== id)), 4200);
  }, []);

  const mergeLogs = React.useCallback((data) => {
    if (!data) return;
    if (typeof data.version === 'number') setLogVersion(data.version);
    const entries = data.entries || [];
    if (entries.length === 0) return;
    let maxSeq = seenSeqRef.current;
    const fresh = [];
    for (const entry of entries) {
      if (entry.seq > seenSeqRef.current) {
        fresh.push(entry);
        if (entry.seq > maxSeq) maxSeq = entry.seq;
      }
    }
    if (fresh.length > 0) {
      seenSeqRef.current = maxSeq;
      logsRef.current = [...logsRef.current, ...fresh].slice(-5000);
      setLogs(logsRef.current.slice());
    }
  }, []);

  // Logs: poll + SSE, exactly as before.
  React.useEffect(() => {
    let alive = true;
    const poll = async () => {
      try {
        const res = await fetch(`/api/logs?since=${seenSeqRef.current + 1}`);
        if (!res.ok) throw new Error(`bad status ${res.status}`);
        const data = await res.json();
        if (!alive) return;
        setConnected(true);
        mergeLogs(data);
      } catch (_) {
        if (alive) setConnected(false);
      }
    };
    poll();
    const timer = setInterval(poll, 1000);
    return () => { alive = false; clearInterval(timer); };
  }, [mergeLogs]);

  React.useEffect(() => {
    let source;
    try {
      source = new EventSource('/api/events');
      source.addEventListener('logs', e => { try { setConnected(true); mergeLogs(JSON.parse(e.data)); } catch (_) {} });
      source.addEventListener('trace', e => { try { setTrace(JSON.parse(e.data)); } catch (_) {} });
      source.onerror = () => {};
    } catch (_) {}
    return () => { if (source) source.close(); };
  }, [mergeLogs]);

  // Trace: poll while anything can change. 1s is fast enough to feel live and
  // cheap enough for a summary payload.
  React.useEffect(() => {
    let alive = true;
    const poll = async () => {
      try {
        const data = await fetchJson('/api/trace');
        if (!alive) return;
        setTrace(data); setTraceError(null);
      } catch (e) {
        if (alive) setTraceError(e.message);
      }
    };
    poll();
    const timer = setInterval(poll, 1000);
    return () => { alive = false; clearInterval(timer); };
  }, []);

  const turns = (trace && trace.turns) || [];
  const live = trace && trace.live;

  // Follow the live turn unless the user has deliberately picked another.
  React.useEffect(() => {
    if (pinLive && live && live.id !== selectedId) { setSelectedId(live.id); setSelectedSeq(null); }
    else if (selectedId == null && turns.length > 0) setSelectedId(turns[0].id);
  }, [pinLive, live, turns, selectedId]);

  // Detail for the selected turn: the live turn already arrives in full, so only
  // a finished turn needs fetching.
  React.useEffect(() => {
    if (selectedId == null) { setDetail(null); return; }
    if (live && live.id === selectedId) { setDetail(live); return; }
    let alive = true;
    (async () => {
      try {
        const data = await fetchJson(`/api/trace/${selectedId}`);
        if (alive) setDetail(data);
      } catch (e) {
        if (alive) { setDetail(null); pushToast(e.message, 'error'); }
      }
    })();
    return () => { alive = false; };
  }, [selectedId, live, pushToast]);

  const selectTurn = (id) => {
    setSelectedId(id);
    setSelectedSeq(null);
    setPinLive(!!(live && live.id === id));
    setMobilePane('trace');
  };

  const steps = (detail && detail.steps) || [];
  const step = selectedSeq == null ? null : steps.find(s => s.seq === selectedSeq);
  // The previous model call, for the prompt diff.
  const prevModelStep = React.useMemo(() => {
    if (!step || step.kind !== 'model') return null;
    let prev = null;
    for (const s of steps) {
      if (s.seq >= step.seq) break;
      if (s.kind === 'model') prev = s;
    }
    return prev;
  }, [step, steps]);

  // ---- control data loading ----
  const loadControl = React.useCallback(async (quiet) => {
    try {
      const [cfg, mem, learn, sess] = await Promise.all([
        fetchJson('/api/config'), fetchJson('/api/memory'),
        fetchJson('/api/learning'), fetchJson('/api/sessions'),
      ]);
      setConfig(cfg); setMemory(mem); setLearning(learn); setSessions(sess);
    } catch (e) {
      if (!quiet) pushToast(e.message, 'error');
    }
  }, [pushToast]);

  React.useEffect(() => { loadControl(true); }, [loadControl]);
  React.useEffect(() => {
    // Refresh while the control rail is on screen; the agent applies changes on
    // its own loop, so the truth lives server-side.
    if (rightTab !== 'control' && mobilePane !== 'control') return;
    const timer = setInterval(() => loadControl(true), 3000);
    return () => clearInterval(timer);
  }, [rightTab, mobilePane, loadControl]);

  const saveConfig = async (payload) => {
    setBusy(true);
    try {
      await postJson('/api/config', payload);
      pushToast('Settings applied to the running session', 'success');
      setConfig(null);
      await loadControl(true);
    } catch (e) { pushToast(e.message, 'error'); }
    finally { setBusy(false); }
  };
  const sendMemory = async (payload) => {
    try {
      await postJson('/api/memory', payload);
      pushToast(payload.remember ? 'Remembered' : 'Forgotten', 'success');
      setTimeout(() => loadControl(true), 900);
    } catch (e) { pushToast(e.message, 'error'); }
  };
  const sendLearning = async (payload) => {
    try {
      await postJson('/api/learning', payload);
      pushToast('Sent to koda', 'success');
      setTimeout(() => loadControl(true), 900);
    } catch (e) { pushToast(e.message, 'error'); }
  };
  const sendSession = async (id, action) => {
    try {
      await postJson(`/api/sessions/${encodeURIComponent(id)}/${action}`, {});
      pushToast(action === 'fork' ? `Forked ${id}` : `Resumed ${id}`, 'success');
      setTimeout(() => loadControl(true), 900);
    } catch (e) { pushToast(e.message, 'error'); }
  };

  // ---- manage panel loading ----
  const openManage = React.useCallback(async (tab) => {
    setManage(tab);
    if (tab === 'graph' && !graph) {
      setGraphState({ loading: true, error: null });
      try { setGraph(await fetchJson('/api/codegraph')); setGraphState({ loading: false, error: null }); }
      catch (e) { setGraphState({ loading: false, error: e.message }); }
    }
    if (tab === 'skills' && !skills) {
      setSkillsState({ loading: true, error: null });
      try { setSkills(await fetchJson('/api/skills')); setSkillsState({ loading: false, error: null }); }
      catch (e) { setSkillsState({ loading: false, error: e.message }); }
    }
    if (tab === 'prompt' && !settings) {
      setSettingsState({ loading: true, error: null });
      try { setSettings(await fetchJson('/api/settings')); setSettingsState({ loading: false, error: null }); }
      catch (e) { setSettingsState({ loading: false, error: e.message }); }
    }
    if (tab === 'debug') {
      setDebugState({ loading: true, error: null });
      try { setDebugData(await fetchJson('/api/debug')); setDebugState({ loading: false, error: null }); }
      catch (e) { setDebugState({ loading: false, error: e.message }); }
    }
  }, [graph, skills, settings]);

  const reloadSkills = React.useCallback(async () => {
    setSkillsState({ loading: true, error: null });
    try { setSkills(await fetchJson('/api/skills')); setSkillsState({ loading: false, error: null }); }
    catch (e) { setSkillsState({ loading: false, error: e.message }); }
  }, []);
  const reloadSettings = React.useCallback(async () => {
    setSettingsState({ loading: true, error: null });
    try { setSettings(await fetchJson('/api/settings')); setSettingsState({ loading: false, error: null }); }
    catch (e) { setSettingsState({ loading: false, error: e.message }); }
  }, []);

  // ---- command palette ----
  const paletteActions = React.useMemo(() => {
    const out = [];
    turns.slice(0, 12).forEach(t => out.push({
      id: `turn-${t.id}`, group: 'turn', label: `#${t.id} ${t.input || '(no input)'}`,
      hint: t.status, run: () => selectTurn(t.id),
    }));
    (config && config.modes || []).forEach(m => out.push({
      id: `mode-${m}`, group: 'mode', label: `Switch mode to ${m}`,
      run: () => saveConfig({ mode: m }),
    }));
    (config && config.tiers || []).forEach(t => out.push({
      id: `tier-${t}`, group: 'auto', label: `Set autonomy to ${t}`,
      run: () => saveConfig({ auto_tier: t }),
    }));
    if (config && config.toggles) {
      Object.keys(config.toggles).forEach(key => out.push({
        id: `toggle-${key}`, group: 'toggle',
        label: `${config.toggles[key] ? 'Disable' : 'Enable'} ${key.replace(/_/g, ' ')}`,
        run: () => saveConfig({ toggles: { [key]: !config.toggles[key] } }),
      }));
    }
    out.push({ id: 'logs', group: 'view', label: showLogs ? 'Hide live logs' : 'Show live logs', hint: 'L', run: () => setShowLogs(v => !v) });
    out.push({ id: 'control', group: 'view', label: 'Show control rail', run: () => { setRightTab('control'); setMobilePane('control'); } });
    out.push({ id: 'inspect', group: 'view', label: 'Show inspector', run: () => { setRightTab('inspect'); setMobilePane('inspect'); } });
    MANAGE_TABS.forEach(t => out.push({ id: `manage-${t.id}`, group: 'manage', label: `Open ${t.label}`, run: () => openManage(t.id) }));
    out.push({
      id: 'export-turn', group: 'export', label: 'Export this turn as JSON',
      run: () => detail ? downloadJson(`koda-turn-${detail.id}.json`, detail) : pushToast('No turn selected', 'error'),
    });
    out.push({
      id: 'export-all', group: 'export', label: 'Export all turn summaries',
      run: () => downloadJson('koda-trace.json', turns),
    });
    out.push({
      id: 'clear-trace', group: 'trace', label: 'Clear the trace ring',
      run: async () => {
        try { await fetch('/api/trace', { method: 'DELETE' }); setSelectedId(null); setDetail(null); pushToast('Trace cleared', 'success'); }
        catch (e) { pushToast(String(e), 'error'); }
      },
    });
    return out;
  }, [turns, config, showLogs, detail, openManage, pushToast]);

  React.useEffect(() => {
    const handler = (e) => {
      const typing = ['INPUT', 'TEXTAREA', 'SELECT'].includes(e.target.tagName);
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault(); setPaletteOpen(v => !v); return;
      }
      if (typing) return;
      if (e.key === 'l' || e.key === 'L') { setShowLogs(v => !v); }
      if (e.key === 'Escape') { setManage(null); }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  const liveBadge = live ? `#${live.id} running` : 'idle';
  const modeChip = config ? config.mode : '';
  const modelChip = config ? config.model : '';

  return (
    <div className="flex flex-col h-screen overflow-hidden bg-canvas text-ink">
      <header className="topbar shrink-0 min-h-[58px] border-b px-3 md:px-4 flex items-center gap-3">
        <div className="w-8 h-8 rounded-lg bg-indigo-500 flex items-center justify-center text-white text-[13px] font-semibold shrink-0">K</div>
        <div className="min-w-0">
          <div className="text-[14px] font-semibold tracking-tight text-zinc-100">koda · control center</div>
          <div className="hidden sm:flex items-center gap-2 text-[11px] text-subtle font-mono">
            <span className={`w-1.5 h-1.5 rounded-full ${connected ? 'bg-emerald-400' : 'bg-rose-400'}`} />
            <span>{connected ? 'runtime live' : 'reconnecting'}</span>
            <span aria-hidden="true">·</span>
            <span className={live ? 'text-indigo-300' : ''}>{liveBadge}</span>
            {modeChip && <><span aria-hidden="true">·</span><span className="uppercase">{modeChip}</span></>}
            {modelChip && <><span aria-hidden="true">·</span><span className="truncate max-w-[180px]">{modelChip}</span></>}
          </div>
        </div>

        <div className="ml-auto flex items-center gap-1.5">
          <button type="button" onClick={() => setPaletteOpen(true)}
            className="control inline-flex items-center gap-2 px-2.5 text-[12px]" title="Command palette">
            <span className="hidden sm:inline">Commands</span>
            <kbd className="nav-key inline-flex h-5 px-1.5 items-center rounded text-[10px] font-mono">⌘K</kbd>
          </button>
          <button type="button" onClick={() => setShowLogs(v => !v)} aria-pressed={showLogs}
            className="control px-2.5 text-[12px]" title="Toggle live logs (L)">Logs</button>
          {/* Agents and the prompt used to be reachable only by opening
              "Manage" -- which lands on Code Graph -- and noticing the tabs.
              They are the two things people come here to change, so they get
              their own buttons. */}
          <button type="button" onClick={() => openManage('skills')}
            className="control px-2.5 text-[12px]" title="Create and edit agents and skills">Agents</button>
          <button type="button" onClick={() => openManage('prompt')}
            className="control px-2.5 text-[12px]" title="View and edit the system prompt">Prompt</button>
          <button type="button" onClick={() => openManage('graph')}
            className="control px-2.5 text-[12px]" title="Code graph and raw captures">Manage</button>
        </div>
      </header>

      {traceError && (
        <div className="shrink-0 px-4 py-2 border-b border-rose-500/25 bg-rose-500/[0.08] text-[12px] text-rose-200">
          {traceError}
        </div>
      )}

      {/* Mobile pane switcher: one region at a time, no horizontal scrolling. */}
      <div className="md:hidden shrink-0 flex border-b border-line" role="tablist" aria-label="Panes">
        {[['turns', 'Turns'], ['trace', 'Trace'], ['inspect', 'Inspect'], ['control', 'Control']].map(([id, label]) => (
          <button key={id} type="button" role="tab" aria-selected={mobilePane === id}
            onClick={() => { setMobilePane(id); if (id === 'inspect' || id === 'control') setRightTab(id); }}
            className={`flex-1 py-2 text-[12px] font-medium ${mobilePane === id ? 'text-indigo-200 bg-accent-soft' : 'text-muted'}`}>
            {label}
          </button>
        ))}
      </div>

      <main className="flex-1 min-h-0 flex overflow-hidden">
        <aside className={`${mobilePane === 'turns' ? 'flex' : 'hidden'} md:flex flex-col w-full md:w-[268px] shrink-0 border-r border-line min-h-0`}>
          <TraceRail turns={turns} selectedId={selectedId} onSelect={selectTurn} loading={!trace} />
        </aside>

        <section className={`${mobilePane === 'trace' ? 'flex' : 'hidden'} md:flex flex-col flex-1 min-w-0 min-h-0`}>
          <TraceWaterfall turn={detail} selectedSeq={selectedSeq}
            onSelect={seq => { setSelectedSeq(seq); setRightTab('inspect'); setMobilePane('inspect'); }} />
        </section>

        <aside className={`${(mobilePane === 'inspect' || mobilePane === 'control') ? 'flex' : 'hidden'} md:flex flex-col w-full md:w-[380px] shrink-0 border-l border-line min-h-0`}>
          <div className="hidden md:flex shrink-0 border-b border-line" role="tablist" aria-label="Right pane">
            {[['inspect', 'Inspector'], ['control', 'Control']].map(([id, label]) => (
              <button key={id} type="button" role="tab" aria-selected={rightTab === id}
                onClick={() => { setRightTab(id); setMobilePane(id); }}
                className={`flex-1 py-2 text-[12px] font-medium ${rightTab === id ? 'text-indigo-200 bg-accent-soft' : 'text-muted hover:text-ink'}`}>
                {label}
              </button>
            ))}
          </div>
          <div className="flex-1 min-h-0">
            {rightTab === 'control' ? (
              <ControlRail config={config} memory={memory} learning={learning} sessions={sessions}
                onSaveConfig={saveConfig} onMemory={sendMemory} onLearning={sendLearning}
                onSession={sendSession} busy={busy} />
            ) : (
              <TraceInspector turn={detail} step={step} prevModelStep={prevModelStep} />
            )}
          </div>
        </aside>
      </main>

      {showLogs && (
        <div className="shrink-0 h-[38vh] border-t border-line flex flex-col min-h-0">
          <div className="shrink-0 px-3 py-1.5 flex items-center gap-2 border-b border-line">
            <span className="text-[11px] font-semibold uppercase tracking-wider text-subtle">Live logs</span>
            <span className="text-[10px] text-subtle font-mono">v{logVersion}</span>
            <button type="button" onClick={() => setShowLogs(false)}
              className="ml-auto text-[11px] text-muted hover:text-ink" aria-label="Hide logs">close</button>
          </div>
          <div className="flex-1 min-h-0">
            <LiveLogs logs={logs} version={logVersion} connected={connected} />
          </div>
        </div>
      )}

      {manage && (
        <div className="fixed inset-0 z-40 flex items-center justify-center p-3 md:p-8 bg-black/60"
          role="dialog" aria-modal="true" aria-label="Manage" onClick={() => setManage(null)}>
          <div className="w-full max-w-5xl h-full max-h-[86vh] rounded-xl border border-line-strong bg-canvas shadow-popover flex flex-col overflow-hidden"
            onClick={e => e.stopPropagation()}>
            <div className="shrink-0 flex items-center gap-1 px-3 py-2 border-b border-line" role="tablist" aria-label="Manage sections">
              {MANAGE_TABS.map(t => (
                <button key={t.id} type="button" role="tab" aria-selected={manage === t.id}
                  onClick={() => openManage(t.id)}
                  className={`px-2.5 py-1.5 rounded-md text-[12px] font-medium ${manage === t.id ? 'bg-accent-soft text-indigo-200' : 'text-muted hover:text-ink hover:bg-white/[0.04]'}`}>
                  {t.label}
                </button>
              ))}
              <button type="button" onClick={() => setManage(null)}
                className="ml-auto control px-2.5 text-[12px]" aria-label="Close manage">Close</button>
            </div>
            <div className="flex-1 min-h-0 overflow-hidden">
              {manage === 'graph' && <CodeGraph graph={graph} loading={graphState.loading} error={graphState.error} />}
              {manage === 'skills' && <AgentsSkills skills={skills} loading={skillsState.loading} error={skillsState.error} onRefresh={reloadSkills} pushToast={pushToast} />}
              {manage === 'prompt' && <SystemPrompt data={settings} loading={settingsState.loading} error={settingsState.error} onRefresh={reloadSettings} pushToast={pushToast} />}
              {manage === 'debug' && <LlmDebug debug={debugData} loading={debugState.loading} error={debugState.error} />}
            </div>
          </div>
        </div>
      )}

      <CommandPalette open={paletteOpen} onClose={() => setPaletteOpen(false)} actions={paletteActions} />

      <div className="fixed z-50 bottom-4 right-4 flex flex-col gap-2 items-end pointer-events-none" aria-live="polite">
        {toasts.map(t => (
          <div key={t.id} role={t.kind === 'error' ? 'alert' : 'status'}
            className={`pointer-events-auto max-w-sm px-3.5 py-2.5 rounded-lg shadow-popover border text-[12px] font-medium animate-toast-in ${
              t.kind === 'success' ? 'bg-[#102019] border-emerald-500/30 text-emerald-200' :
              t.kind === 'error' ? 'bg-[#241216] border-rose-500/30 text-rose-200' :
              'bg-overlay border-line-strong text-zinc-200'}`}>
            {t.message}
          </div>
        ))}
      </div>
    </div>
  );
}
