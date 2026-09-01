// SystemPrompt.jsx — view and edit koda's system prompt from the web UI.
function SystemPrompt({ data, loading, error, onRefresh, pushToast }) {
  const [value, setValue] = React.useState('');
  const [seeded, setSeeded] = React.useState(false);
  const [saving, setSaving] = React.useState(false);

  // Seed the editor once data arrives: show the custom prompt if set, else the
  // built-in default so the user edits from something real rather than blank.
  React.useEffect(() => {
    if (data && !seeded) {
      setValue(data.using_builtin ? (data.builtin_prompt || '') : (data.system_prompt || ''));
      setSeeded(true);
    }
  }, [data, seeded]);

  const usingBuiltin = data && data.using_builtin;
  const builtin = (data && data.builtin_prompt) || '';
  const dirty = data && value !== (usingBuiltin ? builtin : (data.system_prompt || ''));

  const save = async () => {
    setSaving(true);
    try {
      const res = await fetch('/api/settings', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ system_prompt: value }),
      });
      const d = await res.json();
      if (d.ok) {
        pushToast(d.using_builtin ? 'Reset to the built-in prompt' : 'System prompt saved', 'success');
        if (d.note) pushToast(d.note, 'info');
        setSeeded(false);
        onRefresh();
      } else {
        pushToast(d.error || 'Save failed', 'error');
      }
    } catch (e) {
      pushToast('Request failed: ' + e.message, 'error');
    } finally { setSaving(false); }
  };

  const resetToBuiltin = () => { setValue(builtin); };

  if (loading && !data) {
    return <div className="flex items-center justify-center h-full text-gray-500">Loading system prompt…</div>;
  }
  if (error && !data) {
    return (
      <div className="flex items-center justify-center h-full p-8">
        <div className="max-w-lg text-center text-sm text-amber-200 bg-amber-500/10 border border-amber-500/20 rounded-2xl p-6">{error}</div>
      </div>
    );
  }

  const chars = value.length;
  const words = value.trim() ? value.trim().split(/\s+/).length : 0;

  return (
    <div className="flex flex-col h-full overflow-hidden">
      <div className="flex flex-wrap items-center gap-3 px-4 md:px-6 py-4 border-b border-line bg-surface">
        <div>
          <h2 className="text-sm font-semibold text-gray-200 flex items-center gap-2">
            System Prompt
            {usingBuiltin
              ? <span className="px-1.5 py-0.5 rounded bg-white/10 text-gray-400 text-[10px] border border-white/10">built-in</span>
              : <span className="px-1.5 py-0.5 rounded bg-indigo-500/20 text-indigo-300 text-[10px] border border-indigo-500/30">custom</span>}
          </h2>
          <p className="text-[11px] text-gray-500 mt-0.5">
            Replaces koda's base instructions. Mode notes, workspace, tools and skills are still layered on.
          </p>
        </div>
        <div className="ml-auto flex items-center gap-2">
          <span className="text-[10px] text-gray-600 tabular-nums">{words} words · {chars} chars</span>
          <button type="button" onClick={resetToBuiltin} disabled={value === builtin}
            className="px-2.5 py-1.5 rounded-lg text-xs font-medium bg-white/5 border border-white/10 text-gray-400 hover:bg-white/10 disabled:opacity-40 disabled:cursor-not-allowed">
            Load built-in
          </button>
          <button type="button" onClick={save} disabled={saving || !dirty}
            className="primary-button px-3 py-1.5 text-xs font-semibold disabled:opacity-50 disabled:cursor-not-allowed">
            {saving ? 'Saving…' : 'Save prompt'}
          </button>
        </div>
      </div>

      <div className="flex-1 overflow-hidden bg-canvas p-4 md:p-6">
        <textarea value={value} onChange={e => setValue(e.target.value)}
          spellCheck={false} aria-label="System prompt"
          placeholder="Type a custom system prompt, or load the built-in and tweak it…"
          className="form-input w-full h-full font-mono text-[12px] leading-relaxed resize-none" />
      </div>

      {data && data.config_path && (
        <div className="px-4 md:px-6 py-2.5 border-t border-line bg-surface text-[10px] text-subtle truncate" title={data.config_path}>
          📄 saved to {data.config_path} · a running koda applies changes on next start
        </div>
      )}
    </div>
  );
}
