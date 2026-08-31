// LiveLogs.jsx — Live tailing log viewer with filters, search, and auto-scroll
function LiveLogs({ logs, version, connected }) {
  const [levelFilter, setLevelFilter] = React.useState({ debug: false, info: true, warn: true, error: true });
  const [areaFilter, setAreaFilter] = React.useState('all');
  const [search, setSearch] = React.useState('');
  const [detailLevel, setDetailLevel] = React.useState('simple'); // simple | medium | high
  const [autoScroll, setAutoScroll] = React.useState(true);
  const [expandedSeq, setExpandedSeq] = React.useState(null);
  const containerRef = React.useRef(null);
  const bottomRef = React.useRef(null);

  const areas = React.useMemo(() => {
    const s = new Set();
    logs.forEach(l => s.add(l.area));
    return ['all', ...Array.from(s).sort()];
  }, [logs]);

  const filtered = React.useMemo(() => {
    return logs.filter(entry => {
      if (!levelFilter[entry.level]) return false;
      if (areaFilter !== 'all' && entry.area !== areaFilter) return false;
      if (detailLevel === 'simple' && (entry.level === 'debug')) return false;
      if (detailLevel === 'medium' && entry.level === 'debug' && entry.area !== 'tool') return false;
      if (search && !entry.message.toLowerCase().includes(search.toLowerCase()) &&
          !(entry.fields || []).some(([k,v]) => `${k}${v}`.toLowerCase().includes(search.toLowerCase()))) return false;
      return true;
    });
  }, [logs, levelFilter, areaFilter, search, detailLevel]);

  // Cap DOM nodes — show last 500
  const MAX_VISIBLE = 500;
  const visible = filtered.length > MAX_VISIBLE ? filtered.slice(-MAX_VISIBLE) : filtered;
  const trimmed = filtered.length - visible.length;

  React.useEffect(() => {
    if (autoScroll && bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: 'smooth', block: 'end' });
    }
  }, [visible, autoScroll]);

  const handleScroll = () => {
    if (!containerRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = containerRef.current;
    const atBottom = scrollHeight - scrollTop - clientHeight < 60;
    if (!atBottom && autoScroll) setAutoScroll(false);
  };

  const levelColors = {
    debug: 'text-gray-500',
    info: 'text-cyan-400',
    warn: 'text-amber-400',
    error: 'text-red-400',
  };
  const levelBg = {
    debug: 'bg-gray-500/10 border-gray-600/30',
    info: 'bg-cyan-500/10 border-cyan-500/30',
    warn: 'bg-amber-500/10 border-amber-500/30',
    error: 'bg-red-500/10 border-red-500/30',
  };

  const formatTime = (at) => {
    const d = new Date(at * 1000);
    return d.toLocaleTimeString('en-US', { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' }) +
      '.' + String(d.getMilliseconds()).padStart(3, '0');
  };

  return (
    <div className="flex flex-col h-full">
      {/* Filter bar */}
      <div className="flex flex-wrap items-center gap-2 p-3 border-b border-white/5 bg-white/[0.02] backdrop-blur-sm">
        {/* Level toggles */}
        <div className="flex gap-1" role="group" aria-label="Log level filters">
          {['debug','info','warn','error'].map(lv => (
            <button key={lv} onClick={() => setLevelFilter(f => ({...f, [lv]: !f[lv]}))}
              className={`px-2.5 py-1 rounded-md text-xs font-mono font-semibold uppercase tracking-wide transition-all duration-200 border ${
                levelFilter[lv] ? levelBg[lv] + ' ' + levelColors[lv] : 'bg-transparent border-white/5 text-gray-600'
              } hover:scale-105 active:scale-95`}
              aria-pressed={levelFilter[lv]} aria-label={`Filter ${lv} logs`}>
              {lv}
            </button>
          ))}
        </div>
        <div className="w-px h-5 bg-white/10 mx-1" />
        {/* Area select */}
        <select value={areaFilter} onChange={e => setAreaFilter(e.target.value)}
          className="bg-white/5 border border-white/10 rounded-lg px-2 py-1 text-xs text-gray-300 focus:outline-none focus:ring-1 focus:ring-cyan-500/50"
          aria-label="Filter by area">
          {areas.map(a => <option key={a} value={a}>{a}</option>)}
        </select>
        {/* Detail level */}
        <select value={detailLevel} onChange={e => setDetailLevel(e.target.value)}
          className="bg-white/5 border border-white/10 rounded-lg px-2 py-1 text-xs text-gray-300 focus:outline-none focus:ring-1 focus:ring-cyan-500/50"
          aria-label="Detail level">
          <option value="simple">Simple (info+)</option>
          <option value="medium">Medium (+tool)</option>
          <option value="high">High (all)</option>
        </select>
        {/* Search */}
        <div className="relative flex-1 min-w-[140px] max-w-xs">
          <svg className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-gray-500" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" /></svg>
          <input type="text" value={search} onChange={e => setSearch(e.target.value)}
            placeholder="Search logs…" aria-label="Search logs"
            className="w-full bg-white/5 border border-white/10 rounded-lg pl-7 pr-2 py-1 text-xs text-gray-200 placeholder-gray-600 focus:outline-none focus:ring-1 focus:ring-cyan-500/50" />
        </div>
        {/* Auto-scroll toggle */}
        <button onClick={() => { setAutoScroll(!autoScroll); if (!autoScroll && bottomRef.current) bottomRef.current.scrollIntoView({ behavior: 'smooth' }); }}
          className={`flex items-center gap-1.5 px-2.5 py-1 rounded-lg text-xs font-medium transition-all border ${
            autoScroll ? 'bg-cyan-500/15 border-cyan-500/30 text-cyan-400' : 'bg-white/5 border-white/10 text-gray-500'
          } hover:scale-105 active:scale-95`}
          aria-label={autoScroll ? 'Pause auto-scroll' : 'Resume auto-scroll'}>
          {autoScroll ? (
            <svg className="w-3 h-3" fill="currentColor" viewBox="0 0 20 20"><path d="M10 18a8 8 0 100-16 8 8 0 000 16zM8 7a1 1 0 00-1 1v4a1 1 0 001 1h4a1 1 0 001-1V8a1 1 0 00-1-1H8z"/></svg>
          ) : (
            <svg className="w-3 h-3" fill="currentColor" viewBox="0 0 20 20"><path d="M10 18a8 8 0 100-16 8 8 0 000 16zm-1.5-4.5l5-3.5-5-3.5v7z"/></svg>
          )}
          {autoScroll ? 'Tailing' : 'Paused'}
        </button>
        <span className="text-[10px] text-gray-600 tabular-nums">{filtered.length} entries</span>
      </div>

      {/* Log entries */}
      <div ref={containerRef} onScroll={handleScroll}
        className="flex-1 overflow-y-auto overflow-x-hidden font-mono text-[12px] leading-5 scroll-smooth"
        role="log" aria-live="polite" aria-label="Live log output">
        {trimmed > 0 && (
          <div className="text-center text-gray-600 text-[10px] py-1 border-b border-white/5">
            … {trimmed} older entries hidden …
          </div>
        )}
        {visible.map((entry, i) => (
          <div key={entry.seq}
            className={`group flex items-start gap-2 px-3 py-0.5 border-b border-white/[0.03] hover:bg-white/[0.03] transition-colors duration-150 animate-fade-in cursor-pointer ${
              entry.level === 'error' ? 'bg-red-500/[0.04]' : entry.level === 'warn' ? 'bg-amber-500/[0.02]' : ''
            }`}
            onClick={() => setExpandedSeq(expandedSeq === entry.seq ? null : entry.seq)}
            role="button" tabIndex={0} aria-expanded={expandedSeq === entry.seq}
            onKeyDown={e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); setExpandedSeq(expandedSeq === entry.seq ? null : entry.seq); } }}>
            <span className="text-gray-600 select-none shrink-0 w-[70px]">{formatTime(entry.at)}</span>
            <span className={`shrink-0 w-[42px] font-bold uppercase text-[10px] tracking-wider pt-[1px] ${levelColors[entry.level]}`}>{entry.level}</span>
            <span className="shrink-0 w-[60px] text-purple-400/70 text-[10px] pt-[1px]">{entry.area}</span>
            <span className="text-gray-300 break-all flex-1">
              {search ? highlightText(entry.message, search) : entry.message}
            </span>
            {entry.fields && entry.fields.length > 0 && expandedSeq !== entry.seq && (
              <span className="text-gray-600 text-[10px] shrink-0 opacity-0 group-hover:opacity-100 transition-opacity">
                +{entry.fields.length} fields
              </span>
            )}
          </div>
        ))}
        {visible.length > 0 && expandedSeq !== null && visible.some(e => e.seq === expandedSeq) && (() => {
          const entry = visible.find(e => e.seq === expandedSeq);
          if (!entry || !entry.fields || entry.fields.length === 0) return null;
          return (
            <div className="mx-3 mb-1 p-2 rounded-lg bg-white/[0.03] border border-white/5 text-[11px] animate-fade-in">
              {entry.fields.map(([k,v], i) => (
                <div key={i} className="flex gap-2">
                  <span className="text-purple-400 font-medium">{k}:</span>
                  <span className="text-gray-400 break-all">{v}</span>
                </div>
              ))}
            </div>
          );
        })()}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}

function highlightText(text, query) {
  if (!query) return text;
  const idx = text.toLowerCase().indexOf(query.toLowerCase());
  if (idx === -1) return text;
  return React.createElement(React.Fragment, null,
    text.slice(0, idx),
    React.createElement('mark', { className: 'bg-yellow-500/30 text-yellow-200 rounded px-0.5' }, text.slice(idx, idx + query.length)),
    text.slice(idx + query.length)
  );
}
