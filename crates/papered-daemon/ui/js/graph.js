// Graph view: interactive paper relatedness network and timeline.
// Registered as route('/graph'). Rendered with vanilla SVG — no chart
// dependency. Edge weights come from keyword overlap + shared biological
// entities (see crates/papered/src/search/graph.rs).

import * as U from './util.js?v=1';
import { API } from './api.js?v=1';
import { route } from './router.js?v=1';

// ---- module state (restored on re-entry) --------------------------------

const state = {
  limit: 100,
  degree: 4,
  mode: 'network', // 'network' | 'timeline'
  graph: null,     // { nodes, edges } from the API
  selected: null,  // selected node id
  expanded: new Set(), // timeline lane labels with the card cap lifted
};

// "All papers" sends the backend's max supported graph size; the daemon caps
// at MAX_RESULT_LIMIT (1000) so larger values just mean "everything indexed".
const GRAPH_ALL_LIMIT = 1000;

let gen = 0;             // generation counter; stale async callbacks bail out
let rafId = 0;           // active network animation frame
let nodeById = new Map(); // id -> node, rebuilt on every load
let view = null;         // active renderer handles { mode, byId, nodes, edges, nodeEls, edgeEls }
let hoverId = null;      // timeline hover focus (link tracing without pinning)
let foundEl = null;      // last search-located node element (pulsed)
let foundTimer = 0;      // pending removal of the pulse class
let libraryCache = null; // all indexed papers (light nodes), for library-wide search
let libraryPromise = null; // in-flight library fetch (deduped)

// ---- shared helpers -----------------------------------------------------

// Node radius relative to the most-connected node in the CURRENT graph
// (0..1 → 4..9px), with a sqrt curve so mid-degree nodes stay readable.
// Scaling by the graph's max degree keeps the visual density stable when
// "Links/paper" is raised — nodes express relative importance, not an
// absolute count that inflates everyone at once.
function nodeRadius(n, maxDeg) {
  const t = maxDeg > 0 ? Math.min(1, (n.deg || 0) / maxDeg) : 0;
  return 4 + 5 * Math.sqrt(t);
}

// Map a publication year to a color on a cool→warm (old→new) scale.
// Returns null for undated papers (caller falls back to a neutral gray).
function yearColor(year, minYear, maxYear) {
  if (year == null || minYear == null || maxYear == null) return null;
  // Single year in the view → use the midpoint color (no gradient to show).
  const t = minYear === maxYear
    ? 0.5
    : Math.max(0, Math.min(1, (year - minYear) / (maxYear - minYear)));
  // Interpolate blue (#2563eb, oldest) → red (#dc2626, newest) in RGB.
  const r = Math.round(37 + t * (220 - 37));
  const g = Math.round(99 + t * (38 - 99));
  const b = Math.round(235 + t * (38 - 235));
  return 'rgb(' + r + ',' + g + ',' + b + ')';
}

// Neutral gray for papers with no publication date.
const UNDATED_COLOR = '#9ca3af';

function connectionsOf(id) {
  return (state.graph.edges || [])
    .filter((e) => e.source === id || e.target === id)
    .map((e) => ({ edge: e, other: nodeById.get(e.source === id ? e.target : e.source) }))
    .filter((c) => c.other)
    .sort((a, b) => b.edge.weight - a.edge.weight);
}

// ---- paper search (locate + highlight in the current view) ---------------

// Case-insensitive substring match over the nodes of the loaded graph.
// Title hits rank above venue/keyword hits; capped for the dropdown.
function graphMatches(q) {
  q = q.trim().toLowerCase();
  if (!q || !state.graph || !state.graph.nodes) return [];
  const titleHit = [], otherHit = [];
  state.graph.nodes.forEach((n) => {
    if ((n.title || '').toLowerCase().includes(q)) { titleHit.push(n); return; }
    const rest = ((n.venue || '') + ' ' + (n.keywords || []).join(' ')).toLowerCase();
    if (rest.includes(q)) otherHit.push(n);
  });
  return titleHit.concat(otherHit).slice(0, 8);
}

// Lazily fetch every indexed paper once so the search covers the whole
// library, not just the slice rendered in the current view.
function ensureLibrary() {
  if (libraryCache) return Promise.resolve(libraryCache);
  if (!libraryPromise) {
    libraryPromise = API.listPapers({ limit: 1000, status: 'indexed' }).then((r) => {
      libraryCache = ((r && r.papers) || []).map((it) => {
        const p = it.paper || it;
        const d = p.published_date || '';
        const year = /^\d{4}/.test(d) ? parseInt(d.slice(0, 4), 10) : null;
        return { id: p.id, title: p.title, venue: p.venue, year, keywords: p.keywords || [] };
      });
      return libraryCache;
    }).catch(() => {
      libraryPromise = null; // allow retry on the next keystroke
      return [];
    });
  }
  return libraryPromise;
}

