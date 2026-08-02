// Sync view: Zotero & Lattice sync management. Registers route('/sync').
// Each service card renders independently — one failing never blocks the other.

import * as U from './util.js?v=1';
import { API } from './api.js?v=1';
import { route } from './router.js?v=1';

// ---- shared helpers -------------------------------------------------------

// A sync job is terminal once its status is anything but pending/running.
function isTerminalStatus(status) {
  const s = String(status || '').toLowerCase();
  return s !== 'pending' && s !== 'running';
}

// Transport-level failures (daemon unreachable, dropped connection) reject
// with a bare TypeError and no HTTP status — the server message is
// unavailable, so surface an actionable hint instead of the browser's
// cryptic "Failed to fetch".
function failMessage(e) {
  if (e instanceof TypeError) {
    return 'Cannot reach the Papered daemon — it may have stopped. Check that it is running, then retry.';
  }
  return e && e.message ? e.message : String(e);
}

function statusBadge(st) {
  if (!st || st.available === false) return U.badge('Unavailable', 'muted');
  if (st.auto_sync_paused) return U.badge('Paused', 'warn');
  return U.badge('Connected', 'ok');
}

// SyncReport renderer shared by both services.
function reportHTML(report) {
  const errors = report.errors || [];
  const importedIds = report.imported_ids || [];
  const total = (report.imported || 0) + (report.pdf_found || 0)
    + (report.metadata_only || 0) + (report.skipped || 0)
    + (report.removed || 0) + errors.length;
  // An all-zero report means the source library had nothing new since the last
  // sync (incremental) — show a clear message instead of six empty stat cards.
  if (total === 0) {
    return '<div class="stat-card"><div class="stat-value">✓</div>' +
      '<div class="stat-label">Already up to date — no changes since last sync</div></div>';
  }
  let html = `<div class="stat-grid">${
    U.statCard(report.imported, 'Imported') +
    U.statCard(report.pdf_found, 'PDFs found') +
    U.statCard(report.metadata_only, 'Metadata only') +
    U.statCard(report.skipped, 'Skipped') +
    U.statCard(report.removed, 'Removed') +
    U.statCard(errors.length, 'Errors')
  }</div>`;
  if (errors.length) {
    html += `<div class="mt-2">${errors.slice(0, 10).map((e) =>
      `<div class="mono small text-err">${U.esc(e)}</div>`).join('')}${
      errors.length > 10
        ? `<div class="muted small">…and ${U.fmtInt(errors.length - 10)} more</div>`
        : ''}</div>`;
  }
  const links = [];
  if (importedIds.length) {
    links.push('<a class="btn ghost sm" href="#/library">View new papers</a>');
  }
  if ((report.imported || 0) > 0) {
    links.push('<a class="btn subtle sm" href="#/">Watch the index queue</a>');
  }
  if (links.length) html += `<div class="mt-3">${links.join(' ')}</div>`;
  return html;
}


// Error block inside a card with a Retry button (wired by the caller).
function cardErrorHTML(msg) {
  return U.errorState(msg, 'Retry');
}

function wireRetry(card, init) {
  const btn = card.querySelector('[data-retry]');
  if (btn) {
    btn.addEventListener('click', () => { init(); });
  }
}

