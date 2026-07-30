// Health view: index health overview, data quality, duplicates, integrity,
// image quality, and maintenance actions. Registers route('/health').

import * as U from './util.js?v=2';
import { API } from './api.js?v=2';
import { route } from './router.js?v=2';

// ---- tab registry ----------------------------------------------------------
// Each tab: fetch() loads data, paint(el, data, ctx) renders into the tab body.
// ctx = { reload: fn, refreshIntegrity: fn }.

const TABS = [
  { id: 'overview', label: 'Overview', fetch: () => API.health() },
  { id: 'quality', label: 'Data quality', fetch: () => API.dataQuality() },
  { id: 'duplicates', label: 'Duplicates', fetch: () => API.duplicates() },
  { id: 'integrity', label: 'Integrity', fetch: () => API.kbHealth() },
  { id: 'images', label: 'Images', fetch: () => API.imageQuality() },
  { id: 'maintenance', label: 'Maintenance', fetch: null } // static, no fetch
];

// ---- overview tab ----------------------------------------------------------

function statCard(value, label) {
  return `<div class="stat-card"><div class="stat-value">${value}</div>` +
    `<div class="stat-label">${U.esc(label)}</div></div>`;
}

function paintOverview(el, h) {
  let html = '<div class="stat-grid">' +
    statCard(U.fmtInt(h.paper_count), 'Papers') +
    statCard(U.fmtInt(h.vector_count), 'Vectors') +
    statCard(h.embedding_dimension ? U.fmtInt(h.embedding_dimension) : '—', 'Embedding dim') +
    statCard(U.fmtInt(h.processing_count), 'Processing') +
    statCard(U.fmtInt(h.failed_count), 'Failed') +
    '</div>';

  if (h.config_needs_restart) {
    html += '<div class="card card-warn">' +
      `<p class="text-warn mt-0 mb-0">${U.icon('alert', 15)}` +
      ' Configuration changed — restart the daemon for all changes to take effect.</p></div>';
  }

  const ready = h.embedding_model_status === 'ready';
  html += `<div class="card"><div class="card-head"><h3>Embedding model</h3>${
    `<span class="spacer"></span>${U.badge(ready ? 'ready' : 'unavailable', ready ? 'ok' : 'err')}</div>`
  }<p class="mono small">${U.esc(h.embedding_model || '—')}</p>${
    h.embedding_model_error ? `<p class="small text-err">${U.esc(h.embedding_model_error)}</p>` : ''}${
    !ready ? '<div class="mt-2"><a class="btn ghost sm" href="#/settings">Fix in Settings</a></div>' : ''
  }</div>`;

  let pct = 0;
  if (h.reembed_total > 0) {
    pct = Math.round((h.reembed_completed / h.reembed_total) * 100);
    html += `<div class="card"><div class="card-head"><h3>Re-embedding</h3></div>${
      `<div class="small muted mb-1">${U.fmtInt(h.reembed_completed)} / ${
        U.fmtInt(h.reembed_total)} papers${
        h.reembed_pending > 0 ? ' · ' + U.fmtInt(h.reembed_pending) + ' pending' : ''}</div>`
    }<div class="progress" role="progressbar" aria-valuenow="${pct}" aria-valuemin="0" aria-valuemax="100">` +
      '<div data-bar></div></div></div>';
  } else if (h.reembed_pending > 0) {
    html += `<p class="small muted mt-3">${U.icon('alert', 14)} ${
      U.fmtInt(h.reembed_pending)} papers pending re-embed.</p>`;
  }

  if (h.failed_count > 0) {
    html += `<p class="mt-3"><a href="#/library?status=failed" class="text-err">${
      U.icon('alert', 14)} View ${U.fmtInt(h.failed_count)} failed papers</a></p>`;
  }

  el.innerHTML = html;
  const bar = el.querySelector('[data-bar]');
  if (bar) bar.style.width = pct + '%'; // dynamic value, not layout
}

// ---- data quality tab --------------------------------------------------------