// Library-wide matches that are NOT part of the current graph view.
function libraryMatches(q, cap) {
  q = q.trim().toLowerCase();
  if (!q || !libraryCache || cap <= 0) return [];
  const titleHit = [], otherHit = [];
  libraryCache.forEach((n) => {
    if (nodeById.has(n.id)) return;
    if ((n.title || '').toLowerCase().includes(q)) { titleHit.push(n); return; }
    const rest = ((n.venue || '') + ' ' + (n.keywords || []).join(' ')).toLowerCase();
    if (rest.includes(q)) otherHit.push(n);
  });
  return titleHit.concat(otherHit).slice(0, cap);
}

// Pin the paper, bring it into view, and pulse it so the eye finds it.
function locatePaper(id) {
  const n = nodeById.get(id);
  if (!n || !state.graph) return;
  // Timeline mode folds the least-connected cards behind "+N more"; if the
  // target is folded away, lift its lane's cap and re-render first.
  if (state.mode === 'timeline' && !(view && view.nodeEls.has(id))) {
    state.expanded.add(n.year == null ? 'n.d.' : String(n.year));
    buildView();
  }
  select(id); // pins the selection + dims everything else
  if (view && view.reveal) view.reveal(id);
  const el = view && view.nodeEls.get(id);
  if (el) {
    if (foundEl) foundEl.classList.remove('found');
    clearTimeout(foundTimer);
    foundEl = el;
    el.classList.add('found');
    foundTimer = setTimeout(() => {
      el.classList.remove('found');
      if (foundEl === el) foundEl = null;
    }, 2400);
  }
}

// ---- detail panel -------------------------------------------------------

function detailHTML() {
  const n = state.selected ? nodeById.get(state.selected) : null;
  if (!n) {
    const hint = state.mode === 'network'
      ? 'Drag to rearrange · scroll to zoom · click a paper to inspect it.'
      : 'Hover a card to trace links · click to pin.';
    return '<div class="graph-detail-empty">' + U.icon('network', 24) +
      '<p class="muted small">' + hint + '</p></div>';
  }
  const kw = (n.keywords || []).map((k) =>
    '<a class="chip" href="#/library?keyword=' + encodeURIComponent(k) + '">' + U.esc(k) + '</a>'
  ).join('');
  const conns = connectionsOf(n.id);
  const connHTML = conns.map((c) => {
    const shared = [].concat(c.edge.shared_keywords || [], c.edge.shared_entities || []);
    const chips = shared.slice(0, 6).map((s) => '<span class="mini-chip">' + U.esc(s) + '</span>').join('');
    return '<a class="graph-conn" href="#/paper/' + encodeURIComponent(c.other.id) + '">' +
      '<span class="graph-conn-title">' + U.esc(U.truncate(c.other.title || 'Untitled', 48)) + '</span>' +
      '<span class="graph-conn-meta">' + U.badge(U.scorePct(c.edge.weight) + '%', 'accent') + chips + '</span>' +
      '</a>';
  }).join('');
  const meta = [n.year, n.venue].filter(Boolean).map(U.esc).join(' · ');
  return '<div class="graph-detail-head">' +
    '<a class="graph-detail-title" href="#/paper/' + encodeURIComponent(n.id) + '">' +
    U.esc(n.title || 'Untitled') + '</a>' +
    (meta ? '<div class="muted small mt-1">' + meta + '</div>' : '') +
    '</div>' +
    (kw ? '<div class="graph-detail-sec"><div class="label">Keywords</div><div class="graph-chips">' + kw + '</div></div>' : '') +
    '<div class="graph-detail-sec"><div class="label">' + conns.length +
    (conns.length === 1 ? ' connection' : ' connections') + '</div>' +
    (connHTML || '<p class="muted small">No related papers in this view.</p>') + '</div>';
}

function select(id) {
  state.selected = id;
  const detail = document.getElementById('graph-detail');
  if (detail) detail.innerHTML = detailHTML();
  applyHighlight();
}

function applyHighlight() {
  if (!view) return;
  // Focus = pinned selection, else (timeline only) the hovered card.
  const focus = state.selected || (view.mode === 'timeline' ? hoverId : null);
  if (!focus) {
    view.nodeEls.forEach((el) => el.classList.remove('dim', 'sel', 'nbr', 'focal'));
    view.edgeEls.forEach((el) => el.classList.remove('dim', 'sel', 'on'));
    return;
  }
  const neighbors = new Set([focus]);
  (state.graph.edges || []).forEach((e) => {
    if (e.source === focus) neighbors.add(e.target);
    if (e.target === focus) neighbors.add(e.source);
  });
  view.nodeEls.forEach((el, id) => {
    el.classList.toggle('sel', id === state.selected);
    el.classList.toggle('focal', id === focus && id !== state.selected);
    el.classList.toggle('nbr', id !== focus && neighbors.has(id));
    el.classList.toggle('dim', !neighbors.has(id));
  });
  view.edgeEls.forEach((el) => {
    const on = el.getAttribute('data-s') === focus || el.getAttribute('data-t') === focus;
    if (view.mode === 'timeline') {
      // Timeline edges stay hidden unless they touch the focused card.
      el.classList.toggle('on', on);
    } else {
      el.classList.toggle('sel', on);
      el.classList.toggle('dim', !on);
    }
  });
}