// Interactive collection selector with checkboxes for Zotero sync scope.
function wireCollectionSelector(btn, panel, selectedKeys, recursive) {
  let loaded = false;

  function load() {
    loaded = true;
    panel.innerHTML = U.spinner('Loading collections…');
    API.zoteroCollections().then((res) => {
      if (!panel.isConnected) return;
      const items = res.collections || [];
      if (!items.length) {
        panel.innerHTML = '<p class="muted small">No collections found in Zotero.</p>';
        return;
      }
      const selected = new Set(selectedKeys);
      // Build a tree-like display: top-level first, then children indented.
      const byParent = {};
      items.forEach((c) => {
        const p = c.parent_collection || '';
        if (!byParent[p]) byParent[p] = [];
        byParent[p].push(c);
      });
      function renderTree(parentKey, depth) {
        const children = byParent[parentKey] || [];
        return children.map((c) => {
          const depthAttr = depth > 0 ? ` data-depth="${depth}"` : '';
          const checked = selected.has(c.key) ? ' checked' : '';
          let html = `<label class="collection-row"${depthAttr}>` +
            `<input type="checkbox" value="${U.esc(c.key)}"${checked}> ` +
            `<span>${U.esc(c.name)}</span>` +
            `<span class="mono muted small">${U.esc(c.key)}</span></label>`;
          html += renderTree(c.key, depth + 1);
          return html;
        }).join('');
      }
      const treeHTML = renderTree('', 0);
      panel.innerHTML =
        '<div class="field"><label class="checkbox-label">' +
        `<input type="checkbox" id="zotero-recursive"${recursive ? ' checked' : ''}> ` +
        'Include sub-collections of selected collections</label></div>' +
        '<div class="collection-list">' + treeHTML + '</div>' +
        '<div class="toolbar mt-2">' +
        '<button type="button" class="btn primary sm" data-save-collections>Save</button>' +
        '<button type="button" class="btn ghost sm" data-select-all>Select all</button>' +
        '<button type="button" class="btn ghost sm" data-select-none>Clear (sync all)</button>' +
        '</div>' +
        '<p class="field-hint mt-1">Uncheck all and save to sync the entire library. ' +
        'Selecting specific collections limits sync to those collections only.</p>';

      panel.querySelector('[data-save-collections]').addEventListener('click', () => {
        const checks = panel.querySelectorAll('.collection-list input[type=checkbox]:checked');
        const keys = Array.from(checks).map((el) => el.value);
        const recursiveVal = panel.querySelector('#zotero-recursive').checked;
        const saveBtn = panel.querySelector('[data-save-collections]');
        saveBtn.disabled = true;
        saveBtn.textContent = 'Saving…';
        API.zoteroSyncCollections(keys, recursiveVal).then(() => {
          saveBtn.textContent = 'Saved ✓';
          U.toast(keys.length
            ? `Sync scope: ${keys.length} collection${keys.length > 1 ? 's' : ''}`
            : 'Sync scope: entire library', 'success');
          setTimeout(() => { saveBtn.disabled = false; saveBtn.textContent = 'Save'; }, 1500);
        }).catch((e) => {
          saveBtn.disabled = false;
          saveBtn.textContent = 'Save';
          U.toast(e.message, 'error');
        });
      });
      panel.querySelector('[data-select-all]').addEventListener('click', () => {
        panel.querySelectorAll('.collection-list input[type=checkbox]')
          .forEach((el) => { el.checked = true; });
      });
      panel.querySelector('[data-select-none]').addEventListener('click', () => {
        panel.querySelectorAll('.collection-list input[type=checkbox]')
          .forEach((el) => { el.checked = false; });
      });
    }).catch((e) => {
      if (!panel.isConnected) return;
      // Keep the panel open and let the user retry: reset `loaded` so the
      // selector re-fetches on the next open, and offer an inline Retry.
      loaded = false;
      panel.innerHTML = cardErrorHTML(failMessage(e));
      wireRetry(panel, load);
    });
  }

  btn.addEventListener('click', () => {
    const open = panel.classList.toggle('hidden') === false;
    btn.setAttribute('aria-expanded', String(open));
    if (loaded || !open) return;
    load();
  });
}