function paintQuality(el, d) {
  const papers = d.papers_with_issues || [];
  let html = `<p class="muted small">${U.fmtInt(d.total_checked)} papers checked.</p>`;
  const tags = Object.keys(d.issue_summary || {});
  if (tags.length) {
    html += `<div class="mt-2 mb-3">${tags.map((t) =>
      `<span class="chip">${U.esc(t)} ×${U.fmtInt(d.issue_summary[t])}</span>`).join(' ')}</div>`;
  }
  if (!papers.length) {
    el.innerHTML = html + U.emptyState({
      icon: 'check',
      title: 'No quality issues found',
      body: 'Titles, authors, abstracts and indexed content all look healthy.'
    });
    return;
  }
  html += `<div class="table-wrap"><table class="table"><thead><tr><th>Paper</th><th>Issues</th></tr></thead><tbody>${
    papers.map((p) =>
      `<tr><td><a href="#/paper/${encodeURIComponent(p.paper_id)}">${
        U.esc(p.title || p.paper_id)}</a></td><td>${
        (p.issues || []).map((t) => `<span class="badge warn issue-tag">${U.esc(t)}</span>`).join('')
      }</td></tr>`
    ).join('')}</tbody></table></div>`;
  html += `<button type="button" class="btn primary sm mt-4" data-reindex-all>${U.icon('refresh', 14)} Reindex sections for all</button>
    <span class="muted small ml-2" data-reindex-msg></span>`;
  el.innerHTML = html;
  const btn = el.querySelector('[data-reindex-all]');
  const msg = el.querySelector('[data-reindex-msg]');
  btn.addEventListener('click', async () => {
    btn.disabled = true;
    msg.textContent = 'Queuing ' + papers.length + ' papers…';
    try {
      const res = await API.batchReindexSections(papers.map((p) => p.paper_id));
      const ok = (res.success || []).length;
      const err = (res.errors || []).length;
      msg.textContent = 'Done — ' + ok + ' queued' + (err ? ', ' + err + ' failed' : '');
    } catch (e) {
      msg.textContent = 'Error: ' + (e.message || e);
    }
    btn.disabled = false;
  });
}

// ---- duplicates tab ----------------------------------------------------------

function matchBasis(group) {
  const hashes = group.filter((p) => p.file_hash);
  return hashes.length > 1 ? 'same file hash' : 'same title / authors / date';
}

