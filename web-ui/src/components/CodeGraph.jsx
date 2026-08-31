// CodeGraph.jsx — Interactive force-directed graph of code symbols on Canvas
function CodeGraph({ graph, loading, error }) {
  const canvasRef = React.useRef(null);
  const stateRef = React.useRef({ nodes: [], edges: [], transform: { x: 0, y: 0, k: 1 } });
  const [hovered, setHovered] = React.useState(null);
  const [focused, setFocused] = React.useState(null);
  const [search, setSearch] = React.useState('');
  const rafRef = React.useRef(null);
  const dragRef = React.useRef(null);
  const reducedMotion = React.useRef(window.matchMedia('(prefers-reduced-motion: reduce)').matches);

  const kindColors = {
    fn: '#22d3ee', function: '#22d3ee', method: '#38bdf8',
    struct: '#a78bfa', class: '#f472b6', interface: '#c084fc',
    enum: '#fbbf24', trait: '#fb923c', type: '#facc15',
    const: '#34d399', var: '#4ade80', module: '#94a3b8',
  };
  const colorFor = (kind) => kindColors[kind] || '#9ca3af';

  // Build simulation nodes/edges from graph
  React.useEffect(() => {
    if (!graph) return;
    const canvas = canvasRef.current;
    const W = canvas ? canvas.clientWidth : 800;
    const H = canvas ? canvas.clientHeight : 600;

    const symNodes = graph.nodes.map((n, i) => ({
      id: n.id, kind: n.kind, file: n.file, line: n.line, refs: n.refs, type: 'symbol',
      x: W/2 + Math.cos(i) * (100 + Math.random()*200),
      y: H/2 + Math.sin(i) * (100 + Math.random()*200),
      vx: 0, vy: 0,
    }));

    // file nodes derived from edges
    const fileSet = new Map();
    graph.edges.forEach(e => { if (!fileSet.has(e.from)) fileSet.set(e.from, true); });
    const fileNodes = Array.from(fileSet.keys()).map((f, i) => ({
      id: f, kind: 'file', type: 'file', file: f,
      x: W/2 + Math.cos(i*2) * (150 + Math.random()*150),
      y: H/2 + Math.sin(i*2) * (150 + Math.random()*150),
      vx: 0, vy: 0,
    }));

    const nodeMap = new Map();
    [...symNodes, ...fileNodes].forEach(n => nodeMap.set(n.type + ':' + n.id, n));

    const simEdges = graph.edges.map(e => ({
      source: nodeMap.get('file:' + e.from),
      target: nodeMap.get('symbol:' + e.to),
      kind: e.kind,
    })).filter(e => e.source && e.target);

    stateRef.current.nodes = [...symNodes, ...fileNodes];
    stateRef.current.edges = simEdges;
    stateRef.current.transform = { x: 0, y: 0, k: 1 };
    setFocused(null);
    startSim();
    return () => { if (rafRef.current) cancelAnimationFrame(rafRef.current); };
  }, [graph]);

  const startSim = () => {
    let ticks = 0;
    const maxTicks = reducedMotion.current ? 1 : 300;
    const tick = () => {
      const { nodes, edges } = stateRef.current;
      const canvas = canvasRef.current;
      if (!canvas) return;
      const W = canvas.clientWidth, H = canvas.clientHeight;
      const cx = W/2, cy = H/2;

      // repulsion
      for (let i = 0; i < nodes.length; i++) {
        const a = nodes[i];
        for (let j = i+1; j < nodes.length; j++) {
          const b = nodes[j];
          let dx = a.x - b.x, dy = a.y - b.y;
          let dist2 = dx*dx + dy*dy + 0.01;
          const force = 1400 / dist2;
          const dist = Math.sqrt(dist2);
          const fx = (dx/dist) * force, fy = (dy/dist) * force;
          a.vx += fx; a.vy += fy; b.vx -= fx; b.vy -= fy;
        }
      }
      // springs
      for (const e of edges) {
        const dx = e.target.x - e.source.x, dy = e.target.y - e.source.y;
        const dist = Math.sqrt(dx*dx + dy*dy) + 0.01;
        const target = 60;
        const force = (dist - target) * 0.02;
        const fx = (dx/dist)*force, fy = (dy/dist)*force;
        e.source.vx += fx; e.source.vy += fy;
        e.target.vx -= fx; e.target.vy -= fy;
      }
      // centering + integrate
      for (const n of nodes) {
        n.vx += (cx - n.x) * 0.002;
        n.vy += (cy - n.y) * 0.002;
        n.vx *= 0.85; n.vy *= 0.85;
        if (dragRef.current !== n) { n.x += n.vx; n.y += n.vy; }
      }
      draw();
      ticks++;
      if (ticks < maxTicks) rafRef.current = requestAnimationFrame(tick);
    };
    if (rafRef.current) cancelAnimationFrame(rafRef.current);
    tick();
  };

  const draw = () => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    const dpr = window.devicePixelRatio || 1;
    const W = canvas.clientWidth, H = canvas.clientHeight;
    if (canvas.width !== W*dpr) { canvas.width = W*dpr; canvas.height = H*dpr; }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, W, H);

    const t = stateRef.current.transform;
    ctx.translate(t.x, t.y); ctx.scale(t.k, t.k);

    const { nodes, edges } = stateRef.current;
    const q = search.toLowerCase();
    const focusId = focused;

    // edges
    ctx.lineWidth = 0.7 / t.k;
    for (const e of edges) {
      const active = focusId && (e.source.id === focusId || e.target.id === focusId);
      ctx.strokeStyle = active ? 'rgba(34,211,238,0.5)' : 'rgba(148,163,184,0.12)';
      ctx.beginPath();
      ctx.moveTo(e.source.x, e.source.y);
      ctx.lineTo(e.target.x, e.target.y);
      ctx.stroke();
    }

    // nodes
    for (const n of nodes) {
      const isFile = n.type === 'file';
      const r = isFile ? 5 : Math.min(3 + (n.refs || 0) * 0.6, 11);
      const matched = q && n.id.toLowerCase().includes(q);
      const isFocus = focusId === n.id;
      const dim = focusId && !isFocus && !edges.some(e => (e.source.id === focusId && e.target === n) || (e.target.id === focusId && e.source === n));

      ctx.globalAlpha = dim ? 0.15 : 1;
      ctx.beginPath();
      ctx.arc(n.x, n.y, r, 0, Math.PI*2);
      if (isFile) {
        ctx.fillStyle = '#1e293b';
        ctx.strokeStyle = 'rgba(148,163,184,0.4)';
        ctx.fill(); ctx.lineWidth = 1/t.k; ctx.stroke();
      } else {
        ctx.fillStyle = colorFor(n.kind);
        ctx.fill();
      }
      if (matched || isFocus) {
        ctx.strokeStyle = matched ? '#fde047' : '#fff';
        ctx.lineWidth = 2/t.k;
        ctx.stroke();
      }
      ctx.globalAlpha = 1;
      // labels for larger/focused nodes
      if ((r > 7 || isFocus || matched || (hovered && hovered.id === n.id)) && t.k > 0.5) {
        ctx.fillStyle = 'rgba(226,232,240,0.85)';
        ctx.font = `${10/t.k}px ui-monospace, monospace`;
        const label = isFile ? n.id.split('/').pop() : n.id;
        ctx.fillText(label, n.x + r + 2, n.y + 3);
      }
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  };

  React.useEffect(() => { draw(); }, [hovered, focused, search]);

  // Interaction: convert screen to graph coords
  const toGraph = (clientX, clientY) => {
    const canvas = canvasRef.current;
    const rect = canvas.getBoundingClientRect();
    const t = stateRef.current.transform;
    return { x: (clientX - rect.left - t.x) / t.k, y: (clientY - rect.top - t.y) / t.k };
  };
  const nodeAt = (gx, gy) => {
    const { nodes } = stateRef.current;
    for (let i = nodes.length - 1; i >= 0; i--) {
      const n = nodes[i];
      const r = (n.type === 'file' ? 5 : Math.min(3 + (n.refs||0)*0.6, 11)) + 4;
      if ((n.x-gx)**2 + (n.y-gy)**2 < r*r) return n;
    }
    return null;
  };

  const onMouseDown = (e) => {
    const g = toGraph(e.clientX, e.clientY);
    const n = nodeAt(g.x, g.y);
    if (n) { dragRef.current = n; }
    else { dragRef.current = { pan: true, sx: e.clientX, sy: e.clientY, ox: stateRef.current.transform.x, oy: stateRef.current.transform.y }; }
  };
  const onMouseMove = (e) => {
    const d = dragRef.current;
    if (d && d.pan) {
      stateRef.current.transform.x = d.ox + (e.clientX - d.sx);
      stateRef.current.transform.y = d.oy + (e.clientY - d.sy);
      draw();
      return;
    }
    if (d) {
      const g = toGraph(e.clientX, e.clientY);
      d.x = g.x; d.y = g.y; d.vx = 0; d.vy = 0;
      draw();
      return;
    }
    const g = toGraph(e.clientX, e.clientY);
    const n = nodeAt(g.x, g.y);
    setHovered(n && n.type === 'symbol' ? n : (n || null));
    canvasRef.current.style.cursor = n ? 'pointer' : 'grab';
  };
  const onMouseUp = (e) => {
    const d = dragRef.current;
    if (d && !d.pan && Math.abs(d.vx) < 0.01) {
      // treat as click-focus
    }
    if (d && !d.pan) {
      const g = toGraph(e.clientX, e.clientY);
      const n = nodeAt(g.x, g.y);
      if (n && n === d) setFocused(focused === n.id ? null : n.id);
    }
    dragRef.current = null;
  };
  const onWheel = (e) => {
    e.preventDefault();
    const t = stateRef.current.transform;
    const rect = canvasRef.current.getBoundingClientRect();
    const mx = e.clientX - rect.left, my = e.clientY - rect.top;
    const scale = e.deltaY < 0 ? 1.1 : 0.9;
    const nk = Math.max(0.2, Math.min(4, t.k * scale));
    t.x = mx - (mx - t.x) * (nk / t.k);
    t.y = my - (my - t.y) * (nk / t.k);
    t.k = nk;
    draw();
  };

  if (loading && !graph) return <div className="flex items-center justify-center h-full text-gray-500">Building code graph…</div>;
  if (error && !graph) return <div className="flex items-center justify-center h-full text-red-400">Failed to load code graph.</div>;

  const languages = (graph && graph.languages) || [];
  const usedKinds = graph ? Array.from(new Set(graph.nodes.map(n => n.kind))) : [];

  return (
    <div className="relative h-full">
      {/* Top controls */}
      <div className="absolute top-3 left-3 right-3 z-10 flex flex-wrap items-center gap-2 pointer-events-none">
        <div className="relative pointer-events-auto">
          <input type="text" value={search} onChange={e => setSearch(e.target.value)}
            placeholder="Highlight symbol…" aria-label="Search symbols to highlight"
            className="w-52 bg-black/40 backdrop-blur border border-white/10 rounded-lg px-3 py-1.5 text-xs text-gray-200 placeholder-gray-500 focus:outline-none focus:ring-1 focus:ring-cyan-500/50" />
        </div>
        {graph && (
          <div className="pointer-events-auto px-2.5 py-1.5 rounded-lg bg-black/40 backdrop-blur border border-white/10 text-[11px] text-gray-400">
            {graph.files} files · {graph.nodes.length} symbols · {graph.edges.length} edges
          </div>
        )}
        {focused && (
          <button onClick={() => setFocused(null)} className="pointer-events-auto px-2.5 py-1.5 rounded-lg bg-cyan-500/20 border border-cyan-500/30 text-cyan-300 text-[11px] hover:bg-cyan-500/30">
            ✕ Clear focus
          </button>
        )}
      </div>

      {graph && graph.truncated && (
        <div className="absolute top-14 left-3 z-10 px-3 py-1.5 rounded-lg bg-amber-500/15 border border-amber-500/30 text-amber-300 text-[11px]">
          ⚠ Graph truncated — showing a subset of the codebase.
        </div>
      )}

      {/* Language legend */}
      {languages.length > 0 && (
        <div className="absolute bottom-3 left-3 z-10 p-2.5 rounded-lg bg-black/40 backdrop-blur border border-white/10 max-w-[200px]">
          <div className="text-[10px] uppercase tracking-wider text-gray-500 mb-1.5 font-semibold">Languages</div>
          <div className="space-y-0.5">
            {languages.map(([lang, count], i) => (
              <div key={i} className="flex justify-between gap-3 text-[11px]">
                <span className="text-gray-300">{lang}</span>
                <span className="text-gray-500 tabular-nums">{count}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Kind legend */}
      {usedKinds.length > 0 && (
        <div className="absolute bottom-3 right-3 z-10 p-2.5 rounded-lg bg-black/40 backdrop-blur border border-white/10">
          <div className="text-[10px] uppercase tracking-wider text-gray-500 mb-1.5 font-semibold">Symbol kinds</div>
          <div className="flex flex-wrap gap-x-3 gap-y-1 max-w-[220px]">
            {usedKinds.map(k => (
              <span key={k} className="flex items-center gap-1.5 text-[11px] text-gray-300">
                <span className="w-2 h-2 rounded-full" style={{ background: colorFor(k) }} />{k}
              </span>
            ))}
            <span className="flex items-center gap-1.5 text-[11px] text-gray-300">
              <span className="w-2 h-2 rounded-sm border border-slate-400 bg-slate-700" />file
            </span>
          </div>
        </div>
      )}

      {/* Hover tooltip */}
      {hovered && hovered.type === 'symbol' && (
        <div className="absolute top-14 right-3 z-10 p-3 rounded-lg bg-black/70 backdrop-blur border border-white/10 max-w-xs animate-fade-in">
          <div className="font-mono text-sm text-cyan-300 font-semibold break-all">{hovered.id}</div>
          <div className="text-[11px] text-gray-400 mt-1">
            <span className="px-1.5 py-0.5 rounded bg-white/10 mr-1" style={{ color: colorFor(hovered.kind) }}>{hovered.kind}</span>
            {hovered.refs != null && <span>· {hovered.refs} refs</span>}
          </div>
          {hovered.file && <div className="text-[10px] text-gray-500 mt-1 break-all">{hovered.file}{hovered.line ? ':' + hovered.line : ''}</div>}
        </div>
      )}

      <canvas ref={canvasRef}
        className="w-full h-full block"
        style={{ cursor: 'grab' }}
        onMouseDown={onMouseDown} onMouseMove={onMouseMove} onMouseUp={onMouseUp} onMouseLeave={() => { dragRef.current = null; setHovered(null); }}
        onWheel={onWheel}
        role="img" aria-label="Interactive code symbol graph" />

      {graph && graph.nodes.length === 0 && (
        <div className="absolute inset-0 flex items-center justify-center text-gray-600 text-sm">No symbols indexed yet.</div>
      )}
    </div>
  );
}