// Interactive collection selector for Lattice sync scope. Lattice collections
// are selected by *name* (config matches names; names may not be unique, in
// which case all collections sharing the name are synced). No recursive toggle
// — sub-collections are separate entries with their own names/paths.
function wireLatticeCollectionSelector(btn, panel, selectedNames) {
  let loaded = false;

  function load() {
    loaded = true;
    panel.innerHTML = U.spinner('Loading collections…');
    API.latticeCollections().then((res) => {
      if (!panel.isConnected) return;
      const items = res.collections || [];
      if (!items.length) {
        panel.innerHTML = '<p class="muted small">No collections found in Lattice.</p>';
        return;
      }
      const selected = new Set(selectedNames);
      const rows = items.map((c) => {
        const checked = selected.has(c.name) ? ' checked' : '';
        const sub = c.path && c.path !== c.name
          ? `<span class="mono muted small"> · ${U.esc(c.path)}</span>` : '';
        return `<label class="collection-row">` +
          `<input type="checkbox" value="${U.esc(c.name)}"${checked}> ` +
          `<span>${U.esc(c.name)}</span>${sub}</label>`;
      }).join('');
      panel.innerHTML =
        '<div class="collection-list">' + rows + '</div>' +
        '<div class="toolbar mt-2">' +
        '<button type="button" class="btn primary sm" data-save-collections>Save</button>' +
        '<button type="button" class="btn ghost sm" data-select-all>Select all</button>' +
        '<button type="button" class="btn ghost sm" data-select-none>Clear (sync all)</button>' +
        '</div>' +
        '<p class="field-hint mt-1">Uncheck all and save to sync the entire library. ' +
        'Selecting collections limits sync to those collections only; papers outside ' +
        'them are removed from Papered. If two collections share a name, both are synced.</p>';

      panel.querySelector('[data-save-collections]').addEventListener('click', () => {
        const checks = panel.querySelectorAll('.collection-list input[type=checkbox]:checked');
        const names = Array.from(checks).map((el) => el.value);
        const saveBtn = panel.querySelector('[data-save-collections]');
        saveBtn.disabled = true;
        saveBtn.textContent = 'Saving…';
        API.latticeSyncCollections(names).then(() => {
          saveBtn.textContent = 'Saved ✓';
          U.toast(names.length
            ? `Sync scope: ${names.length} collection${names.length > 1 ? 's' : ''}`
            : 'Sync scope: entire library', 'success');
          setTimeout(() => { saveBtn.disabled = false; saveBtn.textContent = 'Save'; }, 1500);
        }).catch((e) => {
          saveBtn.disabled = false;
          saveBtn.textContent = 'Save';
          U.toast(e.message, 'error');
        });
      });
      panel.querySelector('[data-select-all]').addEventListener('click', () => {
        panel.querySelectorAll('.collection-list input[type=checkbox]')
          .forEach((el) => { el.checked = true; });
      });
      panel.querySelector('[data-select-none]').addEventListener('click', () => {
        panel.querySelectorAll('.collection-list input[type=checkbox]')
          .forEach((el) => { el.checked = false; });
      });
    }).catch((e) => {
      if (!panel.isConnected) return;
      // Keep the panel open and let the user retry: reset `loaded` so the
      // selector re-fetches on the next open, and offer an inline Retry.
      loaded = false;
      panel.innerHTML = cardErrorHTML(failMessage(e));
      wireRetry(panel, load);
    });
  }

  btn.addEventListener('click', () => {
    const open = panel.classList.toggle('hidden') === false;
    btn.setAttribute('aria-expanded', String(open));
    if (loaded || !open) return;
    load();
  });
}

// ---- Zotero card ----------------------------------------------------------

