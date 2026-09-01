// App.jsx — workspace shell, data polling, toasts, and section routing
async function fetchJson(path) {
  const res = await fetch(path);
  if (res.status === 404) {
    throw new Error(`${path} is missing — your running koda is older than this UI. Rebuild and restart koda (cargo build --release) to enable this tab.`);
  }
  if (!res.ok) throw new Error(`${path} returned ${res.status}`);
  return res.json();
}

const WORKSPACE_TABS = [
  {
    id: 'logs', label: 'Live Logs', short: 'Logs', key: '1',
    description: 'Inspect runtime events, tool activity, retries, and failures as they happen.',
    icon: 'M4 6h16M4 12h16M4 18h10',
  },
  {
    id: 'debug', label: 'LLM Debug', short: 'LLM', key: '2',
    description: 'See the exact prompt, streaming response, reasoning, and tool calls for each turn.',
    icon: 'M8 9l3 3-3 3m5 0h3M5 20h14a2 2 0 002-2V6a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z',
  },
  {
    id: 'graph', label: 'Code Graph', short: 'Graph', key: '3',
    description: 'Explore indexed symbols, definitions, references, and file relationships.',
    icon: 'M8 6a2 2 0 11-4 0 2 2 0 014 0zm12 0a2 2 0 11-4 0 2 2 0 014 0zM14 18a2 2 0 11-4 0 2 2 0 014 0zM7.7 7.2l3.1 8.1m5.5-8.1l-3.1 8.1M8 6h8',
  },
  {
    id: 'skills', label: 'Agents & Skills', short: 'Skills', key: '4',
    description: 'Manage reusable project knowledge and specialized delegation roles.',
    icon: 'M12 3l2.1 4.26L19 8l-3.5 3.4.83 4.8L12 14l-4.33 2.2.83-4.8L5 8l4.9-.74L12 3z',
  },
  {
    id: 'system', label: 'System Prompt', short: 'Prompt', key: '5',
    description: 'Review and tune the operating instructions applied to every koda session.',
    icon: 'M9 5h6M9 9h6m-6 4h4m5 8H6a2 2 0 01-2-2V3h16v16a2 2 0 01-2 2z',
  },
];

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
  const [settings, setSettings] = React.useState(null);
  const [settingsLoading, setSettingsLoading] = React.useState(false);
  const [settingsError, setSettingsError] = React.useState(null);

  const seenSeqRef = React.useRef(-1);
  const logsRef = React.useRef([]);

  const pushToast = React.useCallback((message, kind = 'info') => {
    const id = Math.random().toString(36).slice(2);
    setToasts(items => [...items, { id, message, kind }]);
    setTimeout(() => setToasts(items => items.filter(item => item.id !== id)), 4000);
  }, []);

  const mergeLogs = React.useCallback((data) => {
    if (!data) return;
    if (typeof data.version === 'number') setVersion(data.version);
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
      source.addEventListener('logs', event => {
        try { setConnected(true); mergeLogs(JSON.parse(event.data)); } catch (_) {}
      });
      source.onerror = () => {};
    } catch (_) {}
    return () => { if (source) source.close(); };
  }, [mergeLogs]);

  const loadDebug = React.useCallback(async () => {
    setDebugLoading(true); setDebugError(null);
    try { setDebug(await fetchJson('/api/debug')); }
    catch (error) { setDebugError(error.message); }
    finally { setDebugLoading(false); }
  }, []);
  const loadGraph = React.useCallback(async () => {
    setGraphLoading(true); setGraphError(null);
    try { setGraph(await fetchJson('/api/codegraph')); }
    catch (error) { setGraphError(error.message); }
    finally { setGraphLoading(false); }
  }, []);
  const loadSkills = React.useCallback(async () => {
    setSkillsLoading(true); setSkillsError(null);
    try { setSkills(await fetchJson('/api/skills')); }
    catch (error) { setSkillsError(error.message); }
    finally { setSkillsLoading(false); }
  }, []);
  const loadSettings = React.useCallback(async () => {
    setSettingsLoading(true); setSettingsError(null);
    try { setSettings(await fetchJson('/api/settings')); }
    catch (error) { setSettingsError(error.message); }
    finally { setSettingsLoading(false); }
  }, []);

  React.useEffect(() => {
    if (tab === 'debug' && !debug) loadDebug();
    if (tab === 'graph' && !graph) loadGraph();
    if (tab === 'skills' && !skills) loadSkills();
    if (tab === 'system' && !settings) loadSettings();
  }, [tab, debug, graph, skills, settings, loadDebug, loadGraph, loadSkills, loadSettings]);

  const refreshDebugQuiet = React.useCallback(async () => {
    try {
      const res = await fetch('/api/debug');
      if (res.ok) setDebug(await res.json());
    } catch (_) {}
  }, []);
  React.useEffect(() => {
    if (tab !== 'debug') return;
    const timer = setInterval(refreshDebugQuiet, 1000);
    return () => clearInterval(timer);
  }, [tab, refreshDebugQuiet]);

  React.useEffect(() => {
    const handler = event => {
      if (['INPUT', 'TEXTAREA', 'SELECT'].includes(event.target.tagName)) return;
      const target = WORKSPACE_TABS.find(item => item.key === event.key);
      if (target) setTab(target.id);
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  const active = WORKSPACE_TABS.find(item => item.id === tab) || WORKSPACE_TABS[0];
  const refreshCurrent = () => {
    if (tab === 'debug') loadDebug();
    if (tab === 'graph') loadGraph();
    if (tab === 'skills') loadSkills();
    if (tab === 'system') loadSettings();
  };

  return (
    <div className="flex h-screen overflow-hidden bg-canvas text-ink">
      <aside className="app-sidebar fixed z-40 bottom-0 inset-x-0 h-16 border-t md:static md:w-[236px] md:h-full md:border-t-0 md:border-r flex md:flex-col shrink-0">
        <div className="hidden md:flex items-center gap-3 h-[76px] px-4 border-b border-line">
          <div className="w-9 h-9 rounded-[10px] bg-indigo-500 flex items-center justify-center shadow-lg shadow-indigo-950/30">
            <span className="text-white font-semibold text-[15px] tracking-tight">K</span>
          </div>
          <div className="min-w-0">
            <div className="text-[14px] font-semibold tracking-tight text-zinc-100">koda</div>
            <div className="text-[11px] text-subtle">observability workspace</div>
          </div>
        </div>

        <nav className="flex flex-1 md:flex-none md:block items-stretch justify-around md:px-2.5 md:py-3 md:space-y-1" role="tablist" aria-label="Workspace sections">
          {WORKSPACE_TABS.map(item => (
            <button key={item.id} id={`tab-${item.id}`} type="button"
              role="tab" aria-selected={tab === item.id} aria-controls="workspace-panel"
              aria-label={item.label} data-active={tab === item.id}
              onClick={() => setTab(item.id)}
              className="nav-item group relative flex flex-col md:flex-row items-center justify-center md:justify-start gap-1 md:gap-2.5 flex-1 md:w-full md:h-10 px-1 md:px-2.5 rounded-none md:rounded-lg text-[10px] md:text-[13px] font-medium transition-colors">
              <svg className="w-[18px] h-[18px] shrink-0" fill="none" viewBox="0 0 24 24" stroke="currentColor" aria-hidden="true">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.7} d={item.icon} />
              </svg>
              <span className="md:hidden">{item.short}</span>
              <span className="hidden md:inline truncate">{item.label}</span>
              <kbd className="nav-key hidden md:inline-flex ml-auto min-w-[21px] h-5 px-1 items-center justify-center rounded text-[10px] font-mono">{item.key}</kbd>
            </button>
          ))}
        </nav>

        <div className="hidden md:block mt-auto p-3">
          <div className="rounded-xl border border-line bg-surface p-3">
            <div className="flex items-center justify-between gap-3">
              <div className="flex items-center gap-2 min-w-0">
                <span className={`w-2 h-2 shrink-0 rounded-full ${connected ? 'bg-emerald-400 status-dot' : 'bg-rose-400'}`} />
                <span className="text-[12px] font-medium text-zinc-300">{connected ? 'Runtime live' : 'Reconnecting'}</span>
              </div>
              <span className="text-[10px] text-subtle font-mono tabular-nums">v{version}</span>
            </div>
            <div className="mt-2 text-[11px] leading-4 text-subtle">
              {connected ? `${logs.length} events in this view` : 'Waiting for the local koda server'}
            </div>
          </div>
        </div>
      </aside>

      <section className="min-w-0 flex-1 flex flex-col pb-16 md:pb-0">
        <header className="topbar shrink-0 min-h-[76px] border-b px-4 md:px-6 flex items-center gap-4">
          <div className="md:hidden w-8 h-8 rounded-lg bg-indigo-500 flex items-center justify-center text-white text-sm font-semibold">K</div>
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <div className="text-[15px] md:text-[16px] leading-5 font-semibold tracking-tight text-zinc-100 truncate">{active.label}</div>
              {tab === 'logs' && (
                <span className="hidden sm:inline-flex items-center gap-1.5 rounded-full border border-emerald-500/20 bg-emerald-500/[0.08] px-2 py-0.5 text-[10px] font-medium text-emerald-300">
                  <span className="w-1.5 h-1.5 rounded-full bg-emerald-400" />live tail
                </span>
              )}
            </div>
            <p className="hidden sm:block mt-0.5 max-w-2xl text-[12px] leading-4 text-subtle truncate">{active.description}</p>
          </div>

          <div className="ml-auto flex items-center gap-2">
            <div className="md:hidden flex items-center gap-1.5 px-2 text-[11px] text-muted">
              <span className={`w-2 h-2 rounded-full ${connected ? 'bg-emerald-400' : 'bg-rose-400'}`} />
              <span className="hidden sm:inline">{connected ? 'Live' : 'Offline'}</span>
            </div>
            {tab !== 'logs' && (
              <button type="button" onClick={refreshCurrent}
                className="control inline-flex items-center gap-2 px-3 text-[12px] font-medium"
                aria-label="Refresh data" title="Refresh this view">
                <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" aria-hidden="true">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.8} d="M20 11a8.1 8.1 0 00-15.5-2M4 4v5h5m-5 4a8.1 8.1 0 0015.5 2M20 20v-5h-5" />
                </svg>
                <span className="hidden sm:inline">Refresh</span>
              </button>
            )}
          </div>
        </header>

        <main id="workspace-panel" className="relative flex-1 min-h-0 overflow-hidden" role="tabpanel" aria-labelledby={`tab-${tab}`}>
          <div className={tab === 'logs' ? 'h-full' : 'hidden'}>
            <LiveLogs logs={logs} version={version} connected={connected} />
          </div>
          {tab === 'debug' && <LlmDebug debug={debug} loading={debugLoading} error={debugError} />}
          {tab === 'graph' && <CodeGraph graph={graph} loading={graphLoading} error={graphError} />}
          {tab === 'skills' && <AgentsSkills skills={skills} loading={skillsLoading} error={skillsError} onRefresh={loadSkills} pushToast={pushToast} />}
          {tab === 'system' && <SystemPrompt data={settings} loading={settingsLoading} error={settingsError} onRefresh={loadSettings} pushToast={pushToast} />}
        </main>
      </section>

      <div className="fixed z-50 bottom-20 md:bottom-5 right-4 md:right-5 flex flex-col gap-2 items-end pointer-events-none" aria-live="polite">
        {toasts.map(toast => (
          <div key={toast.id} role={toast.kind === 'error' ? 'alert' : 'status'}
            className={`pointer-events-auto max-w-sm px-3.5 py-2.5 rounded-lg shadow-popover border text-[12px] font-medium animate-toast-in ${
              toast.kind === 'success' ? 'bg-[#102019] border-emerald-500/30 text-emerald-200' :
              toast.kind === 'error' ? 'bg-[#241216] border-rose-500/30 text-rose-200' :
              'bg-overlay border-line-strong text-zinc-200'
            }`}>
            {toast.message}
          </div>
        ))}
      </div>
    </div>
  );
}
