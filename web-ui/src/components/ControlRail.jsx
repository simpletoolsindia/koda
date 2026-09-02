// ControlRail.jsx — run koda from the browser: model, mode, autonomy, feature
// toggles, project memory, learned rules, and saved sessions.

function CtlSection({ title, hint, children, action }) {
  return (
    <section className="px-3 py-3 border-b border-line">
      <div className="flex items-center gap-2">
        <h3 className="text-[11px] font-semibold uppercase tracking-wider text-subtle">{title}</h3>
        {action}
      </div>
      {hint && <p className="mt-1 text-[11px] leading-4 text-subtle">{hint}</p>}
      <div className="mt-2 space-y-2">{children}</div>
    </section>
  );
}

function CtlField({ label, children }) {
  return (
    <label className="block">
      <span className="block mb-1 text-[11px] text-muted">{label}</span>
      {children}
    </label>
  );
}

function CtlToggle({ label, checked, onChange, disabled }) {
  return (
    <button type="button" role="switch" aria-checked={!!checked} disabled={disabled}
      onClick={() => onChange(!checked)}
      className="w-full flex items-center justify-between gap-2 px-2 py-1.5 rounded-md border border-line bg-surface hover:border-line-strong transition-colors disabled:opacity-50">
      <span className="text-[12px] text-zinc-300">{label}</span>
      <span className={`relative w-8 h-[18px] rounded-full transition-colors ${checked ? 'bg-indigo-500' : 'bg-zinc-700'}`}>
        <span className={`absolute top-[2px] w-[14px] h-[14px] rounded-full bg-white transition-all ${checked ? 'left-[16px]' : 'left-[2px]'}`} />
      </span>
    </button>
  );
}

const TOGGLE_LABELS = [
  ['learning', 'Self-learning'],
  ['memory', 'Project memory'],
  ['codegraph', 'Code graph'],
  ['web_search', 'Web search'],
  ['web_fetch', 'Web fetch'],
  ['subagents', 'Subagents'],
  ['sessions', 'Save sessions'],
  ['watch', 'Watch mode'],
  ['debug', 'Debug capture'],
];

