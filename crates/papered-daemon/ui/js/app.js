// App entry: shell wiring (sidebar toggle, appearance popover, icons),
// dashboard view, and the boot sequence (setup wizard gate).
// View modules self-register their routes on import; the router starts last.

import * as U from './util.js?v=1';
import { API } from './api.js?v=1';
import { route, navigate, start, render } from './router.js?v=1';

// Side-effect imports: each view module registers its routes on load.
import './library.js?v=1';
import './search.js?v=1';
import './graph.js?v=1';
import './ask.js?v=1';
import './prompts.js?v=1';
import './sync.js?v=1';
import './health.js?v=1';
import './settings.js?v=1';
import { renderWizard } from './wizard.js?v=1';

// ---- appearance popover -------------------------------------------------

function initAppearance() {
  const btn = document.getElementById('appearance-btn');
  const pop = document.getElementById('appearance-popover');
  btn.addEventListener('click', (e) => {
    e.stopPropagation();
    const open = pop.classList.toggle('hidden') === false;
    btn.setAttribute('aria-expanded', String(open));
  });
  document.addEventListener('click', (e) => {
    if (!pop.contains(e.target)) {
      pop.classList.add('hidden');
      btn.setAttribute('aria-expanded', 'false');
    }
  });

  function syncPressed() {
    const theme = localStorage.getItem('papered.theme') || 'auto';
    const font = localStorage.getItem('papered.reading-font') === 'sans' ? 'sans' : 'serif';
    pop.querySelectorAll('[data-theme-choice]').forEach((b) => {
      const on = b.getAttribute('data-theme-choice') === theme;
      b.classList.toggle('active', on);
      b.setAttribute('aria-pressed', String(on));
    });
    pop.querySelectorAll('[data-font-choice]').forEach((b) => {
      const on = b.getAttribute('data-font-choice') === font;
      b.classList.toggle('active', on);
      b.setAttribute('aria-pressed', String(on));
    });
  }
  pop.querySelectorAll('[data-theme-choice]').forEach((b) => {
    b.addEventListener('click', () => {
      const v = b.getAttribute('data-theme-choice');
      if (v === 'auto') {
        localStorage.removeItem('papered.theme');
        document.documentElement.removeAttribute('data-theme');
      } else {
        localStorage.setItem('papered.theme', v);
        document.documentElement.setAttribute('data-theme', v);
      }
      syncPressed();
    });
  });
  pop.querySelectorAll('[data-font-choice]').forEach((b) => {
    b.addEventListener('click', () => {
      const v = b.getAttribute('data-font-choice');
      if (v === 'sans') {
        localStorage.setItem('papered.reading-font', 'sans');
        document.documentElement.setAttribute('data-font', 'sans');
      } else {
        localStorage.removeItem('papered.reading-font');
        document.documentElement.removeAttribute('data-font');
      }
      syncPressed();
    });
  });
  syncPressed();
}

// ---- icons in the static shell ------------------------------------------

function injectShellIcons() {
  document.querySelectorAll('[data-icon]').forEach((el) => {
    el.innerHTML = U.icon(el.getAttribute('data-icon'), 17);
    el.classList.add('row-sm');
  });
}

// ---- dashboard ----------------------------------------------------------

const MCP_CLIENTS = {
  opencode: {
    label: 'opencode',
    snippet: (origin) => JSON.stringify({
      mcp: { papered: { type: 'remote', url: origin + '/mcp/v1/messages', enabled: true } }
    }, null, 2)
  },
  hermes: {
    label: 'hermes',
    snippet: (origin) => 'mcp:\n  papered:\n    type: streamable_http\n    url: ' + origin +
      '/mcp/v1/messages\n    enabled: true'
  },
  claude: {
    label: 'Claude Code',
    snippet: (origin) => 'claude mcp add --transport http papered ' + origin + '/mcp/v1/messages'
  }
};

