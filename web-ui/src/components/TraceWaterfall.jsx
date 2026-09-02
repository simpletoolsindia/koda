// TraceWaterfall.jsx — the steps of one turn as a timeline with duration bars.

const STEP_STYLE = {
  model: { bar: 'bg-indigo-500/70', ring: 'border-indigo-500/40', chip: 'text-indigo-300 bg-indigo-500/10 border-indigo-500/25', kind: 'model' },
  tool: { bar: 'bg-emerald-500/70', ring: 'border-emerald-500/40', chip: 'text-emerald-300 bg-emerald-500/10 border-emerald-500/25', kind: 'tool' },
  compaction: { bar: 'bg-amber-500/70', ring: 'border-amber-500/40', chip: 'text-amber-300 bg-amber-500/10 border-amber-500/25', kind: 'compaction' },
};

function stepFailed(step) {
  if (step.kind === 'tool') return step.tool ? !step.tool.ok : false;
  if (step.kind === 'model') return !!(step.model && step.model.error);
  return false;
}

function stepSubtitle(step) {
  if (step.kind === 'model' && step.model) {
    const m = step.model;
    const bits = [];
    if (m.tool_calls && m.tool_calls.length) bits.push(`asked for ${m.tool_calls.join(', ')}`);
    else if (m.text) bits.push(m.text.trim().split('\n')[0].slice(0, 120));
    if (m.finish_reason) bits.push(`finish: ${m.finish_reason}`);
    if (m.error) bits.push(m.error);
    return bits.join(' · ');
  }
  if (step.kind === 'tool' && step.tool) return step.tool.summary || '';
  if (step.kind === 'compaction') return step.note || 'compacting context';
  return step.running ? 'in flight…' : '';
}

