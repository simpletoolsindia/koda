// AgentsSkills.jsx — Skills & role agents list + editor
function AgentsSkills({ skills, loading, error, onRefresh, pushToast }) {
  const [form, setForm] = React.useState({ name: '', when: '', role: '', body: '', model: '' });
  // Saved providers, so the model box offers real choices instead of asking the
  // user to remember exact ids. Empty until /api/providers answers; the field
  // still accepts free text, because a provider may serve models it does not
  // list and koda should not be the thing standing in the way.
  const [providers, setProviders] = React.useState([]);
  const [models, setModels] = React.useState({});   // provider name -> [model ids]
  React.useEffect(() => {
    fetch('/api/providers')
      .then(r => r.json())
      .then(d => setProviders(d.providers || []))
      .catch(() => {});
  }, []);
  const [submitting, setSubmitting] = React.useState(false);
  const [filter, setFilter] = React.useState('all'); // all | skills | agents

  const loadIntoForm = (s) => {
    setForm({ name: s.name || '', when: s.when || '', role: s.role || '', body: s.body || '', model: s.model || '' });
    window.scrollTo && window.scrollTo(0, 0);
  };

  const resetForm = () => setForm({ name: '', when: '', role: '', body: '', model: '' });

  const remove = async (s, e) => {
    if (e) e.stopPropagation();
    if (!window.confirm(`Delete ${s.role ? 'agent' : 'skill'} "${s.name}"? This removes ${s.source || 'its file'}.`)) return;
    try {
      const res = await fetch('/api/skills/' + encodeURIComponent(s.name), { method: 'DELETE' });
      const data = await res.json();
      if (data.ok) {
        pushToast(`Deleted "${s.name}"`, 'success');
        if (form.name === s.name) resetForm();
        onRefresh();
      } else {
        pushToast(data.error || 'Delete failed', 'error');
      }
    } catch (err) {
      pushToast('Request failed: ' + err.message, 'error');
    }
  };

  const submit = async (e) => {
    e.preventDefault();
    if (!form.name.trim()) { pushToast('Name is required', 'error'); return; }
    setSubmitting(true);
    try {
      const payload = { name: form.name.trim(), when: form.when, body: form.body };
      if (form.role.trim()) payload.role = form.role.trim();
      // Only meaningful for an agent: a knowledge skill is read by whichever
      // model is already running and is never dispatched anywhere.
      if (form.role.trim() && form.model.trim()) payload.model = form.model.trim();
      const res = await fetch('/api/skills', {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(payload),
      });
      const data = await res.json();
      if (data.ok) {
        pushToast(`Saved "${form.name}"${form.role ? ' (agent)' : ''}${data.path ? ' → ' + data.path : ''}`, 'success');
        resetForm();
        onRefresh();
      } else {
        pushToast(data.error || 'Save failed', 'error');
      }
    } catch (err) {
      pushToast('Request failed: ' + err.message, 'error');
    } finally {
      setSubmitting(false);
    }
  };

  const list = skills || [];
  const filtered = list.filter(s => filter === 'all' ? true : filter === 'agents' ? !!s.role : !s.role);
  const agentCount = list.filter(s => s.role).length;
  const skillCount = list.length - agentCount;

  return (
    <div className="flex flex-col lg:flex-row h-full overflow-hidden">
      {/* List */}
      <div className="flex-1 overflow-y-auto bg-canvas p-4 md:p-6">
        <div className="flex items-center gap-2 mb-3">
          <h2 className="text-sm font-semibold text-gray-200">Knowledge Base</h2>
          <div className="flex gap-1 ml-auto" role="group" aria-label="Filter">
            {[['all', `All ${list.length}`], ['skills', `Skills ${skillCount}`], ['agents', `Agents ${agentCount}`]].map(([v, label]) => (
              <button key={v} onClick={() => setFilter(v)}
                className={`px-2.5 py-1 rounded-lg text-xs font-medium transition-all ${
                  filter === v ? 'bg-indigo-500/20 text-indigo-300 border border-indigo-500/30' : 'bg-white/5 text-gray-400 border border-white/10 hover:bg-white/10'
                }`}>{label}</button>
            ))}
          </div>
        </div>

        {loading && list.length === 0 && <div className="text-gray-500 text-sm">Loading…</div>}
        {error && list.length === 0 && (
          <div className="text-sm text-amber-200 bg-amber-500/10 border border-amber-500/20 rounded-lg p-4">{error}</div>
        )}
        {!loading && !error && filtered.length === 0 && <div className="text-gray-500 text-sm p-4 rounded-lg bg-white/[0.02] border border-white/5">No entries. Create one on the right →</div>}

        <div className="grid gap-2 sm:grid-cols-2">
          {filtered.map((s, i) => (
            <div key={i}
              className="relative text-left p-4 rounded-xl bg-surface border border-line hover:border-line-strong hover:bg-raised transition-colors group animate-fade-in shadow-panel">
              <div className="flex items-center gap-2 mb-1">
                <button type="button" onClick={() => loadIntoForm(s)} className="flex items-center gap-2 min-w-0 flex-1 text-left" aria-label={`Edit ${s.name}`}>
                  <span className="font-mono text-sm text-gray-100 font-semibold truncate">{s.name}</span>
                  {s.role ? (
                    <span className="px-1.5 py-0.5 rounded bg-purple-500/20 text-purple-300 text-[10px] font-medium border border-purple-500/30 shrink-0">agent · {s.role}</span>
                  ) : (
                    <span className="px-1.5 py-0.5 rounded bg-emerald-500/20 text-emerald-300 text-[10px] font-medium border border-emerald-500/30 shrink-0">skill</span>
                  )}
                </button>
                <div className="flex items-center gap-1 ml-auto opacity-100 transition-opacity">
                  <button type="button" onClick={() => loadIntoForm(s)} title="Edit" aria-label={`Edit ${s.name}`}
                    className="w-8 h-8 inline-flex items-center justify-center rounded-md text-gray-500 hover:text-indigo-300 hover:bg-white/10">
                    <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z" /></svg>
                  </button>
                  <button type="button" onClick={(e) => remove(s, e)} title="Delete" aria-label={`Delete ${s.name}`}
                    className="w-8 h-8 inline-flex items-center justify-center rounded-md text-gray-500 hover:text-red-400 hover:bg-red-500/10">
                    <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" /></svg>
                  </button>
                </div>
              </div>
              <button type="button" onClick={() => loadIntoForm(s)} className="block w-full text-left" aria-label={`Edit ${s.name} details`}>
                {s.when && <div className="text-[11px] text-gray-500 mb-1"><span className="text-gray-600">when:</span> {s.when}</div>}
                {s.body && <div className="text-[11px] text-gray-400 line-clamp-2 leading-relaxed">{s.body.slice(0, 140)}{s.body.length > 140 ? '…' : ''}</div>}
                {s.source && <div className="text-[10px] text-gray-600 mt-1.5 truncate">📄 {s.source}</div>}
              </button>
            </div>
          ))}
        </div>
      </div>

      {/* Editor */}
      <aside className="lg:w-[420px] shrink-0 border-t lg:border-t-0 lg:border-l border-line overflow-y-auto bg-surface">
        <form onSubmit={submit} className="p-4 space-y-3">
          <div className="flex items-center gap-2">
            <h2 className="text-sm font-semibold text-gray-200">{form.role ? 'Edit / Create Agent' : 'Edit / Create Skill'}</h2>
            {(form.name || form.body) && (
              <button type="button" onClick={resetForm} className="ml-auto text-[11px] text-gray-500 hover:text-gray-300">Clear</button>
            )}
          </div>

          <Field label="Name" required>
            <input value={form.name} onChange={e => setForm({...form, name: e.target.value})}
              placeholder="e.g. rust-error-handling"
              className="form-input" aria-label="Name" required />
          </Field>

          <Field label="When to use">
            <input value={form.when} onChange={e => setForm({...form, when: e.target.value})}
              placeholder="e.g. writing Rust that returns Result"
              className="form-input" aria-label="When to use" />
          </Field>

          <Field label="Role (set to make this an Agent)">
            <input value={form.role} onChange={e => setForm({...form, role: e.target.value})}
              placeholder="e.g. senior-reviewer (optional)"
              className="form-input" aria-label="Role" />
            <p className="text-[10px] text-gray-600 mt-1">Setting a role turns this entry into a role agent.</p>
          </Field>

          {form.role.trim() && (
            <Field label="Model for this agent">
              <div className="flex gap-2">
                <select
                  value={providerOf(form.model)}
                  onChange={e => {
                    const p = e.target.value;
                    const rest = modelOf(form.model);
                    setForm({ ...form, model: p ? (rest ? p + '/' + rest : p + '/') : rest });
                    if (p && !models[p]) loadModels(p, setModels);
                  }}
                  className="form-input w-40" aria-label="Provider">
                  <option value="">(session default)</option>
                  {providers.map(p => (
                    <option key={p.name} value={p.name}>{p.name}</option>
                  ))}
                </select>
                <input
                  list="koda-model-options"
                  value={modelOf(form.model)}
                  onChange={e => {
                    const p = providerOf(form.model);
                    const m = e.target.value;
                    setForm({ ...form, model: p ? (m ? p + '/' + m : p + '/') : m });
                  }}
                  placeholder="model id, e.g. auto or qwen2.5-coder:14b"
                  className="form-input flex-1" aria-label="Model" />
                <datalist id="koda-model-options">
                  {(models[providerOf(form.model)] || []).map(m => (
                    <option key={m} value={m} />
                  ))}
                </datalist>
              </div>
              <p className="text-[10px] text-gray-600 mt-1">
                Leave the provider blank to run this agent on whatever the session
                is using. Pick one and the agent runs on that endpoint instead —
                a reviewer can sit on a careful model while the session stays fast.
              </p>
            </Field>
          )}

          <Field label="Body">
            <textarea value={form.body} onChange={e => setForm({...form, body: e.target.value})}
              placeholder="The knowledge / instructions…" rows={10}
              className="form-input font-mono text-[12px] resize-y" aria-label="Body" />
          </Field>

          <button type="submit" disabled={submitting}
            className="primary-button w-full py-2.5 text-sm font-semibold disabled:opacity-50 disabled:cursor-not-allowed">
            {submitting ? 'Saving…' : (form.role ? 'Save Agent' : 'Save Skill')}
          </button>
        </form>
      </aside>
    </div>
  );
}

// `provider/model` is one string in the skill file; the form edits it as two
// boxes. These keep the split in one place so the two halves cannot disagree.
function providerOf(spec) {
  const i = (spec || '').indexOf('/');
  return i === -1 ? '' : spec.slice(0, i);
}

function modelOf(spec) {
  const i = (spec || '').indexOf('/');
  return i === -1 ? (spec || '') : spec.slice(i + 1);
}

// Ask the provider what it serves, so the model box can suggest. A provider
// that cannot be reached simply offers no suggestions — the field still takes
// whatever is typed.
function loadModels(name, setModels) {
  fetch('/api/providers/' + encodeURIComponent(name) + '/models')
    .then(r => r.json())
    .then(d => setModels(prev => ({ ...prev, [name]: d.models || [] })))
    .catch(() => setModels(prev => ({ ...prev, [name]: [] })));
}

function Field({ label, required, children }) {
  return (
    <label className="block">
      <span className="text-[11px] font-medium text-gray-400 mb-1 block">{label}{required && <span className="text-indigo-400"> *</span>}</span>
      {children}
    </label>
  );
}
