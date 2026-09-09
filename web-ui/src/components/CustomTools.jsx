// CustomTools.jsx — teach koda a new action, without writing any config.
//
// The audience is someone who has never edited a TOML file. Three things do the
// heavy lifting: you start from a working example rather than a blank form, the
// fields ask questions instead of naming config keys, and you can run the thing
// and see its real output before anything is saved.

const TOOL_TEMPLATES = [
  {
    id: 'tests',
    icon: '✓',
    label: 'Run the tests',
    blurb: 'Let koda check its own work.',
    tool: {
      name: 'run_tests',
      description: 'Run this project’s test suite and report failures. Use after changing code, and before saying a change is finished.',
      command: 'npm test',
      args: [],
      mutating: false,
    },
  },
  {
    id: 'build',
    icon: '⚙',
    label: 'Build the project',
    blurb: 'Compile, bundle, or type-check.',
    tool: {
      name: 'build',
      description: 'Build the project and report any errors. Use to check that a change compiles.',
      command: 'npm run build',
      args: [],
      mutating: false,
    },
  },
  {
    id: 'search',
    icon: '⌕',
    label: 'Look something up',
    blurb: 'Takes an input you fill in.',
    tool: {
      name: 'find_ticket',
      description: 'Look up a ticket by its id and return the details. Use when the user mentions a ticket number.',
      command: 'curl -s "https://example.com/api/tickets/{ticket_id}"',
      args: ['ticket_id'],
      mutating: false,
    },
  },
  {
    id: 'deploy',
    icon: '↑',
    label: 'Deploy or publish',
    blurb: 'Something that changes the world.',
    tool: {
      name: 'deploy_staging',
      description: 'Deploy the current branch to the staging environment. Use only when the user explicitly asks to deploy.',
      command: './scripts/deploy.sh staging',
      args: [],
      mutating: true,
    },
  },
  {
    id: 'blank',
    icon: '+',
    label: 'Start from scratch',
    blurb: 'An empty tool you fill in.',
    tool: { name: '', description: '', command: '', args: [], mutating: true },
  },
];

const EMPTY = { name: '', description: '', command: '', args: [], mutating: true };