function mcpCardHTML() {
  const options = Object.keys(MCP_CLIENTS)
    .map((k) => '<option value="' + k + '">' + U.esc(MCP_CLIENTS[k].label) + '</option>')
    .join('');
  return '<div class="card"><div class="card-head"><span class="card-title-icon">' + U.icon('sparkles', 17) +
    '</span><h3>Connect your AI agent</h3><span class="spacer"></span>' +
    '<label class="visually-hidden" for="mcp-client">MCP client</label>' +
    '<select class="select" id="mcp-client">' + options + '</select></div>' +
    '<p class="muted small">Add Papered as an MCP server, then ask questions over your library from your agent.</p>' +
    '<div class="snippet-toolbar"><button type="button" id="mcp-copy" class="btn ghost sm">' + U.icon('copy', 14) + ' Copy</button></div>' +
    '<pre class="snippet mono pre-wrap" id="mcp-snippet"></pre></div>';
}

function wireMcpCard(container) {
  const select = container.querySelector('#mcp-client');
  const pre = container.querySelector('#mcp-snippet');
  const copyBtn = container.querySelector('#mcp-copy');
  if (!select || !pre) return;
  function update() {
    pre.textContent = MCP_CLIENTS[select.value].snippet(location.origin);
  }
  select.addEventListener('change', update);
  copyBtn.addEventListener('click', () => U.copy(pre.textContent, 'Config'));
  update();
}

// The add-paper form markup, shared by the standalone card and the
// empty-library hero. Ids are unique because only one variant renders.
function addPaperFormHTML() {
  return '<div class="kv-row"><label class="visually-hidden" for="add-path">Paper file path</label>' +
    '<input class="input" id="add-path" placeholder="/path/to/paper.pdf — pdf, md, txt, tex, docx, images">' +
    '<button type="button" class="btn primary" id="add-btn">Add</button></div>' +
    '<hr class="divider">' +
    '<div class="dropzone" id="add-drop">' + U.icon('upload', 20) +
    '<div class="mt-1">Drop a file here or <button type="button" class="btn subtle sm" id="add-upload">browse</button> to pick files</div></div>' +
    '<input type="file" id="add-file" class="hidden" accept=".pdf,.md,.txt,.tex,.docx,.png,.jpg,.jpeg,.webp,.gif,.bmp" aria-label="Upload a paper file">' +
    '<div id="add-msg" class="field-hint mt-2" role="status"></div>';
}

function addPaperCardHTML() {
  return '<div class="card"><div class="card-head"><span class="card-title-icon">' + U.icon('plus', 17) +
    '</span><h3>Add a paper</h3></div>' + addPaperFormHTML() + '</div>';
}

function heroCardHTML() {
  return '<div class="card">' +
    '<div class="card-head"><span class="card-title-icon">' + U.icon('sparkles', 17) +
    '</span><h3>Welcome — your library is empty</h3></div>' +
    '<p class="muted">Add your first paper below, by path or file upload. ' +
    'Once indexed, you can search your library and chat with it from Ask. ' +
    'You can also <a href="#/sync">sync an existing Zotero / Lattice library</a>.</p>' +
    addPaperFormHTML() + '</div>';
}

