// App.jsx — Root component: nav, data polling, toasts, section routing
function App() {
  const [tab, setTab] = React.useState('logs');
  const [logs, setLogs] = React.useState([]);
  const [version, setVersion] = React.useState(0);
  const [connected, setConnected] = React.useState(false);
  const [toasts, setToasts] = React.useState([]);

  const [debug, setDebug] = React.useState(null);
  const [debugLoading, setDebugLoading] = React.useState(false);
  const [debugError, setDebugError] = React.useState(null);

  const [graph, setGraph] = React.useState(null);
  const [graphLoading, setGraphLoading] = React.useState(false);
  const [graphError, setGraphError] = React.useState(null);

  const [skills, setSkills] = React.useState(null);
  const [skillsLoading, setSkillsLoading] = React.useState(false);
  const [skillsError, setSkillsError] = React.useState(null);

  const seenSeqRef = React.useRef(-1);
  const logsRef = React.useRef([]);

  const pushToast = (message, kind = 'info') => {
    const id = Math.random().toString(36).slice(2);
    setToasts(t => [...t, { id, message, kind }]);
    setTimeout(() => setToasts(t => t.filter(x => x.id !== id)), 4000);
  };

  // Merge new log entries (append-only by seq)
  const mergeLogs = React.useCallback((data) => {
    if (!data) return;
    if (typeof data.version === 'number') setVersion(data.version);
    const entries = data.entries || [];
    if (entries.length === 0) return;
    let maxSeq = seenSeqRef.current;
    const fresh = [];
    for (const e of entries) {
      if (e.seq > seenSeqRef.current) { fresh.push(e); if (e.seq > maxSeq) maxSeq = e.seq; }
    }
    if (fresh.length > 0) {
      seenSeqRef.current = maxSeq;
      logsRef.current = [...logsRef.current, ...fresh];
      // hard cap in memory at 5000
      if (logsRef.current.length > 5000) logsRef.current = logsRef.current.slice(-5000);
      setLogs(logsRef.current.slice());
    }
  }, []);

  // Poll logs every ~1s
  React.useEffect(() => {
    let alive = true;
    const poll = async () => {
      try {
        const since = seenSeqRef.current + 1;
        const res = await fetch(`/api/logs?since=${since}`);
        if (!res.ok) throw new Error('bad status ' + res.status);
        const data = await res.json();
        if (!alive) return;
        setConnected(true);
        mergeLogs(data);
      } catch (e) {
        if (alive) setConnected(false);
      }
    };
    poll();
    const iv = setInterval(poll, 1000);
    return () => { alive = false; clearInterval(iv); };
  }, [mergeLogs]);

  // Optional SSE for lower-latency updates
  React.useEffect(() => {
    let es;
    try {
      es = new EventSource('/api/events');
      es.addEventListener('logs', (ev) => {
        try { const data = JSON.parse(ev.data); setConnected(true); mergeLogs(data); } catch {}
      });
      es.onerror = () => { /* fall back to polling; keep quiet */ };
    } catch {}
    return () => { if (es) es.close(); };
  }, [mergeLogs]);

  // Lazy-load section data
  const loadDebug = React.useCallback(async () => {
    setDebugLoading(true); setDebugError(null);
    try {
      const res = await fetch('/api/debug');
      if (!res.ok) throw new Error('bad status');
      setDebug(await res.json());
    } catch (e) { setDebugError(e.message); } finally { setDebugLoading(false); }
  }, []);

  const loadGraph = React.useCallback(async () => {
    setGraphLoading(true); setGraphError(null);
    try {
      const res = await fetch('/api/codegraph');
      if (!res.ok) throw new Error('bad status');
      setGraph(await res.json());
    } catch (e) { setGraphError(e.message); } finally { setGraphLoading(false); }
  }, []);

  const loadSkills = React.useCallback(async () => {
    setSkillsLoading(true); setSkillsError(null);
    try {
      const res = await fetch('/api/skills');
      if (!res.ok) throw new Error('bad status');
      setSkills(await res.json());
    } catch (e) { setSkillsError(e.message); } finally { setSkillsLoading(false); }
  }, []);

  React.useEffect(() => {
    if (tab === 'debug' && !debug) loadDebug();
    if (tab === 'graph' && !graph) loadGraph();
    if (tab === 'skills' && !skills) loadSkills();
  }, [tab]);

  // Keyboard shortcuts 1-4
  React.useEffect(() => {
    const handler = (e) => {
      if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA' || e.target.tagName === 'SELECT') return;
      const map = { '1': 'logs', '2': 'debug', '3': 'graph', '4': 'skills' };
      if (map[e.key]) setTab(map[e.key]);
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  const tabs = [
    { id: 'logs', label: 'Live Logs', icon: 'M4 6h16M4 12h16M4 18h7' },
    { id: 'debug', label: 'LLM Debug', icon: 'M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z' },
    { id: 'graph', label: 'Code Graph', icon: 'M13 10V3L4 14h7v7l9-11h-7z' },
    { id: 'skills', label: 'Agents & Skills', icon: 'M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z' },
  ];

  return (
    <div className="flex flex-col h-screen text-gray-100">
      {/* Header / Nav */}
      <header className="shrink-0 border-b border-white/10 bg-gradient-to-r from-[#0d0e18]/90 to-[#0a0b12]/90 backdrop-blur-xl">
        <div className="flex items-center gap-4 px-4 h-14">
          {/* Logo */}
          <div className="flex items-center gap-2.5">
            <div className="w-8 h-8 rounded-lg bg-gradient-to-br from-cyan-400 via-blue-500 to-purple-600 flex items-center justify-center shadow-lg shadow-cyan-500/30">
              <span className="text-white font-black text-sm">K</span>
            </div>
            <div>
              <div className="text-sm font-bold tracking-tight bg-gradient-to-r from-cyan-300 to-purple-300 bg-clip-text text-transparent">koda</div>
              <div className="text-[9px] text-gray-500 -mt-0.5 tracking-wider uppercase">observability</div>
            </div>
          </div>

          {/* Tabs */}
          <nav className="flex items-center gap-1 ml-2" role="tablist" aria-label="Sections">
            {tabs.map(t => (
              <button key={t.id} onClick={() => setTab(t.id)}
                role="tab" aria-selected={tab === t.id}
                className={`relative flex items-center gap-2 px-3 py-1.5 rounded-lg text-xs font-medium transition-all ${
                  tab === t.id ? 'text-white' : 'text-gray-500 hover:text-gray-300 hover:bg-white/5'
                }`}>
                {tab === t.id && <span className="absolute inset-0 rounded-lg bg-gradient-to-r from-cyan-500/20 to-purple-500/20 border border-cyan-500/30" />}
                <svg className="relative w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.8} d={t.icon} /></svg>
                <span className="relative hidden sm:inline">{t.label}</span>
              </button>
            ))}
          </nav>

          {/* Status */}
          <div className="ml-auto flex items-center gap-4">
            <div className="flex items-center gap-2 text-xs">
              <span className={`relative flex h-2.5 w-2.5`}>
                {connected && <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-60" />}
                <span className={`relative inline-flex rounded-full h-2.5 w-2.5 ${connected ? 'bg-emerald-400' : 'bg-red-500'}`} />
              </span>
              <span className={connected ? 'text-emerald-400' : 'text-red-400'}>
                {connected ? 'Live' : 'Reconnecting…'}
              </span>
            </div>
            <div className="text-xs text-gray-500 tabular-nums" title="koda log version counter">
              v<span className="text-gray-300 font-medium">{version}</span>
            </div>
          </div>
        </div>
      </header>

      {/* Body */}
      <main className="flex-1 overflow-hidden relative" role="tabpanel">
        <div className={tab === 'logs' ? 'h-full' : 'hidden'}>
          <LiveLogs logs={logs} version={version} connected={connected} />
        </div>
        {tab === 'debug' && <LlmDebug debug={debug} loading={debugLoading} error={debugError} />}
        {tab === 'graph' && <CodeGraph graph={graph} loading={graphLoading} error={graphError} />}
        {tab === 'skills' && <AgentsSkills skills={skills} loading={skillsLoading} error={skillsError} onRefresh={loadSkills} pushToast={pushToast} />}
      </main>

      {/* Refresh button for section data */}
      {tab !== 'logs' && (
        <button onClick={() => { if (tab==='debug') loadDebug(); if (tab==='graph') loadGraph(); if (tab==='skills') loadSkills(); }}
          className="fixed bottom-4 right-4 z-20 w-10 h-10 rounded-full bg-white/10 backdrop-blur border border-white/10 flex items-center justify-center text-gray-300 hover:bg-white/20 hover:rotate-180 transition-all duration-500"
          aria-label="Refresh data" title="Refresh">
          <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" /></svg>
        </button>
      )}

      {/* Toasts */}
      <div className="fixed bottom-4 left-1/2 -translate-x-1/2 z-50 flex flex-col gap-2 items-center pointer-events-none" aria-live="assertive">
        {toasts.map(t => (
          <div key={t.id}
            className={`pointer-events-auto px-4 py-2.5 rounded-xl shadow-2xl backdrop-blur-md border text-sm font-medium animate-toast-in max-w-md ${
              t.kind === 'success' ? 'bg-emerald-500/20 border-emerald-500/40 text-emerald-200' :
              t.kind === 'error' ? 'bg-red-500/20 border-red-500/40 text-red-200' :
              'bg-white/10 border-white/20 text-gray-200'
            }`} role="status">
            {t.message}
          </div>
        ))}
      </div>
    </div>
  );
}