// ---- network (force-directed) -------------------------------------------

function renderNetwork(canvas, graph) {
  cancelAnimationFrame(rafId);
  const W = canvas.clientWidth || 900;
  const H = canvas.clientHeight || 620;

  const nodes = graph.nodes.map((n, i) => {
    const a = (i / Math.max(1, graph.nodes.length)) * Math.PI * 2;
    const r = Math.min(W, H) * 0.33;
    return {
      ...n,
      x: W / 2 + r * Math.cos(a) + (Math.random() - 0.5) * 12,
      y: H / 2 + r * Math.sin(a) + (Math.random() - 0.5) * 12,
      vx: 0, vy: 0, deg: 0, fixed: false
    };
  });
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const edges = graph.edges
    .map((e) => ({ ...e, s: byId.get(e.source), t: byId.get(e.target) }))
    .filter((e) => e.s && e.t);
  edges.forEach((e) => { e.s.deg++; e.t.deg++; });
  const maxDeg = Math.max(1, ...nodes.map((n) => n.deg || 0));

  // Year range for the color scale (undated papers excluded from min/max).
  const datedYears = nodes.map((n) => n.year).filter((y) => y != null);
  const minYear = datedYears.length ? Math.min.apply(null, datedYears) : null;
  const maxYear = datedYears.length ? Math.max.apply(null, datedYears) : null;

  const edgeMarkup = edges.map((e) =>
    '<line class="g-edge" data-s="' + U.esc(e.source) + '" data-t="' + U.esc(e.target) +
    '" stroke-width="' + (0.6 + e.weight * 2.4).toFixed(2) + '"></line>'
  ).join('');
  const nodeMarkup = nodes.map((n) => {
    const c = yearColor(n.year, minYear, maxYear) || UNDATED_COLOR;
    return '<g class="g-node" data-id="' + U.esc(n.id) + '">' +
    '<circle class="g-dot" r="' + nodeRadius(n, maxDeg).toFixed(1) + '" fill="' + c + '"></circle>' +
    '<text class="g-label" dy="-' + (nodeRadius(n, maxDeg) + 4).toFixed(1) + '">' +
    U.esc(U.truncate(n.title || 'Untitled', 32)) + '</text></g>';
  }).join('');

  canvas.innerHTML =
    '<svg class="graph-svg" width="100%" height="100%">' +
    '<g class="g-zoom"><g class="g-edges">' + edgeMarkup + '</g>' +
    '<g class="g-nodes">' + nodeMarkup + '</g></g></svg>';

  // Color legend: shows the year gradient so colored dots are interpretable.
  // Only when there's an actual year span (a single year needs no gradient).
  if (minYear != null && minYear !== maxYear) {
    const legend = document.createElement('div');
    legend.className = 'graph-legend';
    legend.innerHTML =
      '<span class="graph-legend-label">' + minYear + '</span>' +
      '<span class="graph-legend-bar"></span>' +
      '<span class="graph-legend-label">' + maxYear + '</span>';
    canvas.appendChild(legend);
  }

  const svg = canvas.querySelector('svg');
  const zoomG = canvas.querySelector('.g-zoom');
  const nodeEls = new Map();
  canvas.querySelectorAll('.g-node').forEach((el) => nodeEls.set(el.getAttribute('data-id'), el));
  const edgeEls = Array.from(canvas.querySelectorAll('.g-edge'));
  view = { mode: 'network', byId, nodes, edges, nodeEls, edgeEls };

  const vt = { x: 0, y: 0, k: 1 };
  function applyView() {
    zoomG.setAttribute('transform', 'translate(' + vt.x + ',' + vt.y + ') scale(' + vt.k + ')');
  }
  // Fit the whole graph into the viewport with a margin. Called once the
  // force simulation settles, so the initial view is never "too big or too
  // small" — the user then fine-tunes from a sensible starting scale.
  function fitView() {
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    nodes.forEach((n) => {
      if (n.x < minX) minX = n.x;
      if (n.y < minY) minY = n.y;
      if (n.x > maxX) maxX = n.x;
      if (n.y > maxY) maxY = n.y;
    });
    const spanX = maxX - minX || 1;
    const spanY = maxY - minY || 1;
    const pad = 60;
    const k = Math.min((W - pad * 2) / spanX, (H - pad * 2) / spanY, 2);
    vt.k = Math.max(0.05, k);
    vt.x = W / 2 - (minX + maxX) / 2 * vt.k;
    vt.y = H / 2 - (minY + maxY) / 2 * vt.k;
    applyView();
  }
  // Center the view on a node (graph-search locate) without changing zoom.
  view.reveal = (id) => {
    const n = byId.get(id);
    if (!n) return;
    vt.x = W / 2 - n.x * vt.k;
    vt.y = H / 2 - n.y * vt.k;
    applyView();
  };
  function updatePositions() {
    edgeEls.forEach((el) => {
      const s = byId.get(el.getAttribute('data-s'));
      const t = byId.get(el.getAttribute('data-t'));
      if (!s || !t) return;
      el.setAttribute('x1', s.x.toFixed(1));
      el.setAttribute('y1', s.y.toFixed(1));
      el.setAttribute('x2', t.x.toFixed(1));
      el.setAttribute('y2', t.y.toFixed(1));
    });
    nodeEls.forEach((el, id) => {
      const n = byId.get(id);
      el.setAttribute('transform', 'translate(' + n.x.toFixed(1) + ',' + n.y.toFixed(1) + ')');
    });
  }

  // Force simulation: pairwise repulsion + spring attraction along edges +
  // gentle centering gravity, cooled by a decaying alpha.
  let alpha = 1;
  const repulsion = 5200, spring = 0.03, restLen = 82, gravity = 0.015, damping = 0.86;
  function tick() {
    for (let i = 0; i < nodes.length; i++) {
      const a = nodes[i];
      for (let j = i + 1; j < nodes.length; j++) {
        const b = nodes[j];
        let dx = a.x - b.x, dy = a.y - b.y;
        let d2 = dx * dx + dy * dy;
        if (d2 < 0.01) { d2 = 0.01; dx = Math.random() - 0.5; dy = Math.random() - 0.5; }
        const d = Math.sqrt(d2);
        const f = repulsion / d2;
        const fx = (dx / d) * f, fy = (dy / d) * f;
        a.vx += fx; a.vy += fy;
        b.vx -= fx; b.vy -= fy;
      }
    }
    for (const e of edges) {
      const dx = e.t.x - e.s.x, dy = e.t.y - e.s.y;
      const d = Math.sqrt(dx * dx + dy * dy) || 0.01;
      const f = (d - restLen) * spring * (0.4 + e.weight);
      const fx = (dx / d) * f, fy = (dy / d) * f;
      e.s.vx += fx; e.s.vy += fy;
      e.t.vx -= fx; e.t.vy -= fy;
    }
    for (const n of nodes) {
      if (n.fixed) { n.vx = 0; n.vy = 0; continue; }
      n.vx += (W / 2 - n.x) * gravity;
      n.vy += (H / 2 - n.y) * gravity;
      n.vx *= damping; n.vy *= damping;
      n.x += n.vx * alpha;
      n.y += n.vy * alpha;
    }
    alpha *= 0.994;
  }

  // Settle the layout synchronously BEFORE the first paint. Rendering every
  // tick via requestAnimationFrame would show ~650 frames of jitter — about
  // 10s of shaking at 60fps for 521 nodes. The full simulation only costs
  // ~250ms (521 nodes) or ~900ms ("All", 1000 nodes), so the "Building
  // graph…" spinner covers it and the graph appears already settled.
  while (alpha > 0.02) tick();
  updatePositions();
  fitView();

  // Only interactive drags re-activate the animation, so nudging a node
  // around pulls its neighbors with it.
  function frame() {
    if (alpha > 0.02) {
      tick();
      updatePositions();
      rafId = requestAnimationFrame(frame);
    } else {
      updatePositions();
    }
  }

  // ---- interaction: drag nodes, pan background, wheel zoom, click select ----
  function toGraph(clientX, clientY) {
    const rect = svg.getBoundingClientRect();
    const sx = clientX - rect.left, sy = clientY - rect.top;
    return { x: (sx - vt.x) / vt.k, y: (sy - vt.y) / vt.k };
  }

  let drag = null;
  svg.addEventListener('pointerdown', (e) => {
    const nodeEl = e.target.closest('.g-node');
    if (nodeEl) {
      const n = byId.get(nodeEl.getAttribute('data-id'));
      const p = toGraph(e.clientX, e.clientY);
      drag = { node: n, moved: 0, lx: p.x, ly: p.y };
      n.fixed = true;
      alpha = Math.max(alpha, 0.5);
      rafId = requestAnimationFrame(frame);
    } else {
      drag = { pan: true, moved: 0, lx: e.clientX, ly: e.clientY };
    }
    try { svg.setPointerCapture(e.pointerId); } catch (err) { /* ignore */ }
  });
  svg.addEventListener('pointermove', (e) => {
    if (!drag) return;
    if (drag.node) {
      const p = toGraph(e.clientX, e.clientY);
      drag.moved += Math.abs(p.x - drag.lx) + Math.abs(p.y - drag.ly);
      drag.node.x = p.x; drag.node.y = p.y;
      drag.lx = p.x; drag.ly = p.y;
      alpha = Math.max(alpha, 0.3);
      updatePositions();
    } else if (drag.pan) {
      drag.moved += Math.abs(e.clientX - drag.lx) + Math.abs(e.clientY - drag.ly);
      vt.x += e.clientX - drag.lx;
      vt.y += e.clientY - drag.ly;
      drag.lx = e.clientX; drag.ly = e.clientY;
      applyView();
    }
  });
  function endDrag(e) {
    if (!drag) return;
    const wasClick = drag.moved < 4;
    if (drag.node) {
      drag.node.fixed = false;
      if (wasClick) select(drag.node.id === state.selected ? null : drag.node.id);
    } else if (drag.pan && wasClick) {
      select(null);
    }
    drag = null;
    try { svg.releasePointerCapture(e.pointerId); } catch (err) { /* ignore */ }
  }
  svg.addEventListener('pointerup', endDrag);
  svg.addEventListener('pointercancel', endDrag);

  svg.addEventListener('wheel', (e) => {
    e.preventDefault();
    const rect = svg.getBoundingClientRect();
    const sx = e.clientX - rect.left, sy = e.clientY - rect.top;
    // Scale the factor by the actual delta so trackpads (many small deltas)
    // and mouse wheels (few large ones) zoom at a comparable rate.
    const delta = (e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY) || 0;
    const factor = Math.exp(-delta * 0.0012);
    const nk = Math.max(0.03, Math.min(8, vt.k * factor));
    vt.x = sx - (sx - vt.x) * (nk / vt.k);
    vt.y = sy - (sy - vt.y) * (nk / vt.k);
    vt.k = nk;
    applyView();
  }, { passive: false });

  applyHighlight();
}