function ControlRail({ config, memory, learning, sessions, onSaveConfig, onMemory, onLearning, onSession, busy }) {
  const [draft, setDraft] = React.useState(null);
  const [note, setNote] = React.useState('');

  // Adopt server state, but never stomp an edit in progress.
  React.useEffect(() => {
    if (config && !draft) setDraft(config);
  }, [config, draft]);

  if (!config) {
    return <div className="p-4 text-[12px] text-subtle">Loading controls…</div>;
  }
  const cur = draft || config;
  const dirty = JSON.stringify(cur) !== JSON.stringify(config);

  const patch = (fields) => setDraft({ ...cur, ...fields });
  const patchToggle = (key, value) => setDraft({ ...cur, toggles: { ...cur.toggles, [key]: value } });

  const submit = () => {
    onSaveConfig({
      model: cur.model,
      base_url: cur.base_url,
      mode: cur.mode,
      auto_tier: cur.auto_tier,
      reasoning_effort: cur.reasoning_effort,
      temperature: Number(cur.temperature),
      max_steps: Number(cur.max_steps),
      toggles: cur.toggles,
    });
  };

  return (
    <div className="h-full min-h-0 overflow-y-auto">
      <CtlSection title="Model"
        action={dirty ? <span className="ml-auto text-[10px] text-amber-300">unsaved</span> : null}>
        <CtlField label="Model id">
          <input className="form-input !min-h-[32px] !py-1 text-[12px]" value={cur.model}
            onChange={e => patch({ model: e.target.value })} aria-label="Model id" />
        </CtlField>
        <CtlField label="Endpoint">
          <input className="form-input !min-h-[32px] !py-1 text-[12px] font-mono" value={cur.base_url}
            onChange={e => patch({ base_url: e.target.value })} aria-label="Endpoint base URL" />
        </CtlField>
        <div className="grid grid-cols-2 gap-2">
          <CtlField label="Mode">
            <select className="control w-full px-2 text-[12px]" value={cur.mode}
              onChange={e => patch({ mode: e.target.value })} aria-label="Mode">
              {(cur.modes || ['plan', 'execute', 'vibe']).map(m => <option key={m} value={m}>{m}</option>)}
            </select>
          </CtlField>
          <CtlField label="Autonomy">
            <select className="control w-full px-2 text-[12px]" value={cur.auto_tier}
              onChange={e => patch({ auto_tier: e.target.value })} aria-label="Autonomy tier">
              {(cur.tiers || ['ask', 'write', 'full']).map(t => <option key={t} value={t}>{t}</option>)}
            </select>
          </CtlField>
          <CtlField label="Reasoning">
            <select className="control w-full px-2 text-[12px]" value={cur.reasoning_effort}
              onChange={e => patch({ reasoning_effort: e.target.value })} aria-label="Reasoning effort">
              {(cur.efforts || ['off', 'low', 'medium', 'high']).map(x => <option key={x} value={x}>{x}</option>)}
            </select>
          </CtlField>
          <CtlField label="Max steps">
            <input type="number" min="1" max="500" className="form-input !min-h-[32px] !py-1 text-[12px]"
              value={cur.max_steps} onChange={e => patch({ max_steps: e.target.value })} aria-label="Max steps" />
          </CtlField>
        </div>
        <div className="flex items-center gap-2 pt-1">
          <button type="button" onClick={submit} disabled={!dirty || busy}
            className="primary-button px-3 text-[12px] font-medium disabled:opacity-50">
            Apply
          </button>
          {dirty && (
            <button type="button" onClick={() => setDraft(config)} className="control px-3 text-[12px]">
              Reset
            </button>
          )}
          <span className="ml-auto text-[10px] text-subtle">
            {cur.has_api_key ? 'API key set' : 'no API key'}
          </span>
        </div>
      </CtlSection>

      <CtlSection title="Features" hint="Applied to the running session immediately.">
        <div className="space-y-1.5">
          {TOGGLE_LABELS.map(([key, label]) => (
            <CtlToggle key={key} label={label} checked={cur.toggles && cur.toggles[key]}
              onChange={v => patchToggle(key, v)} />
          ))}
        </div>
      </CtlSection>

      <CtlSection title="Memory" hint={memory ? `${(memory.notes || []).length} note(s)` : 'Loading…'}>
        <div className="flex gap-1.5">
          <input className="form-input !min-h-[32px] !py-1 text-[12px]" value={note} placeholder="Remember a project fact…"
            onChange={e => setNote(e.target.value)} aria-label="New memory note"
            onKeyDown={e => { if (e.key === 'Enter' && note.trim()) { onMemory({ remember: note.trim() }); setNote(''); } }} />
          <button type="button" className="control px-2.5 text-[12px] shrink-0" disabled={!note.trim()}
            onClick={() => { onMemory({ remember: note.trim() }); setNote(''); }}>Add</button>
        </div>
        <ul className="space-y-1">
          {(memory && memory.notes || []).slice(-12).reverse().map((n, i) => (
            <li key={i} className="group flex items-start gap-2 px-2 py-1.5 rounded-md border border-line bg-surface">
              <span className="text-[11.5px] leading-4 text-zinc-300 break-words">{n}</span>
              <button type="button" aria-label={`Forget: ${n}`} title="Forget this note"
                onClick={() => onMemory({ forget: n })}
                className="ml-auto shrink-0 text-[10px] text-subtle hover:text-rose-300">✕</button>
            </li>
          ))}
          {memory && (memory.notes || []).length === 0 && (
            <li className="text-[11px] text-subtle">Nothing remembered yet.</li>
          )}
        </ul>
        {memory && (memory.commands || []).length > 0 && (
          <div className="pt-1">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-subtle">Known commands</div>
            <ul className="mt-1 space-y-0.5">
              {memory.commands.slice(0, 6).map((c, i) => (
                <li key={i} className="flex items-center gap-2 text-[11px] font-mono">
                  <span className="truncate text-zinc-400">{c.command}</span>
                  <span className="ml-auto shrink-0 text-emerald-400">{c.ok}✓</span>
                  {c.failed > 0 && <span className="shrink-0 text-rose-400">{c.failed}✗</span>}
                </li>
              ))}
            </ul>
          </div>
        )}
      </CtlSection>

      <CtlSection title="Learned rules"
        hint={learning ? `${(learning.accepted || []).length} accepted · ${(learning.candidates || []).length} pending` : 'Loading…'}
        action={learning && (learning.candidates || []).length > 0 ? (
          <button type="button" onClick={() => onLearning({ accept: 'all' })}
            className="ml-auto text-[10.5px] text-indigo-300 hover:text-indigo-200">Accept all</button>
        ) : null}>
        <ul className="space-y-1" aria-label="Rule candidates">
          {(learning && learning.candidates || []).map((r, i) => (
            <li key={r.key} className="px-2 py-1.5 rounded-md border border-line bg-surface">
              <div className="text-[11.5px] leading-4 text-zinc-300">{r.text}</div>
              <div className="mt-1 flex items-center gap-2">
                <span className="text-[10px] text-subtle font-mono">support {r.support}</span>
                <button type="button" onClick={() => onLearning({ accept: i + 1 })}
                  className="ml-auto text-[10.5px] text-emerald-300 hover:text-emerald-200">Accept</button>
                <button type="button" onClick={() => onLearning({ reject: i + 1 })}
                  className="text-[10.5px] text-rose-300 hover:text-rose-200">Reject</button>
              </div>
            </li>
          ))}
          {learning && (learning.candidates || []).length === 0 && (
            <li className="text-[11px] text-subtle">No pending candidates. koda proposes rules as you work.</li>
          )}
        </ul>
        {learning && (learning.accepted || []).length > 0 && (
          <details className="pt-1">
            <summary className="text-[10.5px] text-subtle cursor-pointer hover:text-muted">
              {learning.accepted.length} accepted rule(s)
            </summary>
            <ul className="mt-1 space-y-0.5">
              {learning.accepted.map(r => (
                <li key={r.key} className="text-[11px] leading-4 text-zinc-400">• {r.text}</li>
              ))}
            </ul>
          </details>
        )}
      </CtlSection>

      <CtlSection title="Sessions" hint="Resume swaps the live conversation; fork copies it first.">
        <ul className="space-y-1">
          {(sessions && sessions.sessions || []).slice(0, 10).map(s => (
            <li key={s.id} className="px-2 py-1.5 rounded-md border border-line bg-surface">
              <div className="text-[11.5px] leading-4 text-zinc-300 line-clamp-2">{s.title || s.id}</div>
              <div className="mt-1 flex items-center gap-2 text-[10px] font-mono text-subtle">
                <span>{s.messages} msg</span>
                <span aria-hidden="true">·</span>
                <span>{s.ago}</span>
                <button type="button" onClick={() => onSession(s.id, 'resume')}
                  className="ml-auto text-[10.5px] font-sans text-indigo-300 hover:text-indigo-200">Resume</button>
                <button type="button" onClick={() => onSession(s.id, 'fork')}
                  className="text-[10.5px] font-sans text-muted hover:text-ink">Fork</button>
              </div>
            </li>
          ))}
          {sessions && (sessions.sessions || []).length === 0 && (
            <li className="text-[11px] text-subtle">No saved sessions in this project.</li>
          )}
        </ul>
      </CtlSection>

      <div className="px-3 py-3 text-[10px] font-mono text-subtle break-all">{config.config_path}</div>
    </div>
  );
}