function wireAddPaperCard(container, onQueued) {
  const input = container.querySelector('#add-path');
  const btn = container.querySelector('#add-btn');
  const msg = container.querySelector('#add-msg');
  const drop = container.querySelector('#add-drop');
  const fileInput = container.querySelector('#add-file');
  const uploadBtn = container.querySelector('#add-upload');
  if (!input) return;

  function note(text, isErr) {
    msg.textContent = text;
    msg.classList.toggle('text-err', !!isErr);
    msg.classList.toggle('text-ok', !isErr);
  }
  function addByPath() {
    const path = input.value.trim();
    if (!path) return;
    btn.disabled = true;
    API.addPaper(path).then((paper) => {
      note('Queued for indexing: ' + (paper.title || path));
      input.value = '';
      U.toast('Paper queued for indexing', 'success');
      if (onQueued) onQueued();
    }).catch((e) => {
      note(e.message, true);
    }).finally(() => { btn.disabled = false; });
  }
  function upload(file) {
    if (!file) return;
    note('Uploading ' + file.name + '…');
    API.importPaper(file).then((res) => {
      note('Queued for indexing: ' + (res.title || file.name));
      U.toast('Upload queued for indexing', 'success');
      if (onQueued) onQueued();
    }).catch((e) => note(e.message, true));
  }
  btn.addEventListener('click', addByPath);
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') addByPath(); });
  uploadBtn.addEventListener('click', async () => {
    let paths;
    try {
      const res = await API.pickFile();
      paths = res.paths || [];
    } catch (e) {
      // The dialog itself failed — tell the user and offer the manual route.
      note(e.message + ' You can still type the file path above.', true);
      U.toast('File picker unavailable — enter the path manually', 'error');
      return;
    }
    if (!paths.length) return; // user cancelled the dialog — stay quiet
    note('Adding ' + paths.length + ' file(s)…');
    try {
      const batchRes = await API.batchAdd(paths);
      const items = batchRes.results || [];
      const failed = items.filter((r) => r.error);
      const queued = batchRes.queued != null ? batchRes.queued : items.length - failed.length;
      if (failed.length) {
        note('Queued ' + queued + ' · failed: ' +
          failed.map((f) => (f.file_path || f.title || '?') + ' (' + f.error + ')').join('; '), true);
      } else {
        note('Queued ' + queued + ' file(s) for indexing');
      }
      U.toast('Queued ' + queued + (queued === 1 ? ' paper' : ' papers') +
        (failed.length ? ' · ' + failed.length + ' failed' : ''), failed.length ? 'info' : 'success');
      if (onQueued) onQueued();
    } catch (e) {
      note(e.message, true);
    }
  });
  fileInput.addEventListener('change', () => { upload(fileInput.files[0]); fileInput.value = ''; });
  ['dragover', 'dragenter'].forEach((ev) => {
    drop.addEventListener(ev, (e) => { e.preventDefault(); drop.classList.add('dragover'); });
  });
  ['dragleave', 'drop'].forEach((ev) => {
    drop.addEventListener(ev, (e) => { e.preventDefault(); drop.classList.remove('dragover'); });
  });
  drop.addEventListener('drop', (e) => {
    if (e.dataTransfer.files && e.dataTransfer.files[0]) upload(e.dataTransfer.files[0]);
  });
}

function aggregateMetrics(groups) {
  const t = { calls: 0, failures: 0, prompt_tokens: 0, completion_tokens: 0, latencySum: 0, latencyN: 0 };
  (groups || []).forEach((g) => {
    t.calls += g.calls || 0;
    t.failures += g.failures || 0;
    t.prompt_tokens += g.prompt_tokens || 0;
    t.completion_tokens += g.completion_tokens || 0;
    if (g.avg_latency_ms && g.calls) {
      t.latencySum += g.avg_latency_ms * g.calls;
      t.latencyN += g.calls;
    }
  });
  t.avgLatency = t.latencyN ? t.latencySum / t.latencyN : 0;
  return t;
}

function metricsCardHTML() {
  return '<div class="card"><div class="card-head"><span class="card-title-icon">' + U.icon('list', 17) +
    '</span><h3>LLM usage</h3><span class="spacer"></span>' +
    '<div class="segmented" id="metrics-range" role="group" aria-label="Metrics range">' +
    '<button type="button" data-range="last_24h" class="active" aria-pressed="true">24h</button>' +
    '<button type="button" data-range="all_time" aria-pressed="false">All time</button></div></div>' +
    '<div id="metrics-body">' + U.spinner() + '</div></div>';
}