// ---- timeline (papers by publication year) ------------------------------
//
// Design: one lane per year on a horizontal time axis; papers without a date
// go to a trailing "n.d." lane. Cards rank by link count inside their lane
// (most connected = likely foundational). All edges stay hidden until a card
// is hovered or pinned — the full edge set is spaghetti at library scale, so
// links are disclosed progressively instead of drawn all at once.

const TL = { laneW: 232, cardW: 208, cardH: 58, cardGap: 10, padX: 32, headH: 60, padBottom: 28 };
// Max cards shown per lane before a "+N more" expander. Dense years (a
// current-year lane can hold 70+ papers) would otherwise stretch the canvas
// to several screenfuls; the least-connected cards fold away first.
const TL_CAP = 16;

// Cubic bezier between two cards, oriented from the earlier-year card to the
// later-year one (edges are undirected). Same-lane links bow out to the right.
function tlEdgePath(a, b) {
  if (a.col === b.col) {
    const x = a.x + TL.cardW, y1 = a.y + TL.cardH / 2, y2 = b.y + TL.cardH / 2;
    return 'M' + x + ',' + y1 + ' C' + (x + 46) + ',' + y1 + ' ' + (x + 46) + ',' + y2 + ' ' + x + ',' + y2;
  }
  const from = a.col < b.col ? a : b, to = a.col < b.col ? b : a;
  const x1 = from.x + TL.cardW, y1 = from.y + TL.cardH / 2;
  const x2 = to.x, y2 = to.y + TL.cardH / 2;
  const dx = Math.max(42, (x2 - x1) * 0.45);
  return 'M' + x1 + ',' + y1 + ' C' + (x1 + dx) + ',' + y1 + ' ' + (x2 - dx) + ',' + y2 + ' ' + x2 + ',' + y2;
}

