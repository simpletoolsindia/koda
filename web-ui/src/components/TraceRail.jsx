// TraceRail.jsx — reverse-chronological turn list; the live turn pins to the top.

// Status colour is semantic only: it never encodes anything you can't also read.
const TURN_STATUS = {
  running: { dot: 'bg-indigo-400', text: 'text-indigo-300', label: 'running' },
  ok: { dot: 'bg-emerald-400', text: 'text-emerald-300', label: 'ok' },
  error: { dot: 'bg-rose-400', text: 'text-rose-300', label: 'error' },
  cancelled: { dot: 'bg-amber-400', text: 'text-amber-300', label: 'cancelled' },
};

function fmtMs(ms) {
  if (ms == null) return '';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(ms < 10000 ? 1 : 0)}s`;
  const m = Math.floor(ms / 60000);
  return `${m}m ${Math.round((ms % 60000) / 1000)}s`;
}

function fmtClock(at) {
  // `at` is seconds since koda started, which is what the log view shows too.
  const total = Math.max(0, at || 0);
  const m = Math.floor(total / 60);
  const s = (total % 60).toFixed(1).padStart(4, '0');
  return `${String(m).padStart(2, '0')}:${s}`;
}

function TraceRail({ turns, selectedId, onSelect, loading }) {
  const [query, setQuery] = React.useState('');
  const filtered = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return turns;
    return turns.filter(t =>
      (t.input || '').toLowerCase().includes(q) ||
      (t.reply || '').toLowerCase().includes(q) ||
      (t.model || '').toLowerCase().includes(q));
  }, [turns, query]);

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="shrink-0 px-3 py-2.5 border-b border-line">
        <div className="flex items-center justify-between gap-2">
          <span className="text-[11px] font-semibold uppercase tracking-wider text-subtle">Turns</span>
          <span className="text-[10px] text-subtle tabular-nums">{turns.length}</span>
        </div>
        <input type="search" value={query} onChange={e => setQuery(e.target.value)}
          placeholder="Filter turns…" aria-label="Filter turns"
          className="form-input mt-2 !min-h-[30px] !py-1 text-[12px]" />
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto" role="listbox" aria-label="Agent turns">
        {filtered.length === 0 && (
          <div className="p-4">
            <div className="empty-state px-4 py-6 text-center">
              <div className="text-[13px] font-medium text-zinc-200">
                {loading ? 'Loading trace…' : turns.length === 0 ? 'No turns yet' : 'No turns match'}
              </div>
              <p className="mt-1.5 text-[11.5px] leading-5 text-subtle">
                {turns.length === 0
                  ? 'Send a message in koda and the turn appears here as it runs.'
                  : 'Clear the filter to see the full history.'}
              </p>
            </div>
          </div>
        )}
        {filtered.map(t => {
          const st = TURN_STATUS[t.status] || TURN_STATUS.ok;
          const active = t.id === selectedId;
          return (
            <button key={t.id} type="button" role="option" aria-selected={active}
              onClick={() => onSelect(t.id)}
              className={`w-full text-left px-3 py-2.5 border-b border-white/[0.04] transition-colors ${
                active ? 'bg-accent-soft' : 'hover:bg-white/[0.03]'
              }`}>
              <div className="flex items-center gap-2">
                <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${st.dot} ${t.running ? 'animate-pulse' : ''}`} />
                <span className="text-[10px] font-mono text-subtle tabular-nums">#{t.id}</span>
                <span className={`text-[10px] font-medium ${st.text}`}>{st.label}</span>
                <span className="ml-auto text-[10px] font-mono text-subtle tabular-nums">{fmtMs(t.ms)}</span>
              </div>
              <div className={`mt-1 text-[12.5px] leading-[17px] line-clamp-2 ${active ? 'text-zinc-100' : 'text-zinc-300'}`}>
                {t.input || <span className="text-subtle italic">no input</span>}
              </div>
              <div className="mt-1 flex items-center gap-2 text-[10px] text-subtle font-mono">
                <span className="uppercase">{t.mode}</span>
                <span aria-hidden="true">·</span>
                <span>{t.model_calls} model</span>
                <span aria-hidden="true">·</span>
                <span>{t.tool_calls} tools</span>
                {t.tokens > 0 && <><span aria-hidden="true">·</span><span>{t.tokens} tok</span></>}
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