function renderMetricsBody(body, groups) {
  if (!groups || !groups.length) {
    body.innerHTML = '<p class="muted small center pad-block">No LLM calls recorded yet.</p>';
    return;
  }
  const t = aggregateMetrics(groups);
  const rows = groups.map((g) =>
    '<tr><td>' + U.esc(g.kind) + '</td><td class="mono">' + U.esc(g.model) + '</td>' +
    '<td class="nowrap">' + U.fmtInt(g.calls) + '</td>' +
    '<td class="nowrap">' + (g.failures ? '<span class="text-err">' + U.fmtInt(g.failures) + '</span>' : '0') + '</td>' +
    '<td class="nowrap">' + U.fmtInt((g.prompt_tokens || 0) + (g.completion_tokens || 0)) + '</td>' +
    '<td class="nowrap">' + U.fmtMs(g.avg_latency_ms) + '</td></tr>'
  ).join('');
  body.innerHTML =
    '<div class="stat-grid mb-4">' +
    '<div class="stat-card"><div class="stat-value">' + U.fmtInt(t.calls) + '</div><div class="stat-label">Calls</div></div>' +
    '<div class="stat-card"><div class="stat-value">' + U.fmtInt(t.failures) + '</div><div class="stat-label">Failures</div></div>' +
    '<div class="stat-card"><div class="stat-value">' + U.fmtInt(t.prompt_tokens + t.completion_tokens) + '</div><div class="stat-label">Tokens</div></div>' +
    '<div class="stat-card"><div class="stat-value">' + U.fmtMs(t.avgLatency) + '</div><div class="stat-label">Avg latency</div></div></div>' +
    '<div class="table-wrap"><table class="table"><thead><tr><th>Kind</th><th>Model</th><th>Calls</th><th>Failures</th><th>Tokens</th><th>Latency</th></tr></thead>' +
    '<tbody>' + rows + '</tbody></table></div>';
}

function healthCardHTML() {
  return '<div class="card"><div class="card-head"><span class="card-title-icon">' + U.icon('health', 17) +
    '</span><h3>Index health</h3><span class="spacer"></span><span id="health-badge"></span></div>' +
    '<div id="health-body">' + U.spinner() + '</div></div>';
}

function renderHealthBody(container, h) {
  const badge = container.querySelector('#health-badge');
  const body = container.querySelector('#health-body');
  const degraded = h.status !== 'ok';
  badge.innerHTML = U.badge(degraded ? 'degraded' : 'healthy', degraded ? 'warn' : 'ok');
  let html = '';
  if (h.config_needs_restart) {
    html += '<p class="text-warn">' + U.icon('alert', 14) +
      ' Configuration changed — restart the daemon for all changes to take effect.</p>';
  }
  html += '<dl class="meta-grid">' +
    '<dt>Embedding model</dt><dd>' + U.esc(h.embedding_model || '—') + ' ' +
    U.badge(h.embedding_model_status === 'ready' ? 'ready' : 'unavailable', h.embedding_model_status === 'ready' ? 'ok' : 'err') + '</dd>' +
    (h.embedding_dimension ? '<dt>Dimension</dt><dd>' + U.fmtInt(h.embedding_dimension) + '</dd>' : '') +
    '<dt>Processing</dt><dd>' + U.fmtInt(h.processing_count || 0) + '</dd>' +
    '<dt>Failed</dt><dd>' + (h.failed_count
      ? '<a href="#/library?status=failed" class="text-err">' + U.fmtInt(h.failed_count) + ' papers</a>'
      : '0') + '</dd>' +
    '</dl>';
  if (h.embedding_model_error) {
    html += '<p class="small text-err">' + U.esc(h.embedding_model_error) + '</p>';
  }
  if (h.reembed_total > 0 && h.reembed_completed < h.reembed_total) {
    const pct = Math.round((h.reembed_completed / h.reembed_total) * 100);
    html += '<div><div class="small muted mb-1">Re-embedding ' +
      U.fmtInt(h.reembed_completed) + ' / ' + U.fmtInt(h.reembed_total) + '</div>' +
      '<div class="progress" role="progressbar" aria-valuenow="' + pct + '" aria-valuemin="0" aria-valuemax="100">' +
      '<div data-bar></div></div></div>';
  } else if (h.reembed_pending > 0) {
    html += '<p class="small muted">' + U.fmtInt(h.reembed_pending) + ' papers pending re-embed.</p>';
  }
  if (degraded || h.embedding_model_status !== 'ready') {
    html += '<div class="mt-3"><a class="btn ghost sm" href="#/settings">Fix in Settings</a> ' +
      '<a class="btn subtle sm" href="#/health">Open Health</a></div>';
  }
  body.innerHTML = html;
  const bar = body.querySelector('[data-bar]');
  if (bar) {
    const pct = Math.round((h.reembed_completed / h.reembed_total) * 100);
    bar.style.width = pct + '%'; // dynamic value, not layout
  }
}

