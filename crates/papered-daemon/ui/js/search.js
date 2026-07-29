// Search view: paper (chunk) search and figure search over the library.
// Registered as route('/search'). Last query/options/results are kept in
// module state so navigating away and back restores the view without
// re-fetching.

import * as U from './util.js?v=1';
import { API } from './api.js?v=1';
import { route } from './router.js?v=1';

// ---- module state (restored on re-entry) --------------------------------

const state = {
  query: '',
  method: 'hybrid',
  section: '',
  minScore: 0,
  limit: 10,
  tab: 'papers',
  // Cached results, each tagged with the options key that produced them.
  papers: null, // { key, results, elapsed }
  figures: null, // { key, results, elapsed }
  passages: null, // { key, results, elapsed }
  searched: false
};

const METHODS = [
  ['hybrid', 'Hybrid'],
  ['semantic', 'Semantic'],
  ['fulltext', 'Full-text']
];

const SECTIONS = [
  ['', 'All sections'],
  ['abstract', 'Abstract'],
  ['introduction', 'Introduction'],
  ['methods', 'Methods'],
  ['results', 'Results'],
  ['discussion', 'Discussion'],
  ['conclusion', 'Conclusion']
];

let reqSeq = 0; // stale-request guard

// ---- option helpers ------------------------------------------------------

function optionsKey() {
  return [state.query, state.method, state.section, state.minScore, state.limit].join('');
}

function searchBody() {
  const body = { query: state.query, limit: state.limit };
  if (state.method !== 'hybrid') body.search_method = state.method;
  if (state.section) body.section_type = state.section;
  if (state.minScore > 0) body.min_score = state.minScore;
  return body;
}

function figureBody() {
  const body = { query: state.query, limit: state.limit };
  if (state.minScore > 0) body.min_score = state.minScore;
  return body;
}

// Passages use pure lexical (BM25) matching — min_score does not apply.
function passageBody() {
  return { query: state.query, limit: state.limit };
}

// ---- static HTML ---------------------------------------------------------

function panelHTML() {
  const methodBtns = METHODS.map(([value, label]) =>
    '<button type="button" data-method="' + value + '"' +
    (state.method === value ? ' class="active" aria-pressed="true"' : ' aria-pressed="false"') +
    '>' + label + '</button>'
  ).join('');
  const sectionOpts = SECTIONS.map(([value, label]) =>
    '<option value="' + value + '"' + (state.section === value ? ' selected' : '') +
    '>' + label + '</option>'
  ).join('');
  const limitOpts = [10, 20, 50].map((n) =>
    '<option value="' + n + '"' + (state.limit === n ? ' selected' : '') + '>' + n + '</option>'
  ).join('');

  return '<div class="card">' +
    '<div class="search-box lg">' + U.icon('search', 18) +
    '<input class="input" id="search-input" aria-label="Search papers, sections and figures" ' +
    'placeholder="Search papers, sections and figures…" value="' + U.esc(state.query) + '">' +
    '</div>' +
    '<div class="toolbar mt-3">' +
    '<div class="segmented" id="search-method" role="group" aria-label="Search method">' + methodBtns + '</div>' +
    '<label class="visually-hidden" for="search-section">Section type</label>' +
    '<select class="select select-auto" id="search-section" title="Section type">' + sectionOpts + '</select>' +
    '<span class="row">' +
    '<label class="muted small nowrap" for="search-min-score">Min score <span id="min-score-val">' +
    state.minScore.toFixed(2) + '</span></label>' +
    '<input type="range" id="search-min-score" min="0" max="0.9" step="0.05" value="' + state.minScore + '">' +
    '</span>' +
    '<label class="visually-hidden" for="search-limit">Result limit</label>' +
    '<select class="select select-auto" id="search-limit" title="Result limit">' + limitOpts + '</select>' +
    '<span class="spacer"></span>' +
    '<button type="button" class="btn primary" id="search-btn">' + U.icon('search', 15) + ' Search</button>' +
    '</div></div>';
}

function tabsHTML() {
  const tabBtn = (id, label, countId) =>
    '<button type="button" role="tab" data-tab="' + id + '" aria-selected="' + (state.tab === id) + '"' +
    (state.tab === id ? ' class="active"' : '') + '>' + label +
    ' <span class="tab-count" id="' + countId + '"></span></button>';
  return '<div class="tabs mt-4" id="search-tabs" role="tablist" aria-label="Search result type">' +
    tabBtn('papers', 'Papers', 'papers-count') +
    tabBtn('passages', 'Passages', 'passages-count') +
    tabBtn('figures', 'Figures', 'figures-count') + '</div>';
}