function TraceWaterfall({ turn, selectedSeq, onSelect }) {
  const steps = (turn && turn.steps) || [];
  // Bars are laid out against the turn's own span, so a row's offset and width
  // mean the same thing in every turn.
  const span = React.useMemo(() => {
    if (steps.length === 0) return { t0: 0, total: 1 };
    const t0 = turn.started != null ? turn.started : steps[0].started;
    const ends = steps.map(s => s.started + (s.ms || 0) / 1000);
    const t1 = Math.max(turn.ended != null ? turn.ended : 0, ...ends, t0 + 0.001);
    return { t0, total: Math.max(t1 - t0, 0.001) };
  }, [turn, steps]);

  const bottomRef = React.useRef(null);
  const follow = turn && turn.status === 'running';
  React.useEffect(() => {
    if (follow && bottomRef.current) bottomRef.current.scrollIntoView({ block: 'end', behavior: 'smooth' });
  }, [steps.length, follow]);

  // A running turn has no end yet, so its elapsed time comes from the furthest
  // step end rather than a bogus subtraction against null.
  const turnMs = React.useMemo(() => {
    if (!turn) return 0;
    if (turn.ms != null) return turn.ms;
    if (turn.ended != null) return (turn.ended - turn.started) * 1000;
    const last = steps.reduce((acc, s) => Math.max(acc, s.started + (s.ms || 0) / 1000), turn.started);
    return (last - turn.started) * 1000;
  }, [turn, steps]);

  if (!turn) {
    return (
      <div className="h-full flex items-center justify-center p-8">
        <div className="empty-state max-w-md w-full px-6 py-8 text-center">
          <div className="mx-auto mb-3 w-9 h-9 rounded-lg bg-indigo-500/10 border border-indigo-500/20 flex items-center justify-center text-indigo-300 font-mono text-sm">⟶</div>
          <div className="text-sm font-medium text-zinc-200">Pick a turn to trace it</div>
          <p className="mt-1.5 text-[12px] leading-5 text-subtle">
            Every model call, tool call, and compaction in that turn appears here in order, with timings.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="shrink-0 px-4 py-3 border-b border-line">
        <div className="flex items-start gap-3">
          <div className="min-w-0">
            <div className="text-[13px] font-medium text-zinc-100 break-words">
              {turn.input || <span className="italic text-subtle">no input</span>}
            </div>
            <div className="mt-1 flex flex-wrap items-center gap-x-2.5 gap-y-1 text-[10.5px] font-mono text-subtle">
              <span>#{turn.id}</span>
              <span className="uppercase">{turn.mode}</span>
              <span>{turn.model}</span>
              <span>{turn.status === 'running' ? `${fmtMs(turnMs)} so far` : fmtMs(turnMs)}</span>
              {turn.tokens > 0 && <span>{turn.tokens} tok context</span>}
              <span className="truncate max-w-[220px]">{turn.endpoint}</span>
            </div>
          </div>
          <span className={`ml-auto shrink-0 inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-[10px] font-medium ${
            turn.status === 'running' ? 'border-indigo-500/30 bg-indigo-500/10 text-indigo-300' :
            turn.status === 'error' ? 'border-rose-500/30 bg-rose-500/10 text-rose-300' :
            turn.status === 'cancelled' ? 'border-amber-500/30 bg-amber-500/10 text-amber-300' :
            'border-emerald-500/25 bg-emerald-500/10 text-emerald-300'}`}>
            {turn.status === 'running' && <span className="w-1.5 h-1.5 rounded-full bg-indigo-400 animate-pulse" />}
            {turn.status}
          </span>
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto px-2 py-2" role="list" aria-label="Turn steps">
        {steps.length === 0 && (
          <div className="p-6 text-center text-[12px] text-subtle">
            {turn.status === 'running' ? 'Waiting for the first model call…' : 'This turn ran no steps.'}
          </div>
        )}
        {steps.map(step => {
          const style = STEP_STYLE[step.kind] || STEP_STYLE.model;
          const failed = stepFailed(step);
          const active = step.seq === selectedSeq;
          const offset = Math.max(0, Math.min(100, ((step.started - span.t0) / span.total) * 100));
          const width = Math.max(1.2, Math.min(100 - offset, ((step.ms || 0) / 1000 / span.total) * 100));
          const approval = step.tool && step.tool.approval;
          return (
            <button key={step.seq} type="button" role="listitem"
              aria-current={active} onClick={() => onSelect(step.seq)}
              aria-label={`step ${step.seq}: ${step.kind} ${step.label}`}
              className={`w-full text-left mb-1 px-2.5 py-2 rounded-lg border transition-colors ${
                active ? 'bg-accent-soft border-line-strong' : 'border-transparent hover:bg-white/[0.03]'
              }`}>
              <div className="flex items-center gap-2">
                <span className="text-[10px] font-mono text-subtle tabular-nums w-5 shrink-0">{step.seq}</span>
                <span className={`shrink-0 inline-flex items-center rounded border px-1.5 py-[1px] text-[9.5px] font-semibold uppercase tracking-wide ${style.chip}`}>
                  {style.kind}
                </span>
                <span className={`text-[12.5px] font-medium truncate ${failed ? 'text-rose-300' : 'text-zinc-200'}`}>
                  {step.label}
                </span>
                {step.running && (
                  <span className="shrink-0 inline-flex items-center gap-1 text-[10px] text-indigo-300">
                    <span className="w-1.5 h-1.5 rounded-full bg-indigo-400 animate-pulse" />live
                  </span>
                )}
                {failed && <span className="shrink-0 text-[10px] font-medium text-rose-300">failed</span>}
                {approval === 'denied' && <span className="shrink-0 text-[10px] font-medium text-amber-300">denied</span>}
                {approval === 'approved' && <span className="shrink-0 text-[10px] text-emerald-300">approved</span>}
                {step.model && step.model.retries > 0 && (
                  <span className="shrink-0 text-[10px] text-amber-300">{step.model.retries} retr{step.model.retries === 1 ? 'y' : 'ies'}</span>
                )}
                <span className="ml-auto shrink-0 text-[10px] font-mono text-subtle tabular-nums">
                  {step.running ? '…' : fmtMs(step.ms)}
                </span>
              </div>

              {/* Duration bar: offset = when it started, width = how long it took. */}
              <div className="mt-1.5 h-1.5 rounded-full bg-white/[0.04] overflow-hidden" aria-hidden="true">
                <div className={`h-full rounded-full ${failed ? 'bg-rose-500/70' : style.bar} ${step.running ? 'animate-pulse' : ''}`}
                  style={{ marginLeft: `${offset}%`, width: `${width}%` }} />
              </div>

              {stepSubtitle(step) && (
                <div className="mt-1 text-[11px] leading-4 text-subtle line-clamp-2 font-mono">{stepSubtitle(step)}</div>
              )}
            </button>
          );
        })}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