function syncCardHTML() {
  return '<div class="card"><div class="card-head"><span class="card-title-icon">' + U.icon('sync', 17) +
    '</span><h3>Sync</h3><span class="spacer"></span><a class="btn subtle sm" href="#/sync">Manage</a></div>' +
    '<div id="sync-body">' + U.spinner() + '</div></div>';
}

function renderSyncBody(container, zotero, lattice) {
  const body = container.querySelector('#sync-body');
  function row(name, st, err) {
    // err: request failed; st.available === false: service not reachable /
    // not configured — neither is "connected".
    if (err || !st || st.available === false) {
      return '<div class="queue-item"><strong>' + name + '</strong><span class="spacer"></span>' +
        U.badge('unavailable', 'muted') + '</div>';
    }
    const detail = st.username || st.app_version || '';
    let html = '<div class="queue-item"><strong>' + name + '</strong>';
    if (detail) html += '<span class="muted small">' + U.esc(detail) + '</span>';
    html += '<span class="spacer"></span>';
    if (st.auto_sync_paused) html += U.badge('auto-sync paused', 'warn');
    else if (st.consecutive_failures > 0) html += U.badge(st.consecutive_failures + ' failures', 'err');
    else html += U.badge('connected', 'ok');
    return html + '</div>';
  }
  body.innerHTML = row('Zotero', zotero.value, zotero.error) + row('Lattice', lattice.value, lattice.error) +
    '<p class="field-hint">Auto-sync runs in the background; trigger a manual sync from the Sync page.</p>';
}

function queueCardHTML() {
  return '<div class="card"><div class="card-head"><span class="card-title-icon">' + U.icon('list', 17) +
    '</span><h3>Index queue</h3><span class="spacer"></span><span id="queue-count"></span></div>' +
    '<div id="queue-body">' + U.spinner() + '</div></div>';
}

function storageTitle(stats) {
  if (!stats || !stats.storage) return '';
  const s = stats.storage;
  const parts = [];
  if (s.images_bytes) parts.push('Images: ' + U.fmtBytes(s.images_bytes));
  if (s.db_bytes) parts.push('Database (incl. WAL): ' + U.fmtBytes(s.db_bytes));
  if (s.covers_bytes) parts.push('Covers: ' + U.fmtBytes(s.covers_bytes));
  if (!parts.length) return '';
  return ' title="' + parts.join(' | ') + '"';
}

