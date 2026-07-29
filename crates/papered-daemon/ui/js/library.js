// Papers library list (#/library) view.
// ES module — routes self-register on import via app.js.
// Shared helpers are exported for use by library-detail.js and library-import.js.

import * as U from './util.js?v=1';
import { API } from './api.js?v=1';
import { route } from './router.js?v=1';
import { openAddModal } from './library-import.js?v=1';
import './library-detail.js?v=1';

const PAGE_SIZE = 20;
const POLL_MS = 3000;

// ---- shared helpers -------------------------------------------------------

// Up to `max` entity chips, prefixed like "gene: BRCA1".
function entityChips(paper, max) {
  const ents = paper.entities || {};
  const groups = [
    ['gene', ents.genes],
    ['species', ents.species],
    ['tech', ents.techniques],
    ['pathway', ents.pathways]
  ];
  let html = '';
  let n = 0;
  for (const [label, vals] of groups) {
    if (n >= max) break;
    for (const v of vals || []) {
      if (n >= max) break;
      html += `<span class="chip">${U.esc(label + ': ' + v)}</span>`;
      n++;
    }
  }
  return html;
}

// Swap a broken cover <img> for a placeholder div (CSP forbids onerror="").
export function wireCoverFallback(img) {
  img.addEventListener('error', () => {
    const d = document.createElement('div');
    d.className = img.className + ' placeholder';
    d.innerHTML = U.icon('file', 20);
    img.replaceWith(d);
  });
}

// Clicking a link inside a U.modal should also close the modal cleanly.
export function wireModalLinks(backdrop) {
  backdrop.querySelectorAll('a[href^="#/"]').forEach((a) => {
    a.addEventListener('click', () => {
      const x = backdrop.querySelector('.modal-close');
      if (x) x.click();
    });
  });
}

// Last-opened modal backdrop (U.modal appends synchronously).
export function lastModal() {
  const all = document.querySelectorAll('.modal-backdrop');
  return all.length ? all[all.length - 1] : null;
}

export function showBatchErrors(errors, title) {
  if (!errors || !errors.length) return;
  const rows = errors.map((e) => {
    const label = e.paper_id || e.file_path || e.title || 'item';
    return `<div class="verify-row"><span class="mono small">${U.esc(label)}</span>` +
      `<span class="spacer"></span><span class="small text-err">${U.esc(e.error || 'failed')}</span></div>`;
  }).join('');
  U.modal({ title: title || 'Some items failed', body: rows });
}

// ---- library view ----------------------------------------------------------

// Filter/sort/page state survives navigation (restored on re-entry).
const libState = {
  status: '', sort_by: 'updated_at', sort_order: 'desc',
  paper_type: '', species: '', gene: '', technique: '', pathway: '',
  keyword: '',
  offset: 0
};

// Annotations cache: paperId -> { rating: number|null, commentCount: number }.
// Seeded from the list response on every load() (which also prunes stale
// ids); mutated locally by the inline star / notes controls.
const annoCache = {};

function libParams() {
  return {
    limit: PAGE_SIZE, offset: libState.offset,
    status: libState.status, sort_by: libState.sort_by, sort_order: libState.sort_order,
    paper_type: libState.paper_type, species: libState.species,
    gene: libState.gene, technique: libState.technique, pathway: libState.pathway,
    keyword: libState.keyword
  };
}

function libHasFilters() {
  return !!(libState.status || libState.paper_type || libState.keyword ||
    libState.species || libState.gene || libState.technique || libState.pathway);
}

