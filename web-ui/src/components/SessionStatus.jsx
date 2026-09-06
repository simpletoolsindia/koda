// SessionStatus.jsx — what the session is doing right now: the plan it is
// working through, the step in flight, and how full the context is.
//
// The web UI could change settings and read finished traces, but not answer the
// question people actually have while a turn runs: what is it doing, and how
// far through is it?

function PlanRow({ item }) {
  const mark = item.status === 'done' ? '✓' : item.status === 'in_progress' ? '▸' : '○';
  const tone = item.status === 'done'
    ? 'text-emerald-400'
    : item.status === 'in_progress'
      ? 'text-indigo-300'
      : 'text-subtle';
  return (
    <li className="flex items-start gap-2">
      <span className={`shrink-0 font-mono text-[11px] leading-4 ${tone}`} aria-hidden="true">{mark}</span>
      <span className={`text-[11.5px] leading-4 break-words ${item.status === 'done' ? 'text-subtle line-through' : 'text-zinc-300'}`}>
        {item.text}
      </span>
    </li>
  );
}

function SessionStatus({ status }) {
  if (!status) return null;
  const plan = status.plan || [];
  const done = status.plan_done || 0;
  const pct = status.context_tokens > 0
    ? Math.min(100, Math.round((status.tokens / status.context_tokens) * 100))
    : 0;
  const heavy = pct >= 80;

  return (
    <section className="px-3 py-3 border-b border-line">
      <div className="flex items-center gap-2">
        <h3 className="text-[11px] font-semibold uppercase tracking-wider text-subtle">Session</h3>
        <span className={`ml-auto inline-flex items-center gap-1.5 text-[10.5px] ${status.busy ? 'text-indigo-300' : 'text-subtle'}`}>
          <span className={`w-1.5 h-1.5 rounded-full ${status.busy ? 'bg-indigo-400 animate-pulse' : 'bg-zinc-600'}`} aria-hidden="true" />
          {status.busy ? 'working' : 'idle'}
        </span>
      </div>

      {status.busy && status.activity && (
        <p className="mt-1.5 text-[11.5px] leading-4 text-zinc-300 break-words">{status.activity}</p>
      )}

      <div className="mt-2">
        <div className="flex items-center gap-2 text-[10px] font-mono text-subtle">
          <span>context</span>
          <span className={heavy ? 'text-amber-300' : ''}>
            {status.tokens.toLocaleString()} / {status.context_tokens.toLocaleString()} ({pct}%)
          </span>
        </div>
        <div className="mt-1 h-1 rounded-full bg-zinc-800 overflow-hidden" role="progressbar"
          aria-valuenow={pct} aria-valuemin="0" aria-valuemax="100" aria-label="Context used">
          <div className={`h-full ${heavy ? 'bg-amber-400' : 'bg-indigo-500'}`} style={{ width: `${pct}%` }} />
        </div>
      </div>

      {plan.length > 0 && (
        <div className="mt-3">
          <div className="flex items-center gap-2">
            <span className="text-[10px] font-semibold uppercase tracking-wider text-subtle">Plan</span>
            <span className="ml-auto text-[10px] font-mono text-subtle">{done}/{plan.length}</span>
          </div>
          <ul className="mt-1.5 space-y-1">
            {plan.map((item, i) => <PlanRow key={i} item={item} />)}
          </ul>
        </div>
      )}
    </section>
  );
}