function renderTimeline(canvas, graph) {
  cancelAnimationFrame(rafId);
  hoverId = null;
  const byId = new Map(graph.nodes.map((n) => [n.id, n]));

  // Link count per paper: drives the ranking inside a lane + the subtitle.
  const linkCount = new Map();
  (graph.edges || []).forEach((e) => {
    linkCount.set(e.source, (linkCount.get(e.source) || 0) + 1);
    linkCount.set(e.target, (linkCount.get(e.target) || 0) + 1);
  });

  const dated = graph.nodes.filter((n) => n.year != null);
  const undated = graph.nodes.filter((n) => n.year == null);
  const laneSort = (a, b) =>
    (linkCount.get(b.id) || 0) - (linkCount.get(a.id) || 0) ||
    (a.title || '').localeCompare(b.title || '');
  const years = Array.from(new Set(dated.map((n) => n.year))).sort((a, b) => a - b);
  const lanes = years.map((y) => ({
    label: String(y), year: y,
    cards: dated.filter((n) => n.year === y).sort(laneSort),
  }));
  if (undated.length) lanes.push({ label: 'n.d.', year: null, cards: undated.sort(laneSort) });

  const datedYears = dated.map((n) => n.year);
  const minYear = datedYears.length ? Math.min.apply(null, datedYears) : null;
  const maxYear = datedYears.length ? Math.max.apply(null, datedYears) : null;

  // Apply the per-lane cap unless the lane was expanded by the user.
  const laneView = lanes.map((lane, ci) => {
    const open = state.expanded.has(lane.label);
    const shown = open ? lane.cards : lane.cards.slice(0, TL_CAP);
    return {
      label: lane.label, year: lane.year, total: lane.cards.length,
      shown, hidden: lane.cards.length - shown.length,
      x: TL.padX + ci * TL.laneW, col: ci,
    };
  });

  const maxCol = Math.max.apply(null, laneView.map((l) => l.shown.length + (l.hidden ? 1 : 0)));
  const W = TL.padX * 2 + lanes.length * TL.laneW;
  const H = TL.headH + maxCol * (TL.cardH + TL.cardGap) + TL.padBottom;
  const axisY = TL.headH - 12;

  // Card top-left positions (+ lane column for edge anchoring). Folded-away
  // cards get no position, and edges pointing at them are skipped below.
  const pos = new Map();
  laneView.forEach((lane) => {
    const x = lane.x + (TL.laneW - TL.cardW) / 2;
    lane.shown.forEach((n, ri) =>
      pos.set(n.id, { x, y: TL.headH + ri * (TL.cardH + TL.cardGap), col: lane.col }));
  });

  // Alternating lane bands so a column stays trackable when scrolled.
  const bandMarkup = lanes.map((lane, ci) => (ci % 2 === 1
    ? '<rect class="g-lane" x="' + (TL.padX + ci * TL.laneW) + '" y="8" width="' +
      TL.laneW + '" height="' + (H - 16) + '" rx="12"></rect>'
    : '')).join('');

  let axisMarkup = '<line class="g-axis" x1="' + TL.padX + '" y1="' + axisY +
    '" x2="' + (W - TL.padX) + '" y2="' + axisY + '"></line>';
  laneView.forEach((lane, ci) => {
    const cx = lane.x + TL.laneW / 2;
    axisMarkup +=
      '<line class="g-tick" x1="' + cx + '" y1="' + (axisY - 4) + '" x2="' + cx + '" y2="' + (axisY + 4) + '"></line>' +
      '<text class="g-year" x="' + cx + '" y="24">' + lane.label + '</text>' +
      '<text class="g-year-count" x="' + cx + '" y="40">' + lane.total +
      (lane.total === 1 ? ' paper' : ' papers') + '</text>';
    // Temporal-honesty hint: mark multi-year gaps between adjacent lanes.
    const next = laneView[ci + 1];
    if (next && lane.year != null && next.year != null && next.year - lane.year > 1) {
      axisMarkup += '<text class="g-gap" x="' + (cx + TL.laneW / 2) + '" y="' + (axisY - 7) + '">· ' +
        (next.year - lane.year - 1) + 'y ·</text>';
    }
  });

  // Full edge set, pre-rendered but hidden (see CSS); revealed per focus card.
  const edges = (graph.edges || []).filter((e) => pos.has(e.source) && pos.has(e.target));
  const edgeMarkup = edges.map((e) =>
    '<path class="g-edge" data-s="' + U.esc(e.source) + '" data-t="' + U.esc(e.target) +
    '" d="' + tlEdgePath(pos.get(e.source), pos.get(e.target)) +
    '" stroke-width="' + (1 + e.weight * 2.2).toFixed(2) + '"></path>').join('');

  const cardMarkup = laneView.map((lane) => lane.shown.map((n) => {
    const p = pos.get(n.id);
    const links = linkCount.get(n.id) || 0;
    const sub = n.venue || (n.keywords || [])[0] || '—';
    const dot = yearColor(n.year, minYear, maxYear) || UNDATED_COLOR;
    return '<g class="g-node g-card" data-id="' + U.esc(n.id) + '" transform="translate(' + p.x + ',' + p.y + ')">' +
      '<rect class="g-card-bg" width="' + TL.cardW + '" height="' + TL.cardH + '" rx="10"></rect>' +
      '<circle class="g-card-dot" cx="17" cy="20" r="3.5" fill="' + dot + '"></circle>' +
      '<text class="g-card-title" x="29" y="24">' + U.esc(U.truncate(n.title || 'Untitled', 28)) + '</text>' +
      '<text class="g-card-sub" x="29" y="43">' + U.esc(U.truncate(sub, 20)) +
      (links ? ' · ' + links + (links === 1 ? ' link' : ' links') : '') + '</text></g>';
  }).join('')).join('');

  // Folded-card expanders: dashed placeholder at the bottom of capped lanes.
  const moreMarkup = laneView.map((lane) => {
    if (!lane.hidden) return '';
    const x = lane.x + (TL.laneW - TL.cardW) / 2;
    const y = TL.headH + lane.shown.length * (TL.cardH + TL.cardGap);
    return '<g class="g-more" data-lane="' + U.esc(lane.label) + '" transform="translate(' + x + ',' + y + ')">' +
      '<rect class="g-more-bg" width="' + TL.cardW + '" height="' + TL.cardH + '" rx="10"></rect>' +
      '<text class="g-more-text" x="' + (TL.cardW / 2) + '" y="' + (TL.cardH / 2 + 4) + '">+ ' +
      lane.hidden + ' more</text></g>';
  }).join('');

  canvas.innerHTML =
    '<div class="graph-scroll"><svg class="graph-svg timeline-svg" width="' + W + '" height="' + H + '">' +
    '<g class="g-lanes">' + bandMarkup + '</g>' +
    '<g class="g-axis-g">' + axisMarkup + '</g>' +
    '<g class="g-edges">' + edgeMarkup + '</g>' +
    '<g class="g-nodes">' + cardMarkup + moreMarkup + '</g></svg></div>';

  const nodeEls = new Map();
  canvas.querySelectorAll('.g-node').forEach((el) => nodeEls.set(el.getAttribute('data-id'), el));
  const edgeEls = Array.from(canvas.querySelectorAll('.g-edge'));
  view = { mode: 'timeline', byId, nodes: graph.nodes, edges, nodeEls, edgeEls };
  // Scroll the card into the visible viewport (graph-search locate).
  view.reveal = (id) => {
    const el = nodeEls.get(id);
    if (el) el.scrollIntoView({ block: 'center', inline: 'center' });
  };

  const svg = canvas.querySelector('svg');
  svg.addEventListener('click', (e) => {
    if (e.target.closest('.g-more')) return; // handled below
    if (!e.target.closest('.g-card')) select(null);
  });
  nodeEls.forEach((el, id) => {
    el.addEventListener('pointerenter', () => { hoverId = id; applyHighlight(); });
    el.addEventListener('pointerleave', () => { hoverId = null; applyHighlight(); });
    el.addEventListener('click', () => select(id === state.selected ? null : id));
  });
  canvas.querySelectorAll('.g-more').forEach((el) => {
    el.addEventListener('click', () => {
      state.expanded.add(el.getAttribute('data-lane'));
      buildView(); // re-render; buildView restores the scroll position
    });
  });

  applyHighlight();
}

