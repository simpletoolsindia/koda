// Analytics.jsx — what the turn rail cannot show: where the time actually
// goes, which tools earn their keep, and whether the token numbers are the
// server's or koda's own guess.
//
// Everything here is derived from the trace ring that was already being kept,
// so this costs one request and no extra bookkeeping.

function ms(n) {
  if (!n) return '0ms';
  if (n < 1000) return `${n}ms`;
  if (n < 60000) return `${(n / 1000).toFixed(1)}s`;
  return `${Math.floor(n / 60000)}m${String(Math.round((n % 60000) / 1000)).padStart(2, '0')}s`;
}

function num(n) {
  if (n == null) return '—';
  if (n < 1000) return String(n);
  if (n < 1000000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1000000).toFixed(1)}M`;
}

function Stat({ label, value, hint }) {
  return (
    <div className="surface p-3">
      <div className="text-[11px] text-subtle">{label}</div>
      <div className="text-[18px] text-ink tabular-nums mt-0.5">{value}</div>
      {hint && <div className="text-[11px] text-muted mt-0.5">{hint}</div>}
    </div>
  );
}

function Analytics({ pushToast }) {
  const [data, setData] = React.useState(null);
  const [error, setError] = React.useState('');
  const [loading, setLoading] = React.useState(true);

  const load = React.useCallback(async () => {
    setLoading(true);
    try {
      const res = await fetch('/api/analytics');
      setData(await res.json());
      setError('');
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  React.useEffect(() => { load(); }, [load]);

  if (loading && !data) return <div className="p-4 text-[12px] text-muted">Loading…</div>;
  if (error) return <div className="p-4 text-[12px] text-rose-300">{error}</div>;
  if (!data || !data.turns) {
    return (
      <div className="p-4">
        <div className="empty-state p-4 text-[12px] max-w-md">
          Nothing traced yet. Run a turn and its timings appear here.
        </div>
      </div>
    );
  }

  const other = Math.max(0, data.total_ms - data.model_ms - data.tool_ms);
  const share = n => (data.total_ms ? Math.round((n / data.total_ms) * 100) : 0);
  const busiest = data.tools || [];
  const slowestTool = busiest.reduce((a, b) => (b.ms > (a ? a.ms : -1) ? b : a), null);

  return (
    <div className="h-full overflow-y-auto p-4 space-y-4">
      <div className="flex items-center gap-2">
        <h2 className="text-[13px] font-medium text-ink">Across the last {data.turns} turn{data.turns === 1 ? '' : 's'}</h2>
        <button type="button" onClick={load} className="ml-auto control px-2.5 text-[12px]">Refresh</button>
      </div>

      <div className="grid sm:grid-cols-2 lg:grid-cols-4 gap-2">
        <Stat label="Turns" value={data.turns}
          hint={`${data.ok} ok · ${data.errored} failed${data.cancelled ? ` · ${data.cancelled} stopped` : ''}`} />
        <Stat label="Median turn" value={ms(data.median_ms)} hint={`slowest ${ms(data.slowest_ms)}`} />
        <Stat label="Tokens in" value={num(data.prompt_tokens)}
          hint={data.tokens_measured ? 'reported by the server' : 'estimated — the server did not say'} />
        <Stat label="Tokens out" value={num(data.completion_tokens)}
          hint={`${data.model_calls} model call${data.model_calls === 1 ? '' : 's'}`} />
      </div>

      {/* Where the wall-clock went. The bar is the point: it answers "is it the
          model or is it us?" without reading a single number. */}
      <div className="surface p-3">
        <div className="text-[12px] font-medium text-ink">Where the time goes</div>
        <div className="flex h-2 rounded overflow-hidden mt-2 bg-white/[0.04]" role="img"
          aria-label={`waiting on the model ${share(data.model_ms)} percent, running tools ${share(data.tool_ms)} percent, koda itself ${share(other)} percent`}>
          <div className="bg-indigo-500/70" style={{ width: `${share(data.model_ms)}%` }} />
          <div className="bg-emerald-500/70" style={{ width: `${share(data.tool_ms)}%` }} />
          <div className="bg-zinc-600/70" style={{ width: `${share(other)}%` }} />
        </div>
        <div className="flex flex-wrap gap-x-4 gap-y-1 mt-2 text-[11px]">
          <span className="text-indigo-300">■ Waiting on the model {ms(data.model_ms)} ({share(data.model_ms)}%)</span>
          <span className="text-emerald-300">■ Running tools {ms(data.tool_ms)} ({share(data.tool_ms)}%)</span>
          <span className="text-zinc-400">■ koda itself {ms(other)} ({share(other)}%)</span>
        </div>
      </div>

      <div className="surface p-3">
        <div className="flex items-center gap-2">
          <span className="text-[12px] font-medium text-ink">Tools</span>
          {slowestTool && (
            <span className="ml-auto text-[11px] text-muted">
              slowest overall: <span className="font-mono text-ink">{slowestTool.name}</span> ({ms(slowestTool.ms)})
            </span>
          )}
        </div>
        {!busiest.length && <p className="text-[12px] text-muted mt-2">No tools ran in this window.</p>}
        {!!busiest.length && (
          <table className="w-full mt-2 text-[12px]">
            <thead>
              <tr className="text-[11px] text-subtle text-left">
                <th className="font-normal py-1">Tool</th>
                <th className="font-normal py-1 text-right">Calls</th>
                <th className="font-normal py-1 text-right">Total</th>
                <th className="font-normal py-1 text-right">Average</th>
                <th className="font-normal py-1 text-right">Failed</th>
              </tr>
            </thead>
            <tbody>
              {busiest.map(t => (
                <tr key={t.name} className="border-t border-line">
                  <td className="py-1 font-mono text-ink">{t.name}</td>
                  <td className="py-1 text-right tabular-nums text-muted">{t.calls}</td>
                  <td className="py-1 text-right tabular-nums text-muted">{ms(t.ms)}</td>
                  <td className="py-1 text-right tabular-nums text-muted">{ms(Math.round(t.ms / Math.max(1, t.calls)))}</td>
                  <td className={`py-1 text-right tabular-nums ${t.failures ? 'text-rose-300' : 'text-subtle'}`}>
                    {t.failures || '—'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {!data.tokens_measured && (
        <p className="text-[11px] text-muted">
          <span aria-hidden="true">⚠ </span>
          Token counts are estimated at four bytes each, because this server did not report its own usage.
        </p>
      )}
    </div>
  );
}