function renderDashboard(container) {
  let queueTimer = null;

  function row2(a, b) {
    // No align-items override: grid stretch keeps both cards the same
    // height, so rows align at top and bottom.
    return '<div class="grid cols-2 mt-6">' + a + b + '</div>';
  }

  // Build the layout once stats are known: the empty-library variant merges
  // the welcome hero with the add form instead of stacking two cards.
  // Cards pair by similar size; the MCP strip and LLM usage take full rows.
  function buildLayout(stats) {
    const empty = !stats || stats.papers === 0;
    let html = '';
    if (empty) {
      html += heroCardHTML();
      html += row2(healthCardHTML(), syncCardHTML());
      html += '<div class="mt-6">' + queueCardHTML() + '</div>';
    } else {
      html += '<div class="stat-grid">' +
        '<div class="stat-card"><div class="stat-value">' + U.fmtInt(stats.papers) + '</div><div class="stat-label">Papers</div></div>' +
        '<div class="stat-card"><div class="stat-value">' + U.fmtInt(stats.vectors) + '</div><div class="stat-label">Vectors</div></div>' +
        '<div class="stat-card"><div class="stat-value">' + U.fmtInt(stats.figures) + '</div><div class="stat-label">Figures</div></div>' +
        '<div class="stat-card"' + storageTitle(stats) + '><div class="stat-value">' + U.fmtMB(stats.data_dir_size_mb) + '</div><div class="stat-label">Data size</div></div>' +
        '</div>';
      html += row2(addPaperCardHTML(), queueCardHTML());
      html += row2(healthCardHTML(), syncCardHTML());
    }
    html += '<div class="mt-6">' + mcpCardHTML() + '</div>';
    html += '<div class="mt-6">' + metricsCardHTML() + '</div>';
    container.innerHTML = html;

    wireAddPaperCard(container, () => pollQueue());
    wireMcpCard(container);
    wireMetricsRange();
    loadHealth();
    loadSyncStatuses();
    loadMetrics();
    pollQueue();
  }

  container.innerHTML = U.spinner();
  API.stats().then(buildLayout).catch(() => {
    // Stats unreachable — still show the dashboard skeleton.
    buildLayout(null);
  });

  function settle(p) { return p.then((v) => ({ value: v }), (e) => ({ error: e })); }

  // Health
  function loadHealth() {
    API.health().then((h) => renderHealthBody(container, h)).catch((e) => {
      const body = container.querySelector('#health-body');
      if (body) body.innerHTML = '<p class="small text-err">' + U.esc(e.message) + '</p>';
    });
  }

  // Sync statuses
  function loadSyncStatuses() {
    Promise.all([settle(API.zoteroStatus()), settle(API.latticeStatus())]).then((r) => {
      renderSyncBody(container, { value: r[0].value, error: r[0].error }, { value: r[1].value, error: r[1].error });
    });
  }

  // Metrics with range toggle
  let metricsData = null;
  let range = 'last_24h';
  function paintMetrics() {
    const body = container.querySelector('#metrics-body');
    if (body && metricsData) renderMetricsBody(body, metricsData[range]);
  }
  function loadMetrics() {
    API.metrics().then((m) => { metricsData = m; paintMetrics(); }).catch(() => {
      const body = container.querySelector('#metrics-body');
      if (body) body.innerHTML = '<p class="muted small center pad-block">Metrics unavailable.</p>';
    });
  }
  function wireMetricsRange() {
    container.querySelectorAll('#metrics-range button').forEach((b) => {
      b.addEventListener('click', () => {
        range = b.getAttribute('data-range');
        container.querySelectorAll('#metrics-range button').forEach((x) => {
          const on = x === b;
          x.classList.toggle('active', on);
          x.setAttribute('aria-pressed', String(on));
        });
        paintMetrics();
      });
    });
  }

  // Index queue with adaptive polling
  function pollQueue() {
    if (!document.getElementById('queue-body')) return;
    API.importQueue().then((items) => {
      const body = container.querySelector('#queue-body');
      const count = container.querySelector('#queue-count');
      if (!body) return;
      if (!items.length) {
        count.innerHTML = '';
        body.innerHTML = '<p class="muted small center pad-block">Queue is empty — everything is indexed.</p>';
      } else {
        count.innerHTML = U.badge(items.length + ' active', 'info');
        body.innerHTML = items.slice(0, 20).map((it) =>
          '<div class="queue-item">' + U.badge(it.status, U.statusTone(it.status)) +
          '<span class="file-path" title="' + U.esc(it.file_path || '') + '">' + U.esc(it.file_path || it.paper_id) + '</span>' +
          '<a class="btn subtle sm" href="#/paper/' + encodeURIComponent(it.paper_id) + '">View</a></div>'
        ).join('');
      }
      queueTimer = setTimeout(pollQueue, items.length ? 2000 : 15000);
    }).catch(() => {
      queueTimer = setTimeout(pollQueue, 15000);
    });
  }

  return function destroy() { clearTimeout(queueTimer); };
}

route('/', { title: 'Dashboard', render: renderDashboard });

// ---- boot ---------------------------------------------------------------

function boot() {
  injectShellIcons();
  initAppearance();
  const toggle = document.getElementById('sidebar-toggle');
  toggle.addEventListener('click', () => {
    const open = document.getElementById('sidebar').classList.toggle('open');
    toggle.setAttribute('aria-expanded', String(open));
  });

  API.setupStatus().then((st) => {
    if (st.needs_setup) {
      document.getElementById('topbar-title').textContent = 'Welcome';
      const app = document.getElementById('app');
      app.innerHTML = '';
      renderWizard(app, () => {
        U.toast('Setup complete — welcome to Papered', 'success');
        navigate('#/');
        render();
      });
    } else {
      start();
    }
  }).catch(() => {
    // If setup status is unreachable, still try to render — views surface
    // their own errors.
    start();
  });
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot);
} else {
  boot();
}
