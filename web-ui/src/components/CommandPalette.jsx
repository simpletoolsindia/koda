// CommandPalette.jsx — Cmd+K: jump to a turn, switch model/mode, toggle a
// feature, query a symbol, export the trace.

function CommandPalette({ open, onClose, actions, onSymbolQuery }) {
  const [query, setQuery] = React.useState('');
  const [cursor, setCursor] = React.useState(0);
  const [symbol, setSymbol] = React.useState(null);
  const inputRef = React.useRef(null);

  React.useEffect(() => {
    if (open) {
      setQuery(''); setCursor(0); setSymbol(null);
      // Focus after paint so the keystroke that opened it isn't captured here.
      setTimeout(() => inputRef.current && inputRef.current.focus(), 0);
    }
  }, [open]);

  const matches = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return actions.slice(0, 40);
    // Subsequence match, so "smem" finds "Show memory".
    const score = (label) => {
      const s = label.toLowerCase();
      if (s.includes(q)) return 0;
      let i = 0;
      for (const ch of q) {
        i = s.indexOf(ch, i);
        if (i === -1) return null;
        i++;
      }
      return 1;
    };
    return actions
      .map(a => ({ a, s: score(`${a.group} ${a.label}`) }))
      .filter(x => x.s !== null)
      .sort((x, y) => x.s - y.s)
      .map(x => x.a)
      .slice(0, 40);
  }, [actions, query]);

  React.useEffect(() => { setCursor(0); }, [query]);

  const runSymbol = async () => {
    const name = query.replace(/^[>@]/, '').trim();
    if (!name) return;
    setSymbol({ loading: true, name });
    try {
      const res = await fetch(`/api/codegraph/symbol?name=${encodeURIComponent(name)}`);
      setSymbol({ ...(await res.json()), loading: false });
    } catch (e) {
      setSymbol({ ok: false, error: String(e), loading: false, name });
    }
  };

  if (!open) return null;

  const onKeyDown = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); onClose(); return; }
    if (e.key === 'ArrowDown') { e.preventDefault(); setCursor(c => Math.min(c + 1, matches.length - 1)); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); setCursor(c => Math.max(c - 1, 0)); return; }
    if (e.key === 'Enter') {
      e.preventDefault();
      if (e.shiftKey || query.startsWith('@')) { runSymbol(); return; }
      const pick = matches[cursor];
      if (pick) { pick.run(); if (!pick.keepOpen) onClose(); }
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center pt-[12vh] px-4 bg-black/60"
      role="dialog" aria-modal="true" aria-label="Command palette" onClick={onClose}>
      <div className="w-full max-w-xl rounded-xl border border-line-strong bg-overlay shadow-popover overflow-hidden animate-fade-in"
        onClick={e => e.stopPropagation()}>
        <input ref={inputRef} type="text" value={query} onChange={e => setQuery(e.target.value)}
          onKeyDown={onKeyDown} placeholder="Run a command, or @symbol to look one up…"
          aria-label="Command or symbol"
          className="w-full px-4 py-3 bg-transparent text-[13.5px] text-ink placeholder:text-subtle border-b border-line focus:outline-none" />

        {symbol ? (
          <div className="max-h-[46vh] overflow-y-auto p-3">
            <div className="flex items-center gap-2">
              <span className="text-[11px] font-semibold uppercase tracking-wider text-subtle">Symbol</span>
              <span className="text-[12px] font-mono text-zinc-200">{symbol.name}</span>
              <button type="button" onClick={() => setSymbol(null)}
                className="ml-auto text-[11px] text-muted hover:text-ink">back to commands</button>
            </div>
            {symbol.loading && <div className="mt-2 text-[12px] text-subtle">Looking it up…</div>}
            {!symbol.loading && (
              <pre className="mt-2 text-[11.5px] leading-[18px] font-mono whitespace-pre-wrap break-words text-zinc-300">
                {symbol.report || symbol.error || 'Nothing found.'}
              </pre>
            )}
          </div>
        ) : (
          <ul className="max-h-[46vh] overflow-y-auto py-1" role="listbox" aria-label="Commands">
            {matches.length === 0 && (
              <li className="px-4 py-3 text-[12px] text-subtle">
                No command matches. Press Shift+Enter to look “{query.trim()}” up in the code graph.
              </li>
            )}
            {matches.map((a, i) => (
              <li key={a.id} role="option" aria-selected={i === cursor}>
                <button type="button" onMouseEnter={() => setCursor(i)}
                  onClick={() => { a.run(); if (!a.keepOpen) onClose(); }}
                  className={`w-full text-left px-4 py-2 flex items-center gap-3 ${i === cursor ? 'bg-accent-soft' : 'hover:bg-white/[0.03]'}`}>
                  <span className="text-[10px] font-mono uppercase tracking-wide text-subtle w-[54px] shrink-0">{a.group}</span>
                  <span className="text-[12.5px] text-zinc-200 truncate">{a.label}</span>
                  {a.hint && <span className="ml-auto shrink-0 text-[10.5px] font-mono text-subtle">{a.hint}</span>}
                </button>
              </li>
            ))}
          </ul>
        )}

        <div className="px-4 py-2 border-t border-line flex items-center gap-3 text-[10px] text-subtle">
          <span>↑↓ navigate</span><span>↵ run</span><span>⇧↵ symbol lookup</span><span>esc close</span>
        </div>
      </div>
    </div>
  );
}
