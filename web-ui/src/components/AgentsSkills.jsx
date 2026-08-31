// AgentsSkills.jsx — Skills & role agents list + editor
function AgentsSkills({ skills, loading, error, onRefresh, pushToast }) {
  const [form, setForm] = React.useState({ name: '', when: '', role: '', body: '' });
  const [submitting, setSubmitting] = React.useState(false);
  const [filter, setFilter] = React.useState('all'); // all | skills | agents

  const loadIntoForm = (s) => {
    setForm({ name: s.name || '', when: s.when || '', role: s.role || '', body: s.body || '' });
    window.scrollTo && window.scrollTo(0, 0);
  };

  const resetForm = () => setForm({ name: '', when: '', role: '', body: '' });

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
      <div className="flex-1 overflow-y-auto p-4">
        <div className="flex items-center gap-2 mb-3">
          <h2 className="text-sm font-semibold text-gray-200">Knowledge Base</h2>
          <div className="flex gap-1 ml-auto" role="group" aria-label="Filter">
            {[['all', `All ${list.length}`], ['skills', `Skills ${skillCount}`], ['agents', `Agents ${agentCount}`]].map(([v, label]) => (
              <button key={v} onClick={() => setFilter(v)}
                className={`px-2.5 py-1 rounded-lg text-xs font-medium transition-all ${
                  filter === v ? 'bg-cyan-500/20 text-cyan-300 border border-cyan-500/30' : 'bg-white/5 text-gray-400 border border-white/10 hover:bg-white/10'
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
              className="relative text-left p-3 rounded-xl bg-white/[0.03] border border-white/5 hover:border-cyan-500/30 hover:bg-white/[0.05] transition-all group animate-fade-in">
              <div className="flex items-center gap-2 mb-1">
                <button type="button" onClick={() => loadIntoForm(s)} className="flex items-center gap-2 min-w-0 flex-1 text-left" aria-label={`Edit ${s.name}`}>
                  <span className="font-mono text-sm text-gray-100 font-semibold truncate">{s.name}</span>
                  {s.role ? (
                    <span className="px-1.5 py-0.5 rounded bg-purple-500/20 text-purple-300 text-[10px] font-medium border border-purple-500/30 shrink-0">agent · {s.role}</span>
                  ) : (
                    <span className="px-1.5 py-0.5 rounded bg-emerald-500/20 text-emerald-300 text-[10px] font-medium border border-emerald-500/30 shrink-0">skill</span>
                  )}
                </button>
                <div className="flex items-center gap-1 ml-auto opacity-0 group-hover:opacity-100 transition-opacity">
                  <button type="button" onClick={() => loadIntoForm(s)} title="Edit" aria-label={`Edit ${s.name}`}
                    className="p-1 rounded text-gray-500 hover:text-cyan-300 hover:bg-white/10">
                    <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z" /></svg>
                  </button>
                  <button type="button" onClick={(e) => remove(s, e)} title="Delete" aria-label={`Delete ${s.name}`}
                    className="p-1 rounded text-gray-500 hover:text-red-400 hover:bg-red-500/10">
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
      <aside className="lg:w-96 shrink-0 border-t lg:border-t-0 lg:border-l border-white/5 overflow-y-auto bg-white/[0.02]">
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

          <Field label="Body">
            <textarea value={form.body} onChange={e => setForm({...form, body: e.target.value})}
              placeholder="The knowledge / instructions…" rows={10}
              className="form-input font-mono text-[12px] resize-y" aria-label="Body" />
          </Field>

          <button type="submit" disabled={submitting}
            className="w-full py-2.5 rounded-xl bg-gradient-to-r from-cyan-500 to-blue-500 text-white text-sm font-semibold hover:from-cyan-400 hover:to-blue-400 transition-all disabled:opacity-50 active:scale-[0.98] shadow-lg shadow-cyan-500/20">
            {submitting ? 'Saving…' : (form.role ? 'Save Agent' : 'Save Skill')}
          </button>
        </form>
      </aside>
    </div>
  );
}

function Field({ label, required, children }) {
  return (
    <label className="block">
      <span className="text-[11px] font-medium text-gray-400 mb-1 block">{label}{required && <span className="text-cyan-400"> *</span>}</span>
      {children}
    </label>
  );
}