function renderLibrary(container, ctx) {
  if (ctx.query.get('status')) libState.status = ctx.query.get('status');
  if (ctx.query.get('keyword')) libState.keyword = ctx.query.get('keyword');

  let pollTimer = null;
  let selected = {}; // paper id -> true

  container.innerHTML = `
    <div class="page-head">
      <div class="page-sub" id="lib-sub" role="status">Loading…</div>
    </div>
    <div class="toolbar">
      <label class="visually-hidden" for="lib-status">Filter by status</label>
      <select class="select select-auto" id="lib-status">
        <option value="">All statuses</option>
        <option value="indexed">Indexed</option>
        <option value="processing">Processing</option>
        <option value="failed">Failed</option>
      </select>
      <label class="visually-hidden" for="lib-sort">Sort by</label>
      <select class="select select-auto" id="lib-sort">
        <option value="updated_at">Recently updated</option>
        <option value="title">Title</option>
        <option value="published_date">Publication date</option>
      </select>
      <button type="button" class="btn ghost sm" id="lib-order"></button>
      <span class="spacer"></span>
      <button type="button" class="btn primary" id="lib-add">${U.icon('upload', 15)} Add paper</button>
    </div>
    <div class="filter-bar mt-3 mb-1">
      <details id="lib-filters">
        <summary class="btn ghost sm summary-btn">${U.icon('list', 14)} Filters
          <span class="badge accent hidden" id="lib-filter-count"></span></summary>
        <div class="grid cols-3 mt-3">
          <div class="field"><label for="lib-f-paper-type">Paper type</label>
            <input class="input" id="lib-f-paper-type" data-f="paper_type" placeholder="e.g. journal-article"></div>
          <div class="field"><label for="lib-f-keyword">Keyword</label>
            <input class="input" id="lib-f-keyword" data-f="keyword" placeholder="e.g. CRISPR"></div>
          <div class="field"><label for="lib-f-species">Species</label>
            <input class="input" id="lib-f-species" data-f="species" placeholder="e.g. Oryza sativa"></div>
          <div class="field"><label for="lib-f-gene">Gene</label>
            <input class="input" id="lib-f-gene" data-f="gene" placeholder="e.g. BRCA1"></div>
          <div class="field"><label for="lib-f-technique">Technique</label>
            <input class="input" id="lib-f-technique" data-f="technique" placeholder="e.g. ChIP-seq"></div>
          <div class="field"><label for="lib-f-pathway">Pathway</label>
            <input class="input" id="lib-f-pathway" data-f="pathway" placeholder="e.g. MAPK"></div>
        </div>
      </details>
      <button type="button" class="btn subtle sm hidden" id="lib-clear-filters">${U.icon('x', 13)} Clear filters</button>
    </div>
    <div id="lib-list" class="mt-4">${U.spinner('Loading papers…')}</div>
    <div class="pagination" id="lib-pager"></div>
    <div class="save-bar hidden" id="lib-selbar"></div>`;

  const listEl = container.querySelector('#lib-list');
  const pagerEl = container.querySelector('#lib-pager');
  const selbarEl = container.querySelector('#lib-selbar');
  const statusSel = container.querySelector('#lib-status');
  const sortSel = container.querySelector('#lib-sort');
  const orderBtn = container.querySelector('#lib-order');

  statusSel.value = libState.status;
  sortSel.value = libState.sort_by;
  paintOrder();

  function paintOrder() {
    orderBtn.innerHTML = (libState.sort_order === 'desc' ? '↓ Desc' : '↑ Asc');
    orderBtn.title = 'Toggle sort order';
  }

  // Number of active entity/keyword filters inside the disclosure (the status
  // dropdown is always visible, so it is not counted in the hidden badge).
  function activeFilterCount() {
    let n = 0;
    container.querySelectorAll('#lib-filters [data-f]').forEach((inp) => {
      if (libState[inp.getAttribute('data-f')]) n++;
    });
    return n;
  }

  // Keep the Filters badge, the clear-filters button and the disclosure's
  // open state in sync with libState.
  function paintFilterBar() {
    const n = activeFilterCount();
    const badge = container.querySelector('#lib-filter-count');
    const clearBtn = container.querySelector('#lib-clear-filters');
    const details = container.querySelector('#lib-filters');
    if (badge) {
      badge.textContent = String(n);
      badge.classList.toggle('hidden', n === 0);
    }
    if (clearBtn) clearBtn.classList.toggle('hidden', !libHasFilters());
    // Reveal active filters so the user can see what narrows the list.
    if (details && n > 0) details.open = true;
  }

  function clearAllFilters() {
    libState.status = ''; libState.paper_type = ''; libState.keyword = '';
    libState.species = ''; libState.gene = ''; libState.technique = ''; libState.pathway = '';
    libState.offset = 0;
    statusSel.value = '';
    container.querySelectorAll('#lib-filters [data-f]').forEach((inp) => { inp.value = ''; });
    const details = container.querySelector('#lib-filters');
    if (details) details.open = false;
    paintFilterBar();
  }

  const selectedIds = () => Object.keys(selected).filter((k) => selected[k]);

  function paintSelbar() {
    const ids = selectedIds();
    if (!ids.length) {
      selbarEl.classList.add('hidden');
      selbarEl.innerHTML = '';
      return;
    }
    selbarEl.classList.remove('hidden');
    selbarEl.innerHTML =
      `<strong>${ids.length} selected</strong><span class="spacer"></span>` +
      '<button type="button" class="btn ghost sm" data-sel="reindex">Reindex sections</button>' +
      `<button type="button" class="btn danger sm" data-sel="delete">${U.icon('trash', 14)} Delete</button>` +
      '<button type="button" class="btn subtle sm" data-sel="clear">Clear</button>';
    selbarEl.querySelector('[data-sel="clear"]').addEventListener('click', () => {
      selected = {};
      container.querySelectorAll('[data-check]').forEach((cb) => {
        cb.checked = false;
        cb.closest('.paper-row').classList.remove('selected');
      });
      paintSelbar();
    });
    selbarEl.querySelector('[data-sel="reindex"]').addEventListener('click', () => {
      API.batchReindexSections(ids).then((res) => {
        const ok = (res.success || []).length;
        U.toast('Section reindex queued for ' + ok + (ok === 1 ? ' paper' : ' papers'),
          ok ? 'success' : 'info');
        showBatchErrors(res.errors, 'Reindex failures');
        selected = {};
        load();
      }).catch(U.handleApiError);
    });
    selbarEl.querySelector('[data-sel="delete"]').addEventListener('click', () => {
      U.confirm({
        title: 'Delete ' + ids.length + (ids.length === 1 ? ' paper?' : ' papers?'),
        body: 'This removes the selected papers and their indexed data. The source files are not touched.',
        confirmLabel: 'Delete', danger: true
      }).then((ok) => {
        if (!ok) return;
        API.batchDelete(ids).then((res) => {
          const n = (res.success || []).length;
          U.toast('Deleted ' + n + (n === 1 ? ' paper' : ' papers'), n ? 'success' : 'info');
          showBatchErrors(res.errors, 'Delete failures');
          selected = {};
          (res.success || []).forEach((pid) => { delete annoCache[pid]; });
          load();
        }).catch(U.handleApiError);
      });
    });
  }

  function inlineStarsHTML(pid) {
    const r = annoCache[pid]?.rating;
    let h = '<div class="inline-stars" role="radiogroup" aria-label="Rate">';
    for (let n = 1; n <= 5; n++) {
      const on = r != null && n <= r;
      h += '<button type="button" class="star-btn' + (on ? ' active' : '') +
        '" data-star="' + n + '" role="radio" aria-checked="' + on + '"' +
        ' title="' + (r === n ? 'Click to clear' : 'Rate ' + n) + '">' +
        U.icon('star', 14) + '</button>';
    }
    return h + '</div>';
  }

  function notesBtnHTML(pid) {
    const cnt = annoCache[pid]?.commentCount || 0;
    return '<button type="button" class="notes-btn icon-btn" data-notes' +
      ' title="Notes" aria-label="Notes">' +
      U.icon('ask', 15) +
      (cnt > 0 ? '<span class="notes-badge">' + cnt + '</span>' : '') +
      '</button>';
  }

  function rowHTML(p) {
    let tags = U.badge(p.status || 'unknown', U.statusTone(p.status));
    if (p.source && p.source !== 'manual') tags += U.badge(p.source, 'accent');
    tags += entityChips(p, 6);
    return `<div class="paper-row${selected[p.id] ? ' selected' : ''}" data-id="${U.esc(p.id)}">` +
      `<input type="checkbox" class="row-check" data-check="${U.esc(p.id)}"` +
      `${selected[p.id] ? ' checked' : ''} aria-label="Select paper">` +
      `<img class="paper-cover" data-cover src="${U.coverUrl(p.id, true)}" alt="" loading="lazy">` +
      '<div class="paper-row-main">' +
      `<div class="paper-title"><a href="#/paper/${encodeURIComponent(p.id)}">` +
      `${U.esc(p.title || 'Untitled')}</a></div>` +
      `<div class="paper-meta">${U.paperLine(p)}</div>` +
      (p.abstract_text
        ? `<div class="paper-abstract">${U.esc(U.truncate(p.abstract_text, 280))}</div>`
        : '') +
      `<div class="paper-tags">${tags}</div>` +
      '</div>' +
      '<div class="paper-row-side">' +
      `<span class="muted small nowrap">${U.esc(U.relTime(p.updated_at))}</span>` +
      inlineStarsHTML(p.id) +
      '<div class="row-sm">' +
      notesBtnHTML(p.id) +
      `<button type="button" class="icon-btn" data-reindex title="Reindex" aria-label="Reindex">` +
      `${U.icon('refresh', 15)}</button>` +
      `<button type="button" class="icon-btn" data-del title="Delete" aria-label="Delete">` +
      `${U.icon('trash', 15)}</button>` +
      '</div></div></div>';
  }

  function wireRow(row) {
    const id = row.getAttribute('data-id');
    const img = row.querySelector('img[data-cover]');
    if (img) {
      wireCoverFallback(img);
      img.addEventListener('click', (e) => { e.stopPropagation(); U.openCoverLightbox(id); });
    }
    row.querySelector('[data-check]').addEventListener('change', (e) => {
      selected[id] = e.target.checked;
      if (!e.target.checked) delete selected[id];
      row.classList.toggle('selected', e.target.checked);
      paintSelbar();
    });
    // Inline star rating
    row.querySelectorAll('.inline-stars [data-star]').forEach((b) => {
      b.addEventListener('click', () => {
        const n = parseInt(b.getAttribute('data-star'), 10);
        const cur = annoCache[id]?.rating;
        const target = cur === n ? null : n;
        const req = target == null ? API.clearRating(id) : API.setRating(id, target);
        req.then(() => {
          if (!annoCache[id]) annoCache[id] = {};
          annoCache[id].rating = target;
          const starsEl = row.querySelector('.inline-stars');
          starsEl.querySelectorAll('[data-star]').forEach((sb) => {
            const sn = parseInt(sb.getAttribute('data-star'), 10);
            const on = target != null && sn <= target;
            sb.classList.toggle('active', on);
            sb.setAttribute('aria-checked', String(on));
          });
          U.toast(target == null ? 'Rating cleared'
            : 'Rated ' + target + ' star' + (target > 1 ? 's' : ''), 'success');
        }).catch(U.handleApiError);
      });
    });
    // Notes popover
    const notesBtn = row.querySelector('[data-notes]');
    if (notesBtn) {
      notesBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        toggleNotesPopover(id, notesBtn);
      });
    }
    row.querySelector('[data-reindex]').addEventListener('click', () => {
      API.reindex(id).then(() => {
        U.toast('Reindex queued', 'success');
        load();
      }).catch(U.handleApiError);
    });
    row.querySelector('[data-del]').addEventListener('click', () => {
      U.confirm({
        title: 'Delete this paper?',
        body: 'This removes the paper and its indexed data. The source file is not touched.',
        confirmLabel: 'Delete', danger: true
      }).then((ok) => {
        if (!ok) return;
        API.deletePaper(id).then(() => {
          U.toast('Paper deleted', 'success');
          delete selected[id];
          delete annoCache[id];
          closeNotesPopover();
          row.remove();
          paintSelbar();
        }).catch(U.handleApiError);
      });
    });
  }

  function paintPager(total, count) {
    if (!total) {
      pagerEl.innerHTML = '';
      return;
    }
    const from = libState.offset + 1;
    const to = libState.offset + count;
    pagerEl.innerHTML =
      '<button type="button" class="btn ghost sm" data-page="prev"' +
      (libState.offset === 0 ? ' disabled' : '') + '>← Prev</button>' +
      `<span class="page-info">Showing ${U.fmtInt(from)}–${U.fmtInt(to)}` +
      ` of ${U.fmtInt(total)}</span>` +
      '<button type="button" class="btn ghost sm" data-page="next"' +
      (to >= total ? ' disabled' : '') + '>Next →</button>';
    pagerEl.querySelector('[data-page="prev"]').addEventListener('click', () => {
      libState.offset = Math.max(0, libState.offset - PAGE_SIZE);
      load();
    });
    pagerEl.querySelector('[data-page="next"]').addEventListener('click', () => {
      libState.offset += PAGE_SIZE;
      load();
    });
  }

  async function load() {
    clearTimeout(pollTimer);
    try {
      const res = await API.listPapers(libParams());
      const papers = res.papers || [];
      // The list response carries rating/comment_count per entry — seed the
      // annotation cache from it instead of per-row requests, and drop cache
      // entries for papers no longer on this page (deleted, filtered out).
      const keep = new Set();
      papers.forEach((p) => {
        keep.add(p.id);
        annoCache[p.id] = { rating: p.rating ?? null, commentCount: p.comment_count || 0 };
      });
      Object.keys(annoCache).forEach((k) => {
        if (!keep.has(k)) delete annoCache[k];
      });
      closeNotesPopover(); // rows are about to be rebuilt; anchors go stale
      const sub = container.querySelector('#lib-sub');
      if (sub) {
        sub.textContent = U.fmtInt(res.total || 0) +
          ((res.total || 0) === 1 ? ' paper' : ' papers') +
          (libHasFilters() ? ' · filters active' : '');
      }
      if (!papers.length) {
        if (libHasFilters()) {
          listEl.innerHTML = U.emptyState({
            icon: 'search',
            title: 'No papers match these filters',
            body: 'Try broadening the status or entity filters.',
            action: '<button type="button" class="btn ghost" data-clear-filters>Clear filters</button>'
          });
          listEl.querySelector('[data-clear-filters]').addEventListener('click', () => {
            clearAllFilters();
            load();
          });
        } else {
          listEl.innerHTML = U.emptyState({
            icon: 'library',
            title: 'Your library is empty',
            body: 'Add a paper by file path or upload, or sync an existing Zotero / Lattice library.',
            action: '<button type="button" class="btn primary" data-open-add>Add a paper</button> ' +
              '<a class="btn ghost" href="#/sync">Set up sync</a>'
          });
          listEl.querySelector('[data-open-add]').addEventListener('click', () => openAddModal(load));
        }
      } else {
        listEl.innerHTML = papers.map(rowHTML).join('');
        listEl.querySelectorAll('.paper-row').forEach(wireRow);
      }
      paintPager(res.total || 0, papers.length);
      // Keep polling while anything is still processing.
      if (papers.some((p) => p.status === 'processing')) {
        pollTimer = setTimeout(load, POLL_MS);
      }
    } catch (e) {
      listEl.innerHTML = U.errorState(e.message, 'Retry');
      listEl.querySelector('[data-retry]').addEventListener('click', load);
    }
  }

  // ---- inline annotations (rating + notes) ----

  let activeNotesPopover = null;

  function closeNotesPopover() {
    if (activeNotesPopover) {
      activeNotesPopover.remove();
      activeNotesPopover = null;
      document.removeEventListener('mousedown', onDocClickCloseNotes);
      document.removeEventListener('keydown', onEscCloseNotes);
    }
  }

  function toggleNotesPopover(paperId, anchor) {
    if (activeNotesPopover) {
      const wasFor = activeNotesPopover.getAttribute('data-for');
      closeNotesPopover();
      if (wasFor === paperId) return;
    }
    const pop = document.createElement('div');
    pop.className = 'notes-popover card';
    pop.setAttribute('data-for', paperId);
    pop.innerHTML = '<div class="notes-pop-head">' +
      '<strong>Notes</strong>' +
      '<button type="button" class="icon-btn" data-pop-close aria-label="Close">' +
      U.icon('x', 14) + '</button></div>' +
      '<div class="notes-pop-body">' + U.spinner('Loading…') + '</div>';
    document.body.appendChild(pop);
    const ar = anchor.getBoundingClientRect();
    // Absolutely positioned against the initial containing block: top needs
    // scrollY, but `right` is measured from the viewport's right edge — no
    // scrollX term (subtracting it drifted the popover on horizontal scroll).
    pop.style.top = (ar.bottom + window.scrollY + 6) + 'px';
    pop.style.right = (window.innerWidth - ar.right) + 'px';
    activeNotesPopover = pop;
    pop.querySelector('[data-pop-close]').addEventListener('click', closeNotesPopover);
    setTimeout(() => {
      document.addEventListener('mousedown', onDocClickCloseNotes);
      document.addEventListener('keydown', onEscCloseNotes);
    }, 0);
    refreshNotesBody(paperId);
  }

  function onDocClickCloseNotes(e) {
    if (activeNotesPopover && !activeNotesPopover.contains(e.target)
      && !e.target.closest('[data-notes]')) {
      closeNotesPopover();
    }
  }

  function onEscCloseNotes(e) {
    if (e.key === 'Escape') closeNotesPopover();
  }

  function refreshNotesBody(paperId) {
    if (!activeNotesPopover) return;
    const body = activeNotesPopover.querySelector('.notes-pop-body');
    body.innerHTML = U.spinner('Loading…');
    Promise.all([
      API.listComments(paperId).catch(() => []),
    ]).then(([comments]) => {
      if (!activeNotesPopover) return;
      const list = comments.length
        ? comments.map((c) =>
          '<div class="comment" data-cid="' + U.esc(String(c.id)) + '">' +
          '<div class="comment-head">' +
          '<span class="muted small">' + U.esc(U.fmtDateTime(c.created_at)) + '</span>' +
          '<span class="spacer"></span>' +
          '<button type="button" class="icon-btn" data-del-c title="Delete" aria-label="Delete">' +
          U.icon('trash', 13) + '</button></div>' +
          '<div class="comment-body">' + U.esc(c.content) + '</div></div>'
        ).join('')
        : '<p class="muted small">No notes yet.</p>';
      body.innerHTML =
        '<div class="comment-list">' + list + '</div>' +
        '<div class="comment-form mt-2">' +
        '<textarea class="textarea" data-note-input rows="2" placeholder="Add a note…"></textarea>' +
        '<div class="row mt-1"><span class="spacer"></span>' +
        '<button type="button" class="btn primary sm" data-add-note>Add</button></div></div>';
      body.querySelectorAll('[data-del-c]').forEach((btn) => {
        btn.addEventListener('click', () => {
          const cid = btn.closest('[data-cid]').getAttribute('data-cid');
          API.deleteComment(paperId, cid).then(() => {
            U.toast('Note deleted', 'success');
            if (annoCache[paperId]) annoCache[paperId].commentCount = Math.max(0, (annoCache[paperId].commentCount || 1) - 1);
            updateNotesBadge(paperId);
            refreshNotesBody(paperId);
          }).catch(U.handleApiError);
        });
      });
      body.querySelector('[data-add-note]').addEventListener('click', () => {
        const inp = body.querySelector('[data-note-input]');
        const content = (inp.value || '').trim();
        if (!content) return;
        API.addComment(paperId, content).then(() => {
          U.toast('Note added', 'success');
          if (!annoCache[paperId]) annoCache[paperId] = {};
          annoCache[paperId].commentCount = (annoCache[paperId].commentCount || 0) + 1;
          updateNotesBadge(paperId);
          inp.value = '';
          refreshNotesBody(paperId);
        }).catch(U.handleApiError);
      });
    }).catch(() => {
      if (!activeNotesPopover) return;
      body.innerHTML = '<p class="small text-err">Failed to load notes.</p>';
    });
  }

  function updateNotesBadge(paperId) {
    const row = listEl.querySelector('.paper-row[data-id="' + CSS.escape(paperId) + '"]');
    if (!row) return;
    const btn = row.querySelector('[data-notes]');
    if (!btn) return;
    const old = btn.querySelector('.notes-badge');
    if (old) old.remove();
    const cnt = annoCache[paperId]?.commentCount || 0;
    if (cnt > 0) {
      btn.insertAdjacentHTML('beforeend', '<span class="notes-badge">' + cnt + '</span>');
    }
  }

  // ---- toolbar wiring ----

  container.querySelector('#lib-add').addEventListener('click', () => openAddModal(load));
  container.querySelector('#lib-clear-filters').addEventListener('click', () => {
    clearAllFilters();
    load();
  });
  statusSel.addEventListener('change', () => {
    libState.status = statusSel.value;
    libState.offset = 0;
    paintFilterBar();
    load();
  });
  sortSel.addEventListener('change', () => {
    libState.sort_by = sortSel.value;
    libState.offset = 0;
    load();
  });
  orderBtn.addEventListener('click', () => {
    libState.sort_order = libState.sort_order === 'desc' ? 'asc' : 'desc';
    paintOrder();
    load();
  });
  const applyFilters = U.debounce(() => {
    container.querySelectorAll('#lib-filters [data-f]').forEach((inp) => {
      libState[inp.getAttribute('data-f')] = inp.value.trim();
    });
    libState.offset = 0;
    paintFilterBar();
    load();
  }, 400);
  container.querySelectorAll('#lib-filters [data-f]').forEach((inp) => {
    inp.value = libState[inp.getAttribute('data-f')] || '';
    inp.addEventListener('input', applyFilters);
  });

  paintFilterBar();
  load();

  // The notes popover is anchored to a row; scrolling would leave it
  // dangling, so close it instead of tracking the anchor.
  window.addEventListener('scroll', closeNotesPopover, { passive: true });

  return function destroy() {
    clearTimeout(pollTimer);
    window.removeEventListener('scroll', closeNotesPopover);
    closeNotesPopover();
  };
}

route('/library', { title: 'Papers', render: renderLibrary });