// ---- result rendering ----------------------------------------------------

function countLine(n, elapsed) {
  let line = U.fmtInt(n) + (n === 1 ? ' result' : ' results');
  if (elapsed != null) line += ' in ' + (elapsed / 1000).toFixed(2) + ' s';
  return '<p class="muted small mt-0 mb-3">' + line + '</p>';
}

function snippetHTML(sec, query) {
  return '<div class="snippet">' +
    U.badge(sec.section_type || 'section', 'accent') + ' ' +
    U.highlight(U.esc(sec.content_snippet || ''), query) + '</div>';
}

function paperRowHTML(r, query) {
  const p = r.paper || {};
  const pct = U.scorePct(r.score);
  const sections = r.matched_sections || [];
  const shown = sections.slice(0, 3).map((s) => snippetHTML(s, query)).join('');
  let more = '';
  if (sections.length > 3) {
    more = '<div class="hidden" data-more-snippets>' +
      sections.slice(3).map((s) => snippetHTML(s, query)).join('') + '</div>' +
      '<button type="button" class="btn subtle sm mt-1" data-show-more>' +
      U.icon('chevronDown', 13) + ' Show ' + (sections.length - 3) + ' more</button>';
  }
  return '<div class="paper-row" data-id="' + encodeURIComponent(p.id) + '">' +
    '<div class="paper-cover">' + U.paperCoverImg(p, { extraAttrs: 'data-cover' }) + '</div>' +
    '<div class="paper-row-main">' +
    U.paperLink(p, { cls: 'paper-title' }) +
    '<div class="paper-meta">' + U.paperLine(p) + '</div>' +
    shown + more +
    '</div>' +
    '<div class="paper-row-side">' +
    '<div class="score-bar"><div class="progress" role="progressbar" aria-label="Relevance score" ' +
    'aria-valuenow="' + pct + '" aria-valuemin="0" aria-valuemax="100">' +
    '<div data-bar data-pct="' + pct + '"></div></div>' +
    '<span class="score-num">' + pct + '%</span></div>' +
    '</div></div>';
}

function renderPaperResults(el, entry) {
  const results = entry.results || [];
  if (!results.length) {
    el.innerHTML = U.emptyState({
      icon: 'search',
      title: 'No matching papers',
      body: 'Try lowering the minimum score, widening the section filter, or switching to full-text search.'
    });
    return;
  }
  el.innerHTML = countLine(results.length, entry.elapsed) +
    results.map((r) => paperRowHTML(r, state.query)).join('');
  // Dynamic value, not layout: bar widths come from the per-row score.
  el.querySelectorAll('[data-bar]').forEach((bar) => {
    bar.style.width = bar.getAttribute('data-pct') + '%';
  });
  wireCovers(el);
  el.querySelectorAll('[data-show-more]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const more = btn.parentNode.querySelector('[data-more-snippets]');
      if (more) more.classList.remove('hidden');
      btn.remove();
    });
  });
}

function figureCardHTML(r, i) {
  const p = r.paper || {};
  const f = r.figure || {};
  const pct = U.scorePct(r.score);
  const caption = U.formatFigureLabel(f, i);
  return '<div class="figure-card">' +
    '<div class="figure-media" data-figure>' +
    '<img src="' + U.figureImageUrl(p.id, f.id) + '" alt="' + U.esc(U.truncate(caption, 80)) + '" loading="lazy">' +
    '</div>' +
    '<div class="figure-caption">' + U.highlight(U.esc(U.truncate(caption, 140)), state.query) + '</div>' +
    '<div class="paper-meta row">' +
    U.paperLink(p, { maxLen: 40, titleAttr: true }) +
    '<span class="spacer"></span>' + U.badge(pct + '%', 'accent') + '</div></div>';
}

function renderFigureResults(el, entry) {
  const results = entry.results || [];
  if (!results.length) {
    el.innerHTML = U.emptyState({
      icon: 'image',
      title: 'No matching figures',
      body: 'Try lowering the minimum score or rephrasing the query — figure search matches captions and descriptions.'
    });
    return;
  }
  el.innerHTML = countLine(results.length, entry.elapsed) +
    '<div class="figure-grid">' + results.map(figureCardHTML).join('') + '</div>';
  // Lightbox on image click.
  el.querySelectorAll('[data-figure]').forEach((media, i) => {
    media.addEventListener('click', () => {
      const p = results[i].paper || {};
      const f = results[i].figure || {};
      U.openLightbox({
        src: U.figureImageUrl(p.id, f.id),
        caption: f.caption,
        description: f.description,
        page: f.page_number,
        label: 'Figure'
      });
    });
  });
}

