// TraceInspector.jsx — the payloads behind one step, plus what changed in the
// prompt since the previous model call (which is how compaction and newly
// learned rules become visible instead of silent).

// A minimal LCS diff over arrays of strings. The inputs here are message lists
// and prompt lines — small enough that clarity beats cleverness.
function lineDiff(a, b) {
  const n = a.length, m = b.length;
  if (n * m > 400000) {
    // Pathological input: fall back to a coarse report rather than hanging.
    return [{ op: 'note', text: `payload too large to diff (${n} vs ${m} lines)` }];
  }
  const dp = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const out = [];
  let i = 0, j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) { out.push({ op: 'same', text: a[i] }); i++; j++; }
    else if (dp[i + 1][j] >= dp[i][j + 1]) { out.push({ op: 'del', text: a[i] }); i++; }
    else { out.push({ op: 'add', text: b[j] }); j++; }
  }
  while (i < n) { out.push({ op: 'del', text: a[i++] }); }
  while (j < m) { out.push({ op: 'add', text: b[j++] }); }
  return out;
}

function safeParse(raw) {
  try { return JSON.parse(raw); } catch { return null; }
}

// One line per message, so a diff shows structural change (a message dropped by
// compaction) rather than noise.
function messageLines(request) {
  const body = safeParse(request);
  if (!body || !Array.isArray(body.messages)) return [];
  return body.messages.map((m, i) => {
    const text = typeof m.content === 'string'
      ? m.content
      : Array.isArray(m.content)
        ? m.content.map(p => p.text || `[${p.type}]`).join(' ')
        : m.tool_calls ? m.tool_calls.map(c => `${c.function.name}(${c.function.arguments})`).join(' ') : '';
    const head = (text || '').replace(/\s+/g, ' ').trim().slice(0, 150);
    return `${String(i).padStart(2, '0')} ${m.role}: ${head}`;
  });
}

function systemPromptOf(request) {
  const body = safeParse(request);
  if (!body || !Array.isArray(body.messages)) return '';
  const sys = body.messages.find(m => m.role === 'system');
  return sys && typeof sys.content === 'string' ? sys.content : '';
}

function Payload({ text, empty, mono = true }) {
  if (!text) return <div className="p-4 text-[12px] text-subtle">{empty}</div>;
  return (
    <pre className={`p-3 text-[11.5px] leading-[18px] whitespace-pre-wrap break-words text-zinc-300 ${mono ? 'font-mono' : ''}`}>
      {text}
    </pre>
  );
}

function DiffView({ ops }) {
  if (!ops || ops.length === 0) {
    return <div className="p-4 text-[12px] text-subtle">No previous model call in this turn to compare against.</div>;
  }
  const changed = ops.some(o => o.op === 'add' || o.op === 'del');
  return (
    <div className="p-2">
      {!changed && (
        <div className="mb-2 px-2 py-1.5 rounded-md border border-line bg-surface text-[11.5px] text-subtle">
          The prompt is unchanged apart from the appended turn.
        </div>
      )}
      <pre className="text-[11px] leading-[17px] font-mono whitespace-pre-wrap break-words">
        {ops.map((o, i) => (
          <div key={i} className={
            o.op === 'add' ? 'text-emerald-300 bg-emerald-500/[0.07]' :
            o.op === 'del' ? 'text-rose-300 bg-rose-500/[0.07]' :
            o.op === 'note' ? 'text-amber-300' : 'text-zinc-500'}>
            {o.op === 'add' ? '+ ' : o.op === 'del' ? '- ' : o.op === 'note' ? '! ' : '  '}{o.text}
          </div>
        ))}
      </pre>
    </div>
  );
}