// ---- controls + data loading --------------------------------------------

function controlsHTML() {
  const modeBtn = (id, label) =>
    '<button type="button" data-mode="' + id + '"' +
    (state.mode === id ? ' class="active" aria-pressed="true"' : ' aria-pressed="false"') +
    '>' + label + '</button>';
  const limitOpts = [50, 100, 200, 500, 'all'].map((n) => {
    const v = n === 'all' ? GRAPH_ALL_LIMIT : n;
    const label = n === 'all' ? 'All' : String(n);
    return '<option value="' + v + '"' + (state.limit === v ? ' selected' : '') + '>' + label + '</option>';
  }).join('');
  const degreeOpts = [2, 3, 4, 5, 8, 12, 16, 20].map((n) =>
    '<option value="' + n + '"' + (state.degree === n ? ' selected' : '') + '>' + n + '</option>'
  ).join('');
  return '<div class="card"><div class="toolbar">' +
    '<div class="segmented" id="graph-mode" role="group" aria-label="View mode">' +
    modeBtn('network', 'Network') + modeBtn('timeline', 'Timeline') + '</div>' +
    '<label class="muted small nowrap" for="graph-limit">Papers</label>' +
    '<select class="select" id="graph-limit">' + limitOpts + '</select>' +
    '<label class="muted small nowrap" for="graph-degree">Links/paper</label>' +
    '<select class="select" id="graph-degree">' + degreeOpts + '</select>' +
    '<div class="graph-search">' +
    '<input type="search" class="input" id="graph-search-input" placeholder="Find a paper…" ' +
    'autocomplete="off" aria-label="Find a paper in the graph">' +
    '<div class="graph-search-pop" id="graph-search-pop" hidden></div>' +
    '</div>' +
    '<span class="spacer"></span>' +
    '<span class="muted small" id="graph-stats"></span>' +
    '<button type="button" class="btn ghost sm" id="graph-refresh" title="Rebuild graph">' +
    U.icon('refresh', 14) + '</button>' +
    '</div></div>';
}