// ---- passage (verbatim source-text) results --------------------------------

function passageCardHTML(r) {
  const p = r.paper || {};
  const c = r.chunk || {};
  const pct = U.scorePct(r.score);
  const page = c.page_number != null ? 'p. ' + c.page_number : '';
  const chunkLink = '#/paper/' + encodeURIComponent(p.id) + '/chunk/' + encodeURIComponent(c.id);
  return '<div class="passage-card">' +
    '<div class="passage-head">' +
    U.badge(c.chunk_type || 'passage', 'accent') +
    (page ? '<span class="muted small nowrap">' + page + '</span>' : '') +
    '<span class="spacer"></span>' + U.badge(pct + '%', 'muted') + '</div>' +
    '<blockquote class="passage-text">' + U.highlight(U.esc(U.cleanVerbatim(c.content)), state.query) + '</blockquote>' +
    '<div class="passage-foot paper-meta">' +
    '<a href="' + chunkLink + '" title="' + U.esc(p.title || '') + '">' +
    U.esc(U.truncate(p.title || 'Untitled', 60)) + '</a>' +
    '<span class="spacer"></span>' +
    '<a class="small" href="' + chunkLink + '">View in context →</a>' +
    '</div></div>';
}

function renderPassageResults(el, entry) {
  const results = entry.results || [];
  if (!results.length) {
    el.innerHTML = U.emptyState({
      icon: 'file',
      title: 'No matching passages',
      body: 'Passage search matches the exact words in the original document text. ' +
        'Try keywords, names or phrases that are likely to appear verbatim.'
    });
    return;
  }
  el.innerHTML = countLine(results.length, entry.elapsed) +
    '<div class="passage-list">' + results.map(passageCardHTML).join('') + '</div>';
}

// Broken cover images collapse into a placeholder.
function wireCovers(el) {
  el.querySelectorAll('img[data-cover]').forEach((img) => {
    const paperId = img.closest('[data-id]')?.getAttribute('data-id');
    if (paperId) {
      img.addEventListener('click', () => U.openCoverLightbox(paperId));
    }
    img.addEventListener('error', () => {
      const ph = document.createElement('div');
      ph.className = 'paper-cover placeholder';
      ph.innerHTML = U.icon('file', 20);
      img.parentNode.replaceChild(ph, img);
    });
  });
}

// ---- empty states ----------------------------------------------------------

function welcomeHTML() {
  return U.emptyState({
    icon: 'search',
    title: 'Search your library',
    body: 'Hybrid combines semantic embeddings with keyword matching and works best for most questions. ' +
      'Semantic finds conceptually related passages even without shared words; ' +
      'full-text is exact keyword matching for names, symbols and phrases.'
  });
}

// ---- search execution ------------------------------------------------------

async function runSearch(container) {
  const query = state.query.trim();
  const el = container.querySelector('#search-results');
  if (!el) return;
  if (!query) {
    state.searched = false;
    state.papers = state.figures = state.passages = null;
    paintCounts(container);
    el.innerHTML = welcomeHTML();
    return;
  }
  state.searched = true;
  const key = optionsKey();
  const tab = state.tab;
  const seq = ++reqSeq;
  el.innerHTML = U.spinner('Searching…');
  const started = (typeof performance !== 'undefined' && performance.now) ? performance.now() : null;
  try {
    const results = await (tab === 'figures' ? API.searchFigures(figureBody())
      : tab === 'passages' ? API.searchPassages(passageBody())
      : API.search(searchBody()));
    if (seq !== reqSeq) return; // superseded or view destroyed
    const elapsed = started != null ? Math.round(performance.now() - started) : null;
    const entry = { key, results: results || [], elapsed };
    if (tab === 'figures') state.figures = entry;
    else if (tab === 'passages') state.passages = entry;
    else state.papers = entry;
    paintCounts(container);
    paintResults(container);
  } catch (err) {
    if (seq !== reqSeq) return;
    el.innerHTML = U.errorState(err.message || 'Search failed', 'Retry');
    const retry = el.querySelector('[data-retry]');
    if (retry) retry.addEventListener('click', () => runSearch(container));
  }
}