function CustomTools({ pushToast }) {
  const [data, setData] = React.useState(null);
  const [loading, setLoading] = React.useState(true);
  const [error, setError] = React.useState('');
  const [draft, setDraft] = React.useState(null);   // null = nothing open
  const [was, setWas] = React.useState('');         // name before an edit, for rename
  const [picking, setPicking] = React.useState(false);
  const [values, setValues] = React.useState({});   // sample input values for a test
  const [result, setResult] = React.useState(null);
  const [busy, setBusy] = React.useState(false);

  const load = React.useCallback(async () => {
    setLoading(true);
    try {
      const res = await fetch('/api/tools');
      setData(await res.json());
      setError('');
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  React.useEffect(() => { load(); }, [load]);

  const tools = (data && data.tools) || [];
  const builtin = (data && data.builtin) || [];

  function startNew() {
    setPicking(true);
    setDraft(null);
    setResult(null);
  }

  function useTemplate(t) {
    setDraft({ ...t.tool, args: [...t.tool.args] });
    setWas('');
    setValues({});
    setResult(null);
    setPicking(false);
  }

  function edit(t) {
    setDraft({ ...t, args: [...(t.args || [])] });
    setWas(t.name);
    setValues({});
    setResult(null);
    setPicking(false);
  }

  // Every problem the server would reject, said the same way, while you type.
  const problem = React.useMemo(() => {
    if (!draft) return '';
    const n = (draft.name || '').trim();
    if (!n) return 'Give the tool a short name.';
    if (!/^[a-z][a-z0-9_]*$/.test(n)) return 'Use lower-case letters, numbers and underscores — for example run_tests.';
    if (builtin.includes(n)) return `koda already has a tool called ${n}. Pick another name.`;
    if (n !== was && tools.some(t => t.name === n)) return `You already have a tool called ${n}.`;
    if (!(draft.description || '').trim()) return 'Say when koda should use this. It is the only thing the model reads.';
    if (!(draft.command || '').trim()) return 'Enter the command to run.';
    for (const a of draft.args || []) {
      if (!(draft.command || '').includes(`{${a}}`)) return `The command never uses {${a}}. Put it in the command, or remove the input.`;
    }
    return '';
  }, [draft, tools, builtin, was]);

  function addArg() {
    const base = 'input';
    let i = 1;
    let name = base;
    while ((draft.args || []).includes(name)) { i += 1; name = `${base}${i}`; }
    setDraft({ ...draft, args: [...(draft.args || []), name], command: `${draft.command || ''} {${name}}`.trim() });
  }

  function renameArg(old, next) {
    const clean = next.toLowerCase().replace(/[^a-z0-9_]/g, '');
    setDraft({
      ...draft,
      args: draft.args.map(a => (a === old ? clean : a)),
      command: (draft.command || '').split(`{${old}}`).join(`{${clean}}`),
    });
  }

  function removeArg(name) {
    setDraft({
      ...draft,
      args: draft.args.filter(a => a !== name),
      command: (draft.command || '').split(` {${name}}`).join('').split(`{${name}}`).join(''),
    });
  }

  // What the shell will actually receive, quoting included.
  const preview = React.useMemo(() => {
    if (!draft) return '';
    let out = draft.command || '';
    for (const a of draft.args || []) {
      const v = values[a] || '';
      out = out.split(`{${a}}`).join(v ? `'${v.replace(/'/g, "'\\''")}'` : `{${a}}`);
    }
    return out;
  }, [draft, values]);

  async function post(url, payload) {
    const res = await fetch(url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    });
    return res.json();
  }

  async function tryIt() {
    setBusy(true);
    setResult(null);
    try {
      const d = await post('/api/tools/test', { ...draft, values });
      setResult(d);
    } catch (e) {
      setResult({ ok: false, error: String(e) });
    } finally {
      setBusy(false);
    }
  }

  async function save() {
    setBusy(true);
    try {
      const d = await post('/api/tools', { ...draft, was });
      if (d.ok) {
        pushToast(`Saved ${draft.name} — koda can use it now`, 'success');
        setDraft(null);
        setWas('');
        load();
      } else {
        pushToast(d.error || 'Could not save', 'error');
      }
    } finally {
      setBusy(false);
    }
  }

  async function remove(name) {
    setBusy(true);
    try {
      const d = await post('/api/tools', { delete: name });
      if (d.ok) {
        pushToast(`Removed ${name}`, 'success');
        if (was === name) { setDraft(null); setWas(''); }
        load();
      } else {
        pushToast(d.error || 'Could not remove', 'error');
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="h-full flex flex-col md:flex-row min-h-0">
      {/* Left: what you already have. */}
      <div className="md:w-64 shrink-0 border-b md:border-b-0 md:border-r border-line flex flex-col min-h-0">
        <div className="px-3 py-2 flex items-center gap-2 border-b border-line">
          <span className="text-[12px] font-medium">Your tools</span>
          <span className="text-[11px] text-subtle tabular-nums">{tools.length}</span>
          <button type="button" onClick={startNew} className="ml-auto primary-button px-2.5 text-[12px]">
            New tool
          </button>
        </div>
        <div className="flex-1 overflow-y-auto p-2 space-y-1">
          {loading && <p className="text-[12px] text-muted px-1">Loading…</p>}
          {error && <p className="text-[12px] text-rose-300 px-1">{error}</p>}
          {!loading && !tools.length && (
            <div className="empty-state p-3 text-[12px]">
              No tools yet. <strong className="text-ink">New tool</strong> starts from an example.
            </div>
          )}
          {tools.map(t => (
            <div key={t.name}
              className={`rounded-md border px-2 py-1.5 ${was === t.name ? 'border-accent bg-accent-soft' : 'border-line hover:border-line-strong'}`}>
              <button type="button" onClick={() => edit(t)} className="w-full text-left">
                <div className="font-mono text-[12px] text-ink">{t.name}</div>
                <div className="text-[11px] text-muted line-clamp-2">{t.description}</div>
              </button>
              <div className="flex items-center gap-2 mt-1">
                <span className="text-[10px] text-subtle">{t.mutating ? 'Asks first' : 'Runs freely'}</span>
                <button type="button" onClick={() => remove(t.name)}
                  className="ml-auto text-[10px] text-subtle hover:text-rose-300">Remove</button>
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* Right: the editor. */}
      <div className="flex-1 min-h-0 overflow-y-auto p-4">
        {!draft && !picking && (
          <div className="max-w-xl">
            <h2 className="text-[15px] font-medium text-ink">Teach koda something new</h2>
            <p className="text-[13px] text-muted mt-1.5 leading-relaxed">
              A tool is one command koda can run on its own — your test suite, a deploy script,
              a lookup against your own API. You describe when to use it; koda decides when.
            </p>
            <button type="button" onClick={startNew} className="primary-button px-3 mt-4 text-[13px]">
              New tool
            </button>
          </div>
        )}

        {picking && (
          <div className="max-w-2xl">
            <h2 className="text-[15px] font-medium text-ink">Start from an example</h2>
            <p className="text-[13px] text-muted mt-1">Pick the closest one and change it. Nothing is saved until you say so.</p>
            <div className="grid sm:grid-cols-2 gap-2 mt-4">
              {TOOL_TEMPLATES.map(t => (
                <button key={t.id} type="button" onClick={() => useTemplate(t)}
                  className="surface text-left p-3 hover:border-line-strong">
                  <div className="flex items-center gap-2">
                    <span className="text-accent" aria-hidden="true">{t.icon}</span>
                    <span className="text-[13px] font-medium text-ink">{t.label}</span>
                  </div>
                  <div className="text-[12px] text-muted mt-1">{t.blurb}</div>
                </button>
              ))}
            </div>
          </div>
        )}

        {draft && (
          <div className="max-w-2xl space-y-4">
            <div>
              <label htmlFor="ct-name" className="block text-[12px] font-medium text-ink">What should koda call it?</label>
              <p className="text-[11px] text-subtle mt-0.5">Lower-case, no spaces — like <code className="font-mono">run_tests</code>.</p>
              <input id="ct-name" className="form-input font-mono mt-1.5" value={draft.name}
                onChange={e => setDraft({ ...draft, name: e.target.value.toLowerCase().replace(/[^a-z0-9_]/g, '_') })}
                placeholder="run_tests" spellCheck="false" />
            </div>

            <div>
              <label htmlFor="ct-desc" className="block text-[12px] font-medium text-ink">When should koda use it?</label>
              <p className="text-[11px] text-subtle mt-0.5">
                This is the only thing the model reads. Say what it does <em>and</em> when to reach for it.
              </p>
              <textarea id="ct-desc" rows={3} className="form-input mt-1.5 resize-y" value={draft.description}
                onChange={e => setDraft({ ...draft, description: e.target.value })}
                placeholder="Run the test suite and report failures. Use after changing code." />
            </div>

            <div>
              <label htmlFor="ct-cmd" className="block text-[12px] font-medium text-ink">What should it run?</label>
              <p className="text-[11px] text-subtle mt-0.5">A command, exactly as you would type it in a terminal.</p>
              <input id="ct-cmd" className="form-input font-mono mt-1.5" value={draft.command}
                onChange={e => setDraft({ ...draft, command: e.target.value })}
                placeholder="npm test" spellCheck="false" />
            </div>

            <div>
              <div className="flex items-center gap-2">
                <span className="text-[12px] font-medium text-ink">Inputs koda fills in</span>
                <button type="button" onClick={addArg} className="ml-auto control px-2 text-[11px]">Add an input</button>
              </div>
              <p className="text-[11px] text-subtle mt-0.5">
                Optional. Each one becomes <code className="font-mono">{'{name}'}</code> in the command, and koda decides what to put there.
                Values are quoted, so they can never turn into extra commands.
              </p>
              {!(draft.args || []).length && <p className="text-[12px] text-muted mt-2">No inputs — the command always runs the same way.</p>}
              <div className="space-y-1.5 mt-2">
                {(draft.args || []).map(a => (
                  <div key={a} className="flex items-center gap-2">
                    <input aria-label={`Input name ${a}`} className="form-input font-mono flex-1" value={a}
                      onChange={e => renameArg(a, e.target.value)} spellCheck="false" />
                    <input aria-label={`Example value for ${a}`} className="form-input flex-1" value={values[a] || ''}
                      onChange={e => setValues({ ...values, [a]: e.target.value })}
                      placeholder="example value, for testing" />
                    <button type="button" onClick={() => removeArg(a)}
                      className="control px-2 text-[11px]" aria-label={`Remove input ${a}`}>Remove</button>
                  </div>
                ))}
              </div>
            </div>

            <fieldset>
              <legend className="text-[12px] font-medium text-ink">Before it runs</legend>
              <div className="space-y-1.5 mt-1.5">
                <label className="flex items-start gap-2 text-[12px] text-muted">
                  <input type="radio" name="ct-mutating" checked={draft.mutating}
                    onChange={() => setDraft({ ...draft, mutating: true })} className="mt-0.5" />
                  <span><strong className="text-ink">Ask me first.</strong> Choose this if it changes files, data, or anything outside this machine.</span>
                </label>
                <label className="flex items-start gap-2 text-[12px] text-muted">
                  <input type="radio" name="ct-mutating" checked={!draft.mutating}
                    onChange={() => setDraft({ ...draft, mutating: false })} className="mt-0.5" />
                  <span><strong className="text-ink">Just run it.</strong> Only for commands that read something and change nothing.</span>
                </label>
              </div>
            </fieldset>

            <div className="surface p-3">
              <div className="flex items-center gap-2">
                <span className="text-[12px] font-medium text-ink">Try it</span>
                <button type="button" onClick={tryIt} disabled={!!problem || busy}
                  className="ml-auto control px-2.5 text-[12px] disabled:opacity-40">
                  {busy ? 'Running…' : 'Run it once'}
                </button>
              </div>
              <p className="text-[11px] text-subtle mt-1">This runs the command for real, here, and shows what came back. Nothing is saved.</p>
              <pre className="mt-2 text-[11px] font-mono text-muted whitespace-pre-wrap break-all">{preview || '—'}</pre>
              {result && (
                <div className="mt-2 border-t border-line pt-2">
                  {result.ok ? (
                    <>
                      <div className="text-[11px] mb-1">
                        <span className={result.exit === 0 ? 'text-emerald-300' : 'text-amber-300'}>
                          {result.exit === 0 ? '✓ Finished cleanly' : `⚠ Exited with code ${result.exit}`}
                        </span>
                      </div>
                      <pre className="text-[11px] font-mono text-ink whitespace-pre-wrap break-all max-h-56 overflow-y-auto">
                        {result.output || '(no output)'}
                      </pre>
                      {result.clipped && <p className="text-[10px] text-subtle mt-1">Output was long, so this is the first part.</p>}
                    </>
                  ) : (
                    <p className="text-[12px] text-rose-300">{result.error}</p>
                  )}
                </div>
              )}
            </div>

            {problem && (
              <p role="alert" className="text-[12px] text-amber-300">
                <span aria-hidden="true">⚠ </span>{problem}
              </p>
            )}

            <div className="flex items-center gap-2 pb-2">
              <button type="button" onClick={save} disabled={!!problem || busy} className="primary-button px-3 text-[13px] disabled:opacity-40">
                {was ? 'Save changes' : 'Add this tool'}
              </button>
              <button type="button" onClick={() => { setDraft(null); setWas(''); setResult(null); }} className="control px-3 text-[13px]">
                Cancel
              </button>
              {data && data.path && (
                <span className="ml-auto text-[10px] text-subtle font-mono truncate" title={data.path}>{data.path}</span>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