function initZoteroCard(card) {
  let pollTimer = null;
  let syncing = false;

  function destroy() {
    clearTimeout(pollTimer);
  }

  function stopPolling() {
    clearTimeout(pollTimer);
    pollTimer = null;
  }

  function setSyncing(on) {
    syncing = on;
    const syncBtn = card.querySelector('[data-zotero-sync]');
    const cancelBtn = card.querySelector('[data-zotero-cancel]');
    if (syncBtn) syncBtn.disabled = on;
    if (cancelBtn) cancelBtn.classList.toggle('hidden', !on);
  }

  const progressEl = () => card.querySelector('#zotero-progress');

  function showProgress(html) {
    const el = progressEl();
    if (el) { el.classList.remove('hidden'); el.innerHTML = html; }
  }

  function poll(syncId) {
    stopPolling();
    pollTimer = setTimeout(function tick() {
      if (!card.isConnected) return;
      API.zoteroSyncStatus(syncId).then((res) => {
        if (!card.isConnected) return;
        const status = String(res.status || '').toLowerCase();
        if (!isTerminalStatus(status)) {
          showProgress(U.spinner('Syncing… status: ' + status));
          pollTimer = setTimeout(tick, 2000);
          return;
        }
        setSyncing(false);
        if (res.report) {
          showProgress(reportHTML(res.report));
          U.toast('Sync complete — ' + U.fmtInt(res.report.imported || 0) + ' imported', 'success');
        } else if (status === 'failed') {
          showProgress(cardErrorHTML('Sync failed.' +
            (res.message ? ' ' + res.message : '')));
          wireRetry(card, startSync);
        } else if (status === 'cancelled' || status === 'canceled') {
          showProgress('<p class="muted small">Sync cancelled.</p>');
        } else {
          showProgress(`<p class="muted small">Sync finished with status: ${U.esc(status)}.</p>`);
        }
      }).catch((e) => {
        if (!card.isConnected) return;
        setSyncing(false);
        showProgress(cardErrorHTML(failMessage(e)));
        // The sync job keeps running server-side — retry resumes polling it.
        wireRetry(card, () => {
          setSyncing(true);
          poll(syncId);
        });
      });
    }, 2000);
  }

  function startSync() {
    showProgress(U.spinner('Starting sync…'));
    API.zoteroSync().then((res) => {
      if (!card.isConnected) return;
      setSyncing(true);
      showProgress(U.spinner('Syncing… status: ' + String(res.status || 'pending').toLowerCase()));
      poll(res.sync_id);
    }).catch((e) => {
      if (!card.isConnected) return;
      if (e.code === 'sync_busy' || e.status === 503) {
        showProgress('<p class="muted small">A sync is already running — check back shortly.</p>');
        U.toast('A sync is already running', 'info');
      } else {
        showProgress(cardErrorHTML(failMessage(e)));
        wireRetry(card, startSync);
      }
    });
  }

  function cancelSync() {
    API.zoteroSyncCancel().then((res) => {
      U.toast(res.message || (res.cancelled ? 'Sync cancelled' : 'No active sync'), 'info');
    }).catch((e) => {
      U.toast(e.message, 'error');
    });
  }

  function renderStatus(st) {
    const meta = [];
    if (st.username) meta.push(U.esc(st.username));
    if (st.base_url) meta.push(`<span class="mono">${U.esc(st.base_url)}</span>`);

    let callout = '';
    if (st.auto_sync_paused) {
      callout = `<p class="text-warn">${U.icon('alert', 14)
        } Auto-sync is paused after ${U.fmtInt(st.consecutive_failures || 0)
        } consecutive failures. A successful manual sync resumes it.</p>`;
    } else if ((st.consecutive_failures || 0) > 0) {
      callout = `<div class="mb-2">${
        U.badge(st.consecutive_failures + ' consecutive failures', 'err')}</div>`;
    }

    const selectedKeys = st.collection_keys || [];
    const recursive = st.recursive_collections !== false;
    const filterNote = selectedKeys.length
      ? `<p class="small muted mt-1">Syncing ${selectedKeys.length} selected collection${
          selectedKeys.length > 1 ? 's' : ''}${recursive ? ' (incl. sub-collections)' : ''}.</p>`
      : '<p class="small text-warn mt-1">No collection filter — all items in the library will be synced.</p>';

    card.querySelector('#zotero-badge').innerHTML = statusBadge(st);
    card.querySelector('#zotero-body').innerHTML =
      (meta.length ? `<p class="muted small">${meta.join(' · ')}</p>` : '') +
      callout +
      '<div class="toolbar">' +
      `<button type="button" class="btn primary" data-zotero-sync>${U.icon('sync', 15)} Sync now</button>` +
      '<button type="button" class="btn ghost" data-zotero-collections aria-expanded="false">Sync scope</button>' +
      '<span class="spacer"></span>' +
      '<button type="button" class="btn danger sm hidden" data-zotero-cancel>Cancel sync</button>' +
      '</div>' +
      filterNote +
      '<div id="zotero-progress" class="hidden mt-3" role="status"></div>' +
      '<div id="zotero-collections" class="hidden mt-3"></div>';

    if (st.available === false) {
      card.querySelector('#zotero-body').insertAdjacentHTML('afterbegin',
        '<p class="muted small">Zotero is not available on this daemon.</p>');
    }

    card.querySelector('[data-zotero-sync]').addEventListener('click', () => {
      if (!syncing) startSync();
    });
    card.querySelector('[data-zotero-cancel]').addEventListener('click', cancelSync);
    wireCollectionSelector(
      card.querySelector('[data-zotero-collections]'),
      card.querySelector('#zotero-collections'),
      selectedKeys,
      recursive
    );
  }

  function init() {
    stopPolling();
    setSyncing(false);
    card.querySelector('#zotero-badge').innerHTML = '';
    card.querySelector('#zotero-body').innerHTML = U.spinner('Checking Zotero…');
    API.zoteroStatus().then((st) => {
      if (!card.isConnected) return;
      renderStatus(st);
    }).catch((e) => {
      if (!card.isConnected) return;
      card.querySelector('#zotero-badge').innerHTML = U.badge('Unavailable', 'muted');
      card.querySelector('#zotero-body').innerHTML = cardErrorHTML(failMessage(e));
      wireRetry(card, init);
    });
  }

  init();
  return destroy;
}