function paintDuplicates(el, d, ctx) {
  const groups = d.groups || [];
  if (!groups.length) {
    el.innerHTML = U.emptyState({
      icon: 'check',
      title: 'No duplicates found',
      body: 'No papers share the same file hash or title, authors and date.'
    });
    return;
  }
  let html = '<p class="muted small">Papers that appear to be the same document, grouped by matching file hash or metadata. Deleting a paper removes it from the library permanently.</p>';
  groups.forEach((g, gi) => {
    html += `<div class="card mt-4"><div class="card-head"><h3>Group ${gi + 1}</h3>${
      U.badge(g.length + ' papers', 'warn')}<span class="spacer"></span>${
      `<span class="muted small">${matchBasis(g)}</span></div>`
    }<div class="table-wrap"><table class="table"><thead><tr><th>Title</th><th>Authors</th><th>Published</th>` +
      `<th>Hash</th><th>Status</th><th>Updated</th><th></th></tr></thead><tbody>${
      g.map((p) => {
        const authors = (p.authors || []).slice(0, 2).join(', ') + ((p.authors || []).length > 2 ? ' et al.' : '');
        return `<tr><td><a href="#/paper/${encodeURIComponent(p.id)}">${
          U.esc(U.truncate(p.title || p.id, 60))}</a></td>` +
          `<td class="small">${U.esc(authors || '—')}</td>` +
          `<td class="nowrap small">${U.esc(p.published_date || '—')}</td>` +
          `<td class="mono small" title="${U.esc(p.file_hash || '')}">${
            p.file_hash ? U.esc(U.truncate(p.file_hash, 12)) : '—'}</td>` +
          `<td>${U.badge(p.status, U.statusTone(p.status))}</td>` +
          `<td class="nowrap small muted">${U.esc(U.relTime(p.updated_at))}</td>` +
          `<td><button type="button" class="btn danger sm" data-del-group="${gi}" data-del="${
            U.esc(p.id)}">Delete</button></td></tr>`;
      }).join('')}</tbody></table></div></div>`;
  });
  el.innerHTML = html;
  el.querySelectorAll('[data-del]').forEach((b) => {
    b.addEventListener('click', () => {
      const id = b.getAttribute('data-del');
      U.confirm({
        title: 'Delete paper',
        body: 'Delete this paper? This cannot be undone.',
        confirmLabel: 'Delete',
        danger: true
      }).then((ok) => {
        if (!ok) return;
        b.disabled = true;
        API.deletePaper(id).then(() => {
          U.toast('Paper deleted', 'success');
          ctx.reload();
        }).catch((e) => {
          b.disabled = false;
          U.toast(e.message, 'error');
        });
      });
    });
  });
}

// ---- integrity tab -----------------------------------------------------------

function paintIntegrity(el, k) {
  const lists = [
    { key: 'papers_without_vectors', label: 'Papers without vectors', kind: 'papers' },
    { key: 'orphaned_vector_paper_ids', label: 'Orphaned vectors (paper ids)', kind: 'plain' },
    { key: 'papers_with_missing_files', label: 'Papers with missing files', kind: 'papers' },
    { key: 'figures_with_missing_images', label: 'Figures with missing images', kind: 'figures' },
    { key: 'orphaned_directories', label: 'Orphaned directories', kind: 'dirs' }
  ];
  let total = 0;
  lists.forEach((l) => { total += (k[l.key] || []).length; });

  let html = `<div class="grid cols-3">${lists.map((l) => {
    const n = (k[l.key] || []).length;
    return `<div class="stat-card"><div class="stat-value${n ? ' text-warn' : ''}">${
      U.fmtInt(n)}</div><div class="stat-label">${U.esc(l.label)}</div></div>`;
  }).join('')}</div>`;

  if (!total) {
    el.innerHTML = html + `<div class="mt-4">${U.emptyState({
      icon: 'check',
      title: 'Index is consistent',
      body: 'No missing vectors, orphaned data, or missing files detected.'
    })}</div>`;
    return;
  }

  lists.forEach((l) => {
    const items = k[l.key] || [];
    if (!items.length) return;
    html += `<div class="card"><div class="card-head"><h3>${U.esc(l.label)}</h3>${
      U.badge(items.length, 'warn')}</div><div class="link-list">${
      items.slice(0, 50).map((it) => {
        if (l.kind === 'papers') {
          return `<a href="#/paper/${encodeURIComponent(it)}" class="mono small">${U.esc(it)}</a>`;
        }
        if (l.kind === 'figures') {
          return `<a href="#/paper/${encodeURIComponent(it.paper_id)}" class="mono small">${
            U.esc(it.paper_id)} · figure ${U.esc(it.figure_id)}</a>`;
        }
        return `<span class="mono small">${U.esc(it)}</span>`;
      }).join('')}${
      items.length > 50 ? `<span class="muted small">… and ${U.fmtInt(items.length - 50)} more</span>` : ''
    }</div></div>`;
  });

  html += `<p class="field-hint mt-3">${U.icon('alert', 14)}` +
    ' Cleanup (Maintenance tab) removes orphaned vectors, directories, and papers whose files are missing.</p>';
  el.innerHTML = html;
}

// ---- images tab ---------------------------------------------------------------

function paintImages(el, d) {
  const bad = d.bad_images || [];
  let html = `<p class="muted small">${U.fmtInt(d.total_scanned)} figure images scanned.</p>`;
  if (!bad.length) {
    el.innerHTML = html + U.emptyState({
      icon: 'check',
      title: 'No image problems found',
      body: 'All extracted figure images passed quality checks.'
    });
    return;
  }
  html += `<div class="table-wrap"><table class="table"><thead><tr><th>Paper</th><th>File</th><th>Reason</th></tr></thead><tbody>${
    bad.map((b) =>
      `<tr><td><a href="#/paper/${encodeURIComponent(b.paper_id)}">${
        U.esc(U.truncate(b.paper_title || b.paper_id, 60))}</a></td>` +
      `<td class="mono small">${U.esc(b.filename)}</td>` +
      `<td class="small">${U.esc(b.reason)}</td></tr>`
    ).join('')}</tbody></table></div>`;
  el.innerHTML = html;
}

// ---- maintenance tab ------------------------------------------------------------

function actionCard(title, desc, btnLabel, btnKind, btnId) {
  return `<div class="card"><div class="card-head"><h3>${U.esc(title)}</h3></div>` +
    `<p class="muted small">${U.esc(desc)}</p>` +
    `<div class="mt-2"><button type="button" class="btn ${btnKind}" id="${btnId}">${
      U.esc(btnLabel)}</button></div></div>`;
}

function paintMaintenance(el, _data, ctx) {
  el.innerHTML =
    '<div class="grid cols-2">' +
    actionCard('Optimize store', 'Compact and optimize the database file.', 'Optimize', 'primary', 'm-optimize') +
    actionCard('Optimize images', 'Re-encode all figure images using current settings (reduces storage significantly). Shows a savings preview before changing anything.', 'Optimize images', 'primary', 'm-optimize-images') +
    actionCard('Clean up KB', 'Remove papers whose files are missing, orphaned vectors and empty directories.',
      'Clean up', 'danger', 'm-cleanup') +
    actionCard('Clean up images', 'Delete figure images that fail quality checks.', 'Clean up images', 'danger', 'm-clean-images') +
    '<div class="card"><div class="card-head"><h3>Export</h3></div>' +
    '<div class="field"><label for="m-export-target">Target</label>' +
    '<select class="select" id="m-export-target">' +
    '<option value="database">Database</option>' +
    '<option value="sections">Sections</option>' +
    '<option value="full_papers">Full papers</option></select></div>' +
    '<div class="field"><label for="m-export-format">Format</label>' +
    '<select class="select" id="m-export-format">' +
    '<option value="json">JSON</option>' +
    '<option value="csv">CSV</option>' +
    '<option value="markdown">Markdown</option>' +
    '<option value="sqlite">SQLite</option></select></div>' +
    '<div class="field"><label for="m-export-dest">Destination directory</label>' +
    '<input class="input mono" id="m-export-dest" placeholder="~/Downloads/papered-export">' +
    '<div class="field-hint">Must be inside ~/Downloads, ~/Documents, ~/Desktop, a temp dir, or the data directory.</div></div>' +
    `<button type="button" class="btn primary" id="m-export">${U.icon('download', 15)} Export</button>` +
    '</div></div>';

  // Optimize
  el.querySelector('#m-optimize').addEventListener('click', function () {
    const b = this;
    b.disabled = true;
    API.optimize().then((r) => {
      U.toast(r.message || 'Database optimized', 'success');
    }).catch((e) => {
      U.toast(e.message, 'error');
    }).finally(() => { b.disabled = false; });
  });

  // Optimize images (destructive: lossy re-encode in place) — two-phase:
  // dry-run preview first, real run only after explicit confirmation.
  el.querySelector('#m-optimize-images').addEventListener('click', function () {
    const b = this;
    b.disabled = true;
    b.textContent = 'Analyzing…';
    API.optimizeImages(true).then((preview) => {
      b.disabled = false;
      b.textContent = 'Optimize images';
      if (!preview.images_processed) {
        U.modal({
          title: 'Optimize images',
          body: '<p class="modal-text">Nothing to optimize — no processable figure images found.' +
            (preview.images_skipped
              ? ' (' + U.fmtInt(preview.images_skipped) + ' skipped: no image file on disk.)' : '') +
            '</p>',
          actions: [{ label: 'OK', value: true, kind: 'primary' }]
        });
        return;
      }
      U.modal({
        title: 'Optimize ' + U.fmtInt(preview.images_processed) + ' images?',
        body: '<p class="modal-text">Images are re-encoded in place with the current settings. ' +
          'This cannot be undone. Dry-run estimate:</p>' +
          '<dl class="meta-grid">' +
          `<dt>Images</dt><dd>${U.fmtInt(preview.images_processed)}</dd>` +
          `<dt>Current size</dt><dd>${U.esc(preview.size_before || '')}</dd>` +
          `<dt>Estimated size after</dt><dd>${U.esc(preview.size_after || '')}</dd>` +
          `<dt>Estimated savings</dt><dd>${U.esc(preview.saved || '0 B')}</dd></dl>`,
        actions: [
          { label: 'Cancel', value: null, kind: 'ghost' },
          {
            label: 'Optimize', kind: 'danger',
            onClick: async (close, btn) => {
              btn.disabled = true;
              btn.textContent = 'Optimizing…';
              try {
                const r = await API.optimizeImages(false);
                close(true);
                U.modal({
                  title: 'Image optimization complete',
                  body: '<dl class="meta-grid">' +
                    `<dt>Images processed</dt><dd>${U.fmtInt(r.images_processed)}</dd>` +
                    `<dt>Before</dt><dd>${U.esc(r.size_before || '')}</dd>` +
                    `<dt>After</dt><dd>${U.esc(r.size_after || '')}</dd>` +
                    `<dt>Space saved</dt><dd>${U.esc(r.saved || '0 B')}</dd></dl>`,
                  actions: [{ label: 'OK', value: true, kind: 'primary' }]
                });
                ctx.refreshIntegrity();
              } catch (e) {
                close(null);
                U.toast(e.message, 'error');
              }
            }
          }
        ]
      });
    }).catch((e) => {
      b.disabled = false;
      b.textContent = 'Optimize images';
      U.toast(e.message, 'error');
    });
  });

  // Clean up KB (destructive)
  el.querySelector('#m-cleanup').addEventListener('click', function () {
    const b = this;
    U.confirm({
      title: 'Clean up knowledge base',
      body: 'This permanently removes papers whose files are missing, orphaned vectors, and empty directories. This cannot be undone. Continue?',
      confirmLabel: 'Clean up',
      danger: true
    }).then((ok) => {
      if (!ok) return;
      b.disabled = true;
      API.cleanup().then((r) => {
        b.disabled = false;
        const removedPapers = (r.removed_papers || []).length;
        const removedVectors = (r.removed_orphan_vectors || []).length;
        const removedDirs = (r.removed_orphan_directories || []).length;
        U.modal({
          title: 'Cleanup complete',
          body: '<dl class="meta-grid">' +
            `<dt>Papers removed</dt><dd>${U.fmtInt(removedPapers)}</dd>` +
            `<dt>Orphaned vectors removed</dt><dd>${U.fmtInt(removedVectors)}</dd>` +
            `<dt>Directories removed</dt><dd>${U.fmtInt(removedDirs)}</dd>` +
            `<dt>Figure records removed</dt><dd>${U.fmtInt(r.removed_figures || 0)}</dd></dl>`,
          actions: [{ label: 'OK', value: true, kind: 'primary' }]
        });
        ctx.refreshIntegrity();
      }).catch((e) => {
        b.disabled = false;
        U.toast(e.message, 'error');
      });
    });
  });

  // Clean up images (destructive)
  el.querySelector('#m-clean-images').addEventListener('click', function () {
    const b = this;
    U.confirm({
      title: 'Clean up images',
      body: 'This permanently deletes figure images that fail quality checks. This cannot be undone. Continue?',
      confirmLabel: 'Delete images',
      danger: true
    }).then((ok) => {
      if (!ok) return;
      b.disabled = true;
      API.cleanupImages().then((r) => {
        U.toast('Removed ' + U.fmtInt(r.removed || 0) + ' images', 'success');
      }).catch((e) => {
        U.toast(e.message, 'error');
      }).finally(() => { b.disabled = false; });
    });
  });

  // Export
  el.querySelector('#m-export').addEventListener('click', function () {
    const b = this;
    const dest = el.querySelector('#m-export-dest').value.trim();
    if (!dest) {
      U.toast('Enter a destination directory', 'error');
      return;
    }
    b.disabled = true;
    API.exportData({
      target: el.querySelector('#m-export-target').value,
      format: el.querySelector('#m-export-format').value,
      destination: dest
    }).then((r) => {
      b.disabled = false;
      U.modal({
        title: 'Export complete',
        body: `<p class="modal-text">${U.fmtInt(r.papers_exported || 0)} papers exported.</p>` +
          `<div class="link-list mt-2">${
            (r.files_created || []).map((f) => `<span class="mono small">${U.esc(f)}</span>`).join('')
          }</div>`,
        actions: [{ label: 'OK', value: true, kind: 'primary' }]
      });
    }).catch((e) => {
      b.disabled = false;
      U.toast(e.message, 'error');
    });
  });
}

const PAINTERS = {
  overview: paintOverview,
  quality: paintQuality,
  duplicates: paintDuplicates,
  integrity: paintIntegrity,
  images: paintImages,
  maintenance: paintMaintenance
};

// ---- view ----------------------------------------------------------------------

function renderHealth(container) {
  // Cached per-tab data so switching tabs is instant; null = not loaded.
  const cache = {};
  let active = 'overview';

  container.innerHTML =
    '<div class="page-head">' +
    '<div class="page-sub">Index health, data quality, and maintenance.</div></div>' +
    '<div class="toolbar mb-4">' +
    `<div class="tabs" id="health-tabs" role="tablist">${TABS.map((t) =>
      `<button type="button" role="tab" data-tab="${t.id}" aria-selected="${t.id === active}"${
        t.id === active ? ' class="active"' : ''}>${U.esc(t.label)}</button>`
    ).join('')}</div>` +
    '<span class="spacer"></span>' +
    `<button type="button" class="btn ghost sm" id="health-refresh">${U.icon('refresh', 14)} Refresh</button></div>` +
    '<div id="health-body"></div>';

  const body = () => container.querySelector('#health-body');

  const tabDef = (id) => TABS.find((t) => t.id === id) || TABS[0];

  const ctx = {
    reload: () => { cache[active] = null; activate(active, true); },
    refreshIntegrity: () => {
      cache.integrity = null;
      if (active === 'integrity') activate('integrity', true);
    }
  };

  function activate(id, force) {
    active = id;
    container.querySelectorAll('#health-tabs button').forEach((b) => {
      const on = b.getAttribute('data-tab') === id;
      b.classList.toggle('active', on);
      b.setAttribute('aria-selected', String(on));
    });
    const el = body();
    if (!el) return;
    if (!force && cache[id]) {
      PAINTERS[id](el, cache[id], ctx);
      return;
    }
    const def = tabDef(id);
    if (!def.fetch) {
      cache[id] = {};
      PAINTERS[id](el, cache[id], ctx);
      return;
    }
    el.innerHTML = U.spinner('Loading…');
    def.fetch().then((data) => {
      cache[id] = data;
      if (active !== id) return; // user switched tabs while loading
      const el2 = body();
      if (el2) PAINTERS[id](el2, data, ctx);
    }).catch((e) => {
      if (active !== id) return;
      const el2 = body();
      if (!el2) return;
      el2.innerHTML = U.errorState(e.message, 'Retry');
      const r = el2.querySelector('[data-retry]');
      if (r) r.addEventListener('click', () => { activate(id, true); });
    });
  }

  container.querySelectorAll('#health-tabs button').forEach((b) => {
    b.addEventListener('click', () => { activate(b.getAttribute('data-tab')); });
  });
  const refreshBtn = container.querySelector('#health-refresh');
  if (refreshBtn) {
    refreshBtn.addEventListener('click', () => { activate(active, true); });
  }

  // First render: fetch overview immediately.
  activate('overview', true);
}

route('/health', { title: 'Health', render: renderHealth });