function paintCounts(container) {
  const pc = container.querySelector('#papers-count');
  const gc = container.querySelector('#passages-count');
  const fc = container.querySelector('#figures-count');
  if (pc) pc.textContent = state.papers && state.papers.key === optionsKey()
    ? String(state.papers.results.length) : '';
  if (gc) gc.textContent = state.passages && state.passages.key === optionsKey()
    ? String(state.passages.results.length) : '';
  if (fc) fc.textContent = state.figures && state.figures.key === optionsKey()
    ? String(state.figures.results.length) : '';
}

function paintResults(container) {
  const el = container.querySelector('#search-results');
  if (!el) return;
  if (!state.searched) {
    el.innerHTML = welcomeHTML();
    return;
  }
  const entry = state.tab === 'figures' ? state.figures
    : state.tab === 'passages' ? state.passages : state.papers;
  if (entry && entry.key === optionsKey()) {
    if (state.tab === 'figures') renderFigureResults(el, entry);
    else if (state.tab === 'passages') renderPassageResults(el, entry);
    else renderPaperResults(el, entry);
  } else {
    // Options changed since this tab was last searched — fetch fresh.
    runSearch(container);
  }
}

// ---- wiring ----------------------------------------------------------------

function wireControls(container) {
  const input = container.querySelector('#search-input');
  const methodBox = container.querySelector('#search-method');
  const sectionSel = container.querySelector('#search-section');
  const scoreRange = container.querySelector('#search-min-score');
  const scoreVal = container.querySelector('#min-score-val');
  const limitSel = container.querySelector('#search-limit');
  const searchBtn = container.querySelector('#search-btn');

  function submit() {
    state.query = input.value;
    runSearch(container);
  }

  searchBtn.addEventListener('click', submit);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') submit();
  });

  methodBox.querySelectorAll('button').forEach((btn) => {
    btn.addEventListener('click', () => {
      state.method = btn.getAttribute('data-method');
      methodBox.querySelectorAll('button').forEach((b) => {
        const on = b === btn;
        b.classList.toggle('active', on);
        b.setAttribute('aria-pressed', String(on));
      });
      if (state.searched) submit();
    });
  });

  sectionSel.addEventListener('change', () => {
    state.section = sectionSel.value;
    if (state.searched) submit();
  });

  scoreRange.addEventListener('input', () => {
    state.minScore = Number(scoreRange.value) || 0;
    scoreVal.textContent = state.minScore.toFixed(2);
  });
  scoreRange.addEventListener('change', () => {
    if (state.searched) submit();
  });

  limitSel.addEventListener('change', () => {
    state.limit = Number(limitSel.value) || 10;
    if (state.searched) submit();
  });

  container.querySelectorAll('#search-tabs button').forEach((btn) => {
    btn.addEventListener('click', () => {
      const tab = btn.getAttribute('data-tab');
      if (tab === state.tab) return;
      state.tab = tab;
      container.querySelectorAll('#search-tabs button').forEach((b) => {
        const on = b === btn;
        b.classList.toggle('active', on);
        b.setAttribute('aria-selected', String(on));
      });
      paintResults(container);
    });
  });
}

// ---- route -----------------------------------------------------------------

route('/search', {
  title: 'Search',
  render(container, ctx) {
    // Query-string overrides (e.g. linked from another view).
    const q = ctx.query.get('q');
    if (ctx.query.get('method') && METHODS.some(([value]) => value === ctx.query.get('method'))) {
      state.method = ctx.query.get('method');
    }
    if (ctx.query.get('section') !== null && SECTIONS.some(([value]) => value === ctx.query.get('section'))) {
      state.section = ctx.query.get('section');
    }
    if (ctx.query.get('limit')) {
      const lim = Number(ctx.query.get('limit'));
      if ([10, 20, 50].indexOf(lim) >= 0) state.limit = lim;
    }

    container.innerHTML =
      '<div class="page-head">' +
      '<p class="page-sub">Semantic, full-text and hybrid search over your library</p></div>' +
      panelHTML() + tabsHTML() +
      '<div id="search-results" class="mt-4"></div>';

    wireControls(container);
    const input = container.querySelector('#search-input');
    if (input) input.focus();

    if (q !== null && q.trim()) {
      // Explicit query in the URL always wins over the cached view.
      state.query = q;
      input.value = q;
      runSearch(container);
    } else {
      // Restore the cached view without re-fetching.
      paintCounts(container);
      paintResults(container);
    }

    return function destroy() {
      reqSeq++; // drop any in-flight response
      U.closeLightbox();
    };
  }
});