function buildView() {
  const canvas = document.getElementById('graph-canvas');
  const detail = document.getElementById('graph-detail');
  if (!canvas || !state.graph) return;
  if (!state.graph.nodes || !state.graph.nodes.length) {
    canvas.innerHTML = U.emptyState({
      icon: 'network', title: 'No papers yet',
      body: 'Index some papers to build the relatedness graph.'
    });
    if (detail) detail.innerHTML = '';
    view = null;
    return;
  }
  if (detail) detail.innerHTML = detailHTML();
  // Preserve the timeline scroll position across re-renders (lane expanders).
  const sc = canvas.querySelector('.graph-scroll');
  const scroll = sc ? { l: sc.scrollLeft, t: sc.scrollTop } : null;
  if (state.mode === 'network') renderNetwork(canvas, state.graph);
  else renderTimeline(canvas, state.graph);
  if (scroll) {
    const sc2 = canvas.querySelector('.graph-scroll');
    if (sc2) { sc2.scrollLeft = scroll.l; sc2.scrollTop = scroll.t; }
  }
}

function load(focusId) {
  const myGen = gen;
  const canvas = document.getElementById('graph-canvas');
  if (canvas) canvas.innerHTML = U.spinner('Building graph…');
  API.graph({ limit: state.limit, max_edges_per_node: state.degree, focus: focusId || undefined }).then((g) => {
    if (myGen !== gen) return;
    state.graph = g;
    state.selected = null;
    state.expanded = new Set();
    nodeById = new Map((g.nodes || []).map((n) => [n.id, n]));
    const stats = document.getElementById('graph-stats');
    if (stats) {
      stats.textContent = U.fmtInt((g.nodes || []).length) + ' papers · ' +
        U.fmtInt((g.edges || []).length) + ' links';
    }
    buildView();
    // A library-search pick outside the current view lands here: the backend
    // force-includes it (with its strongest neighbors) so we can locate it.
    if (focusId) {
      if (nodeById.has(focusId)) locatePaper(focusId);
      else U.toast('Paper not found in the indexed library', 'error');
    }
  }).catch((err) => {
    if (myGen !== gen) return;
    if (canvas) canvas.innerHTML = U.errorState(err.message, 'Retry');
    const retry = canvas && canvas.querySelector('[data-retry]');
    if (retry) retry.addEventListener('click', () => load());
  });
}