function zoteroCardHTML() {
  return `<div class="card" id="zotero-card"><div class="card-head">${
    `<span class="card-title-icon">${U.icon('sync', 17)}</span><h3>Zotero</h3>`
  }<span class="spacer"></span><span id="zotero-badge"></span></div>` +
    `<div id="zotero-body">${U.spinner('Checking Zotero…')}</div></div>`;
}

// ---- Lattice card ---------------------------------------------------------

function initLatticeCard(card) {
  let syncing = false;

  function destroy() { /* no timers — latticeSync is one long request */ }

  function setSyncing(on) {
    syncing = on;
    const syncBtn = card.querySelector('[data-lattice-sync]');
    const cancelBtn = card.querySelector('[data-lattice-cancel]');
    if (syncBtn) {
      syncBtn.disabled = on;
      syncBtn.innerHTML = on
        ? '<span class="spinner small"></span> Syncing…'
        : U.icon('sync', 15) + ' Sync now';
    }
    if (cancelBtn) cancelBtn.classList.toggle('hidden', !on);
  }

  const progressEl = () => card.querySelector('#lattice-progress');

  function showProgress(html) {
    const el = progressEl();
    if (el) { el.classList.remove('hidden'); el.innerHTML = html; }
  }

  function startSync() {
    setSyncing(true);
    showProgress(U.spinner('Syncing with Lattice… this can take several minutes.'));
    API.latticeSync().then((report) => {
      if (!card.isConnected) return;
      showProgress(reportHTML(report));
      U.toast('Sync complete — ' + U.fmtInt(report.imported || 0) + ' imported', 'success');
    }).catch((e) => {
      if (!card.isConnected) return;
      showProgress(cardErrorHTML(failMessage(e)));
      U.toast('Lattice sync failed', 'error');
      wireRetry(card, startSync);
    }).finally(() => {
      if (card.isConnected) setSyncing(false);
    });
  }

  function cancelSync() {
    API.latticeSyncCancel().then((res) => {
      U.toast(res.message || (res.cancelled ? 'Sync cancelled' : 'No active sync'), 'info');
    }).catch((e) => {
      U.toast(e.message, 'error');
    });
  }

  // ---- search & import ----

  function importButtonLabel(res) {
    const paper = res.paper || {};
    const metaOnly = /metadata/i.test(String(paper.status || '') + ' ' + String(res.source || ''));
    if (res.indexing_queued) return metaOnly ? 'Imported (metadata only)' : 'Imported and queued for indexing';
    return metaOnly ? 'Imported (metadata only)' : 'Imported';
  }

  function wireImportButtons(resultsEl) {
    resultsEl.querySelectorAll('[data-import]').forEach((btn) => {
      btn.addEventListener('click', () => {
        btn.disabled = true;
        btn.textContent = 'Importing…';
        API.latticeImport(btn.getAttribute('data-import')).then((res) => {
          btn.textContent = 'Queued ✓';
          U.toast(importButtonLabel(res), 'success');
        }).catch((e) => {
          btn.disabled = false;
          btn.textContent = 'Import';
          U.toast(e.message, 'error');
        });
      });
    });
  }

  function runSearch() {
    const input = card.querySelector('#lattice-search-q');
    const resultsEl = card.querySelector('#lattice-results');
    const q = input.value.trim();
    if (!q) return;
    resultsEl.innerHTML = U.spinner('Searching Lattice…');
    API.latticeSearch(q).then((res) => {
      if (!card.isConnected) return;
      const papers = res.papers || [];
      if (!papers.length) {
        resultsEl.innerHTML = `<p class="muted small">No papers matched “${U.esc(q)}”.</p>`;
        return;
      }
      resultsEl.innerHTML = '<div class="table-wrap"><table class="table"><thead><tr>' +
        '<th>Title</th><th>Authors</th><th>Year</th><th>Type</th><th>Citekey</th><th></th>' +
        `</tr></thead><tbody>${papers.map((p) =>
          `<tr><td>${U.esc(p.title)}</td>` +
          `<td>${U.esc(U.truncate(p.authorsDisplay || '', 60))}</td>` +
          `<td class="nowrap">${U.esc(p.year || '—')}</td>` +
          `<td>${U.badge(p.paperType || 'unknown', 'muted')}</td>` +
          `<td class="mono">${U.esc(p.citekey || '')}</td>` +
          `<td><button type="button" class="btn ghost sm" data-import="${
            U.esc(p.id)}">Import</button></td></tr>`
        ).join('')}</tbody></table></div>`;
      wireImportButtons(resultsEl);
    }).catch((e) => {
      if (!card.isConnected) return;
      resultsEl.innerHTML = cardErrorHTML(failMessage(e));
      wireRetry(card, runSearch);
    });
  }

  function renderStatus(st) {
    const meta = [];
    if (st.app_version) meta.push('app ' + U.esc(st.app_version));
    if (st.api_version) meta.push('api ' + U.esc(st.api_version));
    if (st.base_url) meta.push(`<span class="mono">${U.esc(st.base_url)}</span>`);

    let callout = '';
    if (st.auto_sync_paused) {
      callout = `<p class="text-warn">${U.icon('alert', 14)
        } Auto-sync is paused after ${U.fmtInt(st.consecutive_failures || 0)
        } consecutive failures. A successful manual sync resumes it.</p>`;
    } else if ((st.consecutive_failures || 0) > 0) {
      callout = `<div class="mb-2">${
        U.badge(st.consecutive_failures + ' consecutive failures', 'err')}</div>`;
    }

    // Capabilities are read-only info badges (what the Lattice app's local API
    // supports) — rendered as badges, not chips, so they don't look clickable.
    const caps = (st.capabilities || []).length
      ? `<div class="mt-1 mb-1" title="API capabilities reported by the Lattice app">${
          st.capabilities.map((c) => U.badge(c, 'muted')).join(' ')}</div>`
      : '';

    const selectedNames = st.collections || [];
    const filterNote = selectedNames.length
      ? `<p class="small muted mt-1">Syncing ${selectedNames.length} selected collection${
          selectedNames.length > 1 ? 's' : ''}.</p>`
      : '<p class="small text-warn mt-1">No collection filter — all collections will be synced.</p>';

    card.querySelector('#lattice-badge').innerHTML = statusBadge(st);
    card.querySelector('#lattice-body').innerHTML =
      (meta.length ? `<p class="muted small">${meta.join(' · ')}</p>` : '') +
      caps +
      callout +
      '<div class="toolbar">' +
      `<button type="button" class="btn primary" data-lattice-sync>${U.icon('sync', 15)} Sync now</button>` +
      '<button type="button" class="btn ghost" data-lattice-collections aria-expanded="false">Sync scope</button>' +
      '<span class="spacer"></span>' +
      '<button type="button" class="btn danger sm hidden" data-lattice-cancel>Cancel sync</button>' +
      '</div>' +
      filterNote +
      '<div id="lattice-progress" class="hidden mt-3" role="status"></div>' +
      '<div id="lattice-collections" class="hidden mt-3"></div>' +
      '<hr class="divider">' +
      '<div class="field"><label for="lattice-search-q">Search Lattice &amp; import</label>' +
      '<div class="toolbar">' +
      '<input class="input" id="lattice-search-q" placeholder="Search by title, author, citekey…">' +
      `<button type="button" class="btn ghost" id="lattice-search-btn">${U.icon('search', 15)} Search</button>` +
      '</div></div>' +
      '<div id="lattice-results" class="mt-2" role="status"></div>';

    card.querySelector('[data-lattice-sync]').addEventListener('click', () => {
      if (!syncing) startSync();
    });
    card.querySelector('[data-lattice-cancel]').addEventListener('click', cancelSync);
    card.querySelector('#lattice-search-btn').addEventListener('click', runSearch);
    card.querySelector('#lattice-search-q').addEventListener('keydown', (e) => {
      if (e.key === 'Enter') runSearch();
    });
    wireLatticeCollectionSelector(
      card.querySelector('[data-lattice-collections]'),
      card.querySelector('#lattice-collections'),
      selectedNames
    );
  }

  function renderUnavailable(msg, hint) {
    card.querySelector('#lattice-badge').innerHTML = U.badge('Unavailable', 'muted');
    card.querySelector('#lattice-body').innerHTML =
      cardErrorHTML(msg) +
      (hint ? `<p class="muted small center">${U.esc(hint)}</p>` : '');
    wireRetry(card, init);
  }

  function init() {
    card.querySelector('#lattice-badge').innerHTML = '';
    card.querySelector('#lattice-body').innerHTML = U.spinner('Checking Lattice…');
    API.latticeStatus().then((st) => {
      if (!card.isConnected) return;
      if (st.available === false) {
        renderUnavailable('Lattice is unavailable.', null);
        return;
      }
      renderStatus(st);
    }).catch((e) => {
      if (!card.isConnected) return;
      const hint = (e.code === 'lattice_error' || e.status === 502)
        ? 'Lattice is not configured on this daemon.'
        : null;
      renderUnavailable(failMessage(e), hint);
    });
  }

  init();
  return destroy;
}

function latticeCardHTML() {
  return `<div class="card" id="lattice-card"><div class="card-head">${
    `<span class="card-title-icon">${U.icon('sparkles', 17)}</span><h3>Lattice</h3>`
  }<span class="spacer"></span><span id="lattice-badge"></span></div>` +
    `<div id="lattice-body">${U.spinner('Checking Lattice…')}</div></div>`;
}

// ---- view -----------------------------------------------------------------

function render(container) {
  container.innerHTML =
    '<div class="page-head">' +
    '<p class="page-sub">Keep your library in step with Zotero and Lattice</p></div>' +
    zoteroCardHTML() +
    latticeCardHTML();

  const destroyZotero = initZoteroCard(container.querySelector('#zotero-card'));
  const destroyLattice = initLatticeCard(container.querySelector('#lattice-card'));

  return function destroy() {
    destroyZotero();
    destroyLattice();
  };
}

route('/sync', { title: 'Sync', render });