function TraceInspector({ turn, step, prevModelStep }) {
  const [tab, setTab] = React.useState('request');

  const model = step && step.model;
  const tool = step && step.tool;

  const tabs = React.useMemo(() => {
    if (!step) return [];
    if (step.kind === 'tool') {
      const t = [{ id: 'tool', label: 'Tool' }, { id: 'result', label: 'Result' }];
      if (tool && tool.diff) t.push({ id: 'diff', label: 'Change' });
      return t;
    }
    if (step.kind === 'compaction') return [{ id: 'result', label: 'Compaction' }];
    return [
      { id: 'request', label: 'Request' },
      { id: 'response', label: 'Response' },
      { id: 'reasoning', label: 'Reasoning' },
      { id: 'promptdiff', label: 'Prompt Δ' },
    ];
  }, [step, tool]);

  // Keep the selected tab valid as the selection moves between step kinds.
  React.useEffect(() => {
    if (tabs.length > 0 && !tabs.some(t => t.id === tab)) setTab(tabs[0].id);
  }, [tabs, tab]);

  const promptOps = React.useMemo(() => {
    if (!model || !prevModelStep || !prevModelStep.model) return [];
    const prev = prevModelStep.model.request;
    const cur = model.request;
    const sysPrev = systemPromptOf(prev), sysCur = systemPromptOf(cur);
    const ops = [];
    if (sysPrev !== sysCur) {
      ops.push({ op: 'note', text: 'system prompt changed' });
      ops.push(...lineDiff(sysPrev.split('\n'), sysCur.split('\n')));
      ops.push({ op: 'note', text: 'conversation' });
    }
    ops.push(...lineDiff(messageLines(prev), messageLines(cur)));
    return ops;
  }, [model, prevModelStep]);

  if (!step) {
    return (
      <div className="h-full flex items-center justify-center p-6">
        <div className="empty-state w-full px-4 py-6 text-center">
          <div className="text-[13px] font-medium text-zinc-200">Nothing selected</div>
          <p className="mt-1.5 text-[11.5px] leading-5 text-subtle">
            Click a step in the waterfall to see the exact request, response, and tool payload.
          </p>
        </div>
      </div>
    );
  }

  const copyText =
    tab === 'request' ? (model && model.request) :
    tab === 'response' ? (model && model.response) :
    tab === 'reasoning' ? (model && model.reasoning) :
    tab === 'tool' ? (tool && tool.args) :
    tab === 'result' ? (tool ? tool.detail : step.note) :
    tab === 'diff' ? (tool && tool.diff) : '';

  return (
    <div className="flex flex-col h-full min-h-0">
      <div className="shrink-0 px-3 py-2 border-b border-line">
        <div className="flex items-center gap-2">
          <span className="text-[11px] font-semibold uppercase tracking-wider text-subtle">Step {step.seq}</span>
          <span className="text-[11px] text-zinc-300 truncate">{step.label}</span>
          <span className="ml-auto shrink-0"><CopyButton text={copyText} /></span>
        </div>
        <div className="mt-1.5 flex items-center gap-1 overflow-x-auto" role="tablist" aria-label="Step payloads">
          {tabs.map(t => (
            <button key={t.id} type="button" role="tab" aria-selected={tab === t.id}
              onClick={() => setTab(t.id)}
              className={`shrink-0 px-2 py-1 rounded-md text-[11.5px] font-medium transition-colors ${
                tab === t.id ? 'bg-accent-soft text-indigo-200' : 'text-muted hover:text-ink hover:bg-white/[0.04]'
              }`}>
              {t.label}
            </button>
          ))}
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-auto">
        {step.kind === 'model' && (
          <>
            {tab === 'request' && (
              <>
                <div className="px-3 pt-2.5 flex flex-wrap gap-x-3 gap-y-1 text-[10.5px] font-mono text-subtle">
                  <span>{model && model.prompt_tokens} prompt tok (est)</span>
                  <span>{model && model.completion_tokens} completion tok (est)</span>
                  {model && model.retries > 0 && <span className="text-amber-300">{model.retries} retries</span>}
                  {model && model.finish_reason && <span>finish: {model.finish_reason}</span>}
                </div>
                <Payload text={model && model.request} empty="No request captured for this step." />
              </>
            )}
            {tab === 'response' && (
              <>
                {model && model.error && (
                  <div className="m-3 px-3 py-2 rounded-md border border-rose-500/30 bg-rose-500/[0.08] text-[11.5px] text-rose-200">
                    {model.error}
                  </div>
                )}
                {model && model.text && (
                  <div className="px-3 pt-3">
                    <div className="text-[10px] font-semibold uppercase tracking-wider text-subtle">Assistant text</div>
                    <pre className="mt-1 text-[12px] leading-[19px] whitespace-pre-wrap break-words text-zinc-200">{model.text}</pre>
                  </div>
                )}
                <div className="px-3 pt-3 text-[10px] font-semibold uppercase tracking-wider text-subtle">Raw stream</div>
                <Payload text={model && model.response} empty="No response bytes captured." />
              </>
            )}
            {tab === 'reasoning' && (
              <Payload text={model && model.reasoning} mono={false}
                empty="This model sent no reasoning for this call." />
            )}
            {tab === 'promptdiff' && <DiffView ops={promptOps} />}
          </>
        )}

        {step.kind === 'tool' && (
          <>
            {tab === 'tool' && (
              <>
                <div className="px-3 pt-2.5 flex flex-wrap gap-x-3 gap-y-1 text-[10.5px] font-mono text-subtle">
                  <span className={tool && tool.ok ? 'text-emerald-300' : 'text-rose-300'}>{tool && tool.ok ? 'ok' : 'failed'}</span>
                  {tool && tool.approval && <span>approval: {tool.approval}</span>}
                  <span>{fmtMs(step.ms)}</span>
                </div>
                <Payload text={tool && tool.args} empty="This call took no arguments." />
              </>
            )}
            {tab === 'result' && (
              <>
                {tool && tool.summary && (
                  <div className="px-3 pt-3 text-[12px] text-zinc-200">{tool.summary}</div>
                )}
                <Payload text={tool && tool.detail} empty="The tool returned no detail." />
              </>
            )}
            {tab === 'diff' && <Payload text={tool && tool.diff} empty="No change preview for this call." />}
          </>
        )}

        {step.kind === 'compaction' && (
          <div className="p-4">
            <div className="text-[12.5px] text-zinc-200">Context was compacted here.</div>
            <div className="mt-1 text-[12px] font-mono text-amber-300">{step.note || 'no token counts recorded'}</div>
            <p className="mt-3 text-[11.5px] leading-5 text-subtle">
              Everything before this point was replaced by a hand-off note. Compare the prompt on the next
              model call to see exactly what the agent kept.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