function wireControls(container) {
  container.querySelectorAll('#graph-mode button').forEach((b) => {
    b.addEventListener('click', () => {
      const m = b.getAttribute('data-mode');
      if (m === state.mode) return;
      state.mode = m;
      container.querySelectorAll('#graph-mode button').forEach((x) => {
        const on = x === b;
        x.classList.toggle('active', on);
        x.setAttribute('aria-pressed', String(on));
      });
      buildView();
    });
  });
  const limit = container.querySelector('#graph-limit');
  limit.addEventListener('change', () => { state.limit = +limit.value; load(); });
  const degree = container.querySelector('#graph-degree');
  degree.addEventListener('change', () => { state.degree = +degree.value; load(); });
  container.querySelector('#graph-refresh').addEventListener('click', () => load());

  // ---- paper search: match against the loaded graph AND the whole library ----
  const searchInput = container.querySelector('#graph-search-input');
  const searchPop = container.querySelector('#graph-search-pop');
  let matches = []; // [{ n, inView }]
  let activeIdx = -1;
  let searchSeq = 0; // stale async library repaints bail out

  function closePop() { searchPop.hidden = true; activeIdx = -1; }

  function itemHTML(n, i, inView) {
    const bits = [n.year, n.venue].filter(Boolean).map(U.esc);
    if (!inView) bits.push('not in view');
    const meta = bits.join(' · ');
    return '<button type="button" class="graph-search-item' + (i === activeIdx ? ' active' : '') +
      '" data-id="' + U.esc(n.id) + '">' +
      '<span class="graph-search-item-title">' + U.esc(U.truncate(n.title || 'Untitled', 60)) + '</span>' +
      (meta ? '<span class="graph-search-item-meta">' + meta + '</span>' : '') +
      '</button>';
  }

  function renderPop(q, withLibrary) {
    const inView = graphMatches(q);
    const outside = withLibrary ? libraryMatches(q, 8 - inView.length) : [];
    matches = inView.map((n) => ({ n, inView: true }))
      .concat(outside.map((n) => ({ n, inView: false })));
    if (!matches.length) {
      searchPop.innerHTML = '<div class="graph-search-empty">' +
        (withLibrary ? 'No matching paper in the library.' : 'Searching the library…') + '</div>';
    } else {
      searchPop.innerHTML = matches.map((m, i) => itemHTML(m.n, i, m.inView)).join('');
      searchPop.querySelectorAll('.graph-search-item').forEach((b, i) => {
        b.addEventListener('click', () => pick(matches[i]));
      });
    }
    searchPop.hidden = false;
  }

  function paintPop() {
    const q = searchInput.value.trim();
    if (!q) { closePop(); return; }
    const seq = ++searchSeq;
    // In-view matches paint instantly; the library-wide pass repaints when
    // the (cached) full paper list arrives.
    renderPop(q, false);
    ensureLibrary().then(() => {
      if (seq !== searchSeq || searchInput.value.trim() !== q) return;
      renderPop(q, true);
    });
  }

  function pick(m) {
    closePop();
    if (!m) return;
    // In the current view: locate directly. Outside it: reload the graph with
    // the backend focus expansion (paper + its strongest neighbors).
    if (m.inView && nodeById.has(m.n.id)) locatePaper(m.n.id);
    else load(m.n.id);
  }

  searchInput.addEventListener('input', () => { activeIdx = -1; paintPop(); });
  searchInput.addEventListener('focus', () => { if (searchInput.value.trim()) paintPop(); });
  // Delayed so a click on a dropdown item lands before the pop closes.
  searchInput.addEventListener('blur', () => setTimeout(closePop, 150));
  searchInput.addEventListener('keydown', (e) => {
    if (searchPop.hidden) return;
    if (e.key === 'Escape') {
      closePop();
      searchInput.blur();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (!matches.length) return;
      activeIdx = (activeIdx + (e.key === 'ArrowDown' ? 1 : -1) + matches.length) % matches.length;
      paintPop();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      pick(matches[activeIdx >= 0 ? activeIdx : 0]);
    }
  });
}

// ---- route --------------------------------------------------------------

route('/graph', {
  title: 'Graph',
  wide: true,
  render(container) {
    gen++;
    container.innerHTML =
      '<div class="page-head"><p class="page-sub">Relatedness network built from shared keywords and biological entities</p></div>' +
      controlsHTML() +
      '<div class="graph-wrap mt-4">' +
      '<div class="graph-canvas" id="graph-canvas">' + U.spinner('Building graph…') + '</div>' +
      '<aside class="graph-detail" id="graph-detail"></aside>' +
      '</div>';
    wireControls(container);
    load();
    return function destroy() {
      gen++;
      cancelAnimationFrame(rafId);
      view = null;
    };
  }
});
