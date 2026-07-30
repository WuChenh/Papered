// Paper detail view (#/paper/:id) — sections, figures, metadata, translations.
// ES module — routes self-register on import via app.js.

import * as U from './util.js?v=2';
import { API } from './api.js?v=2';
import { navigate } from './router.js?v=2';
import { renderMarkdown } from './markdown.js?v=2';
import { wireCoverFallback, wireModalLinks, lastModal } from './library.js?v=2';

const POLL_MS = 3000;

// ---- detail helpers --------------------------------------------------------

function metaRow(dt, ddHTML) {
  return `<dt>${U.esc(dt)}</dt><dd>${ddHTML}</dd>`;
}

function chips(list) {
  return (list || []).map((v) => `<span class="chip">${U.esc(v)}</span>`).join(' ');
}

// Clickable keyword chips — navigate to the library filtered by that keyword.
function keywordChips(list) {
  return (list || []).map((v) =>
    `<a class="chip chip-link" href="#/library?keyword=${encodeURIComponent(v)}" ` +
    `title="Show papers with this keyword">${U.esc(v)}</a>`
  ).join(' ');
}

// ---- paper detail view -----------------------------------------------------

function renderPaper(container, ctx) {
  const id = ctx.params.id;
  let pendingChunk = ctx.params.chunkId || null;

  let pollTimer = null;
  let paper = null;
  let activeTab = 'sections';
  const tabCache = {}; // tab name -> fetched data (or 'error')
  let translations = {}; // "contentType:contentRef" -> translated text
  let translationLang = '';
  let translationPanelOpen = false;

  container.innerHTML = U.spinner('Loading paper…');
  load();

  // ---- data ----

  async function load() {
    clearTimeout(pollTimer);
    try {
      const p = await API.getPaper(id);
      paper = p;
      paint();
      if (p.status === 'processing') {
        pollTimer = setTimeout(poll, POLL_MS);
      }
      if (pendingChunk) {
        const c = pendingChunk;
        pendingChunk = null;
        openChunkModal(c);
      }
    } catch (e) {
      if (e.status === 404) {
        container.innerHTML = U.errorState('Paper not found.') +
          '<div class="center"><a class="btn ghost" href="#/library">← Back to Papers</a></div>';
      } else {
        container.innerHTML = U.errorState(e.message, 'Retry');
        container.querySelector('[data-retry]').addEventListener('click', load);
      }
    }
  }

  function poll() {
    API.getPaper(id).then((p) => {
      const changed = p.status !== paper.status;
      paper = p;
      if (changed) {
        paint(); // re-render once status flips; stops polling unless still processing
        if (p.status === 'processing') pollTimer = setTimeout(poll, POLL_MS);
      } else {
        pollTimer = setTimeout(poll, POLL_MS);
      }
    }).catch(() => {
      pollTimer = setTimeout(poll, POLL_MS * 5);
    });
  }

  // ---- layout ----

  function badges() {
    let html = U.badge(paper.status || 'unknown', U.statusTone(paper.status));
    if (paper.source && paper.source !== 'manual') html += U.badge(paper.source, 'accent');
    if (paper.paper_type) html += U.badge(paper.paper_type, 'muted');
    if (paper.embedding_model) html += U.badge(paper.embedding_model, 'muted');
    return html;
  }

  function metaCardHTML() {
    let rows = '';
    if (paper.doi) {
      rows += metaRow('DOI', `<a href="https://doi.org/${encodeURIComponent(paper.doi)}` +
        `" target="_blank" rel="noopener">${U.esc(paper.doi)} ${U.icon('external', 12)}</a>`);
    }
    if (paper.published_date) rows += metaRow('Published', U.esc(U.fmtDate(paper.published_date)));
    if (paper.venue) rows += metaRow('Venue', U.esc(paper.venue));
    if (paper.authors && paper.authors.length) {
      rows += metaRow('Authors', U.esc(paper.authors.join(', ')));
    }
    if (paper.affiliations && paper.affiliations.length) {
      rows += metaRow('Affiliations', '<ol class="meta-affiliations">' +
        paper.affiliations.map((a) => `<li>${U.esc(a)}</li>`).join('') + '</ol>');
    }
    if (paper.emails && paper.emails.length) {
      rows += metaRow('Emails', U.esc(paper.emails.join(', ')));
    }
    if (paper.corresponding_author && paper.corresponding_author.length) {
      rows += metaRow('Corresponding', U.esc(paper.corresponding_author.join(', ')));
    }
    if (paper.urls && paper.urls.length) {
      rows += metaRow('Links', paper.urls.map((u) =>
        `<a href="${U.esc(u)}" target="_blank" rel="noopener">` +
        `${U.esc(U.truncate(u, 60))}</a>`
      ).join('<br>'));
    }
    if (paper.data_availability) {
      rows += metaRow('Data availability', U.esc(paper.data_availability));
    }
    if (paper.file_path) {
      rows += metaRow('File', `<span class="mono small">${U.esc(paper.file_path)}</span> ` +
        '<button type="button" class="icon-btn" data-copy-path title="Copy path" aria-label="Copy path">' +
        `${U.icon('copy', 13)}</button>`);
    }
    const ents = paper.entities || {};
    [['Species', ents.species], ['Genes', ents.genes],
     ['Techniques', ents.techniques], ['Pathways', ents.pathways]].forEach(([label, vals]) => {
      if (vals && vals.length) rows += metaRow(label, chips(vals));
    });
    if (paper.retry_count > 0) rows += metaRow('Index retries', U.fmtInt(paper.retry_count));
    if (!rows) return '';
    // Sibling spacing comes from `.card + .card`. Collapsed by default —
    // metadata can be tall (affiliations, entities, links).
    return '<details class="card meta-details">' +
      '<summary class="card-head meta-summary"><h3>Metadata</h3>' +
      `<span class="meta-chevron">${U.icon('chevronRight', 14)}${U.icon('chevronDown', 14)}</span>` +
      '</summary>' +
      `<dl class="meta-grid">${rows}</dl></details>`;
  }

  // ---- rating & comments (user annotations) ----

  let currentRating = null; // last-known rating (null = unrated)

  function annotateCardHTML() {
    return '<div class="card" id="annotate-card">' +
      '<div class="card-head"><h3>Rating &amp; notes</h3></div>' +
      '<div class="stars" id="stars" role="radiogroup" aria-label="Rate this paper"></div>' +
      '<div id="comments-body" class="mt-3">' + U.spinner('Loading notes…') + '</div>' +
      '</div>';
  }

  function paintStars(rating) {
    currentRating = rating;
    const el = container.querySelector('#stars');
    if (!el) return;
    let html = '';
    for (let n = 1; n <= 5; n++) {
      const on = rating != null && n <= rating;
      html += '<button type="button" class="star-btn' + (on ? ' active' : '') + '" ' +
        'data-star="' + n + '" role="radio" aria-checked="' + String(on) + '" ' +
        'aria-label="' + n + ' star' + (n > 1 ? 's' : '') + '" ' +
        'title="' + (rating === n ? 'Click again to clear' : 'Rate ' + n + ' star' + (n > 1 ? 's' : '')) + '">' +
        U.icon('star', 20) + '</button>';
    }
    if (rating != null) {
      html += '<button type="button" class="btn subtle sm star-clear" data-star-clear>Clear</button>';
    }
    el.innerHTML = html;
    el.querySelectorAll('[data-star]').forEach((b) => {
      b.addEventListener('click', () => {
        const n = parseInt(b.getAttribute('data-star'), 10);
        saveRating(currentRating === n ? null : n);
      });
    });
    const clr = el.querySelector('[data-star-clear]');
    if (clr) clr.addEventListener('click', () => saveRating(null));
  }

  function saveRating(target) {
    const req = target == null ? API.clearRating(id) : API.setRating(id, target);
    req.then(() => {
      paintStars(target);
      U.toast(target == null ? 'Rating cleared'
        : 'Rated ' + target + ' star' + (target > 1 ? 's' : ''), 'success');
    }).catch(U.handleApiError);
  }

  function commentHTML(c) {
    return '<div class="comment" data-comment-id="' + encodeURIComponent(c.id) + '">' +
      '<div class="comment-head">' +
      '<span class="muted small">' + U.esc(U.fmtDateTime(c.created_at)) + '</span>' +
      '<span class="spacer"></span>' +
      '<button type="button" class="icon-btn" data-del-comment title="Delete note" aria-label="Delete note">' +
      U.icon('trash', 14) + '</button>' +
      '</div>' +
      '<div class="comment-body">' + U.esc(c.content) + '</div>' +
      '</div>';
  }

  function paintComments(comments) {
    const el = container.querySelector('#comments-body');
    if (!el) return;
    const list = (comments || []).length
      ? comments.map(commentHTML).join('')
      : '<p class="muted small">No notes yet — add the first one below.</p>';
    el.innerHTML =
      '<div class="comment-list">' + list + '</div>' +
      '<div class="comment-form mt-3">' +
      '<textarea class="textarea" id="comment-input" rows="2" ' +
      'placeholder="Add a note about this paper…"></textarea>' +
      '<div class="row mt-2"><span class="spacer"></span>' +
      '<button type="button" class="btn subtle sm" id="comment-insight" ' +
      'title="Draft an AI insight / critique / inspiration into the box">🤔</button>' +
      '<button type="button" class="btn primary sm" id="comment-add">Add note</button></div>' +
      '</div>';
    el.querySelectorAll('[data-del-comment]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const cid = btn.closest('[data-comment-id]').getAttribute('data-comment-id');
        API.deleteComment(id, cid).then(() => {
          U.toast('Note deleted', 'success');
          loadComments();
        }).catch(U.handleApiError);
      });
    });
    el.querySelector('#comment-insight').addEventListener('click', () => {
      const btn = el.querySelector('#comment-insight');
      const input = el.querySelector('#comment-input');
      btn.disabled = true;
      btn.textContent = '⏳';
      API.generateInsight(id).then((r) => {
        input.value = (r && r.insight ? r.insight : '').trim();
        input.focus();
      }).catch(U.handleApiError).finally(() => {
        btn.disabled = false;
        btn.textContent = '🤔';
      });
    });
    el.querySelector('#comment-add').addEventListener('click', () => {
      const input = el.querySelector('#comment-input');
      const content = (input.value || '').trim();
      if (!content) return;
      API.addComment(id, content).then(() => {
        U.toast('Note added', 'success');
        loadComments();
      }).catch(U.handleApiError);
    });
  }

  function loadComments() {
    API.listComments(id).then((comments) => {
      paintComments(comments);
    }).catch((e) => {
      const el = container.querySelector('#comments-body');
      if (el) el.innerHTML = U.errorState(e.message);
    });
  }

  function loadAnnotations() {
    API.getRating(id).then((r) => {
      paintStars(r && r.rating != null ? r.rating : null);
    }).catch(() => { paintStars(null); });
    loadComments();
  }

  function paint() {
    container.innerHTML = `
      <div class="back-link-wrap">
      <a class="back-link" href="#/library" title="Back to Papers" aria-label="Back to Papers">${U.icon('arrowLeft', 18)}</a>
      <div class="card">
        <div class="row-top">
          <img class="paper-cover detail-cover" data-cover src="${U.coverUrl(paper.id)}" alt="">
          <div class="flex-1">
            <div class="detail-title-wrap">
              <h1 class="detail-title">${U.esc(paper.title || 'Untitled')}</h1>
              <button type="button" class="trans-icon-btn" data-trans-icon data-trans-key="title:_self"
                title="Toggle title translation" aria-label="Toggle title translation">${U.icon('translate', 15)}</button>
            </div>
            <div class="trans-content trans-title hidden" data-trans-body="title:_self"></div>
            <div class="paper-meta">${U.paperLine(paper)}</div>
            <div class="paper-tags">${badges()}</div>
            ${paper.keywords && paper.keywords.length
              ? `<div class="paper-keywords mt-2">${keywordChips(paper.keywords)}</div>` : ''}
            ${paper.status === 'failed' && paper.error_message
              ? `<p class="small text-err mt-2">${U.icon('alert', 13)} ${U.esc(paper.error_message)}</p>`
              : ''}
            <div class="toolbar mt-4">
              <button type="button" class="btn primary sm" data-act="ask">${U.icon('ask', 14)}
                Ask about this paper</button>
              <button type="button" class="btn ghost sm" data-act="similar">Similar papers</button>
              <button type="button" class="btn ghost sm" data-act="edit">${U.icon('edit', 14)} Edit</button>
              <button type="button" class="btn ghost sm" data-act="reindex">${U.icon('refresh', 14)}
                Reindex</button>
              <button type="button" class="btn ghost sm" data-act="translate">${U.icon('translate', 14)}
                Translate</button>
              <button type="button" class="btn ghost sm" data-act="reindex-sections">Reindex sections</button>
              <span class="spacer"></span>
              <button type="button" class="btn danger sm" data-act="delete">${U.icon('trash', 14)}
                Delete</button>
            </div>
          </div>
        </div>
      </div>
      ${metaCardHTML()}
      ${annotateCardHTML()}
      <div class="card${translationPanelOpen ? '' : ' hidden'}" id="translation-panel">
        <div class="card-head"><h3>${U.icon('translate', 16)} Translations</h3>
          <button type="button" class="icon-btn" id="close-translation-panel" aria-label="Close">${U.icon('x', 14)}</button></div>
        <div class="kv-row mb-3">
          <select class="select select-sm" id="translation-lang-select"></select>
          <button type="button" class="btn primary sm" id="translate-all-btn">Translate all</button>
          <button type="button" class="btn ghost sm" id="delete-translations-btn">${U.icon('trash', 14)}</button>
        </div>
        <div id="translation-status" class="muted small"></div>
      </div>
      <div class="card">
        <div class="tabs" role="tablist" aria-label="Paper contents">
          <button type="button" role="tab" data-tab="sections">Sections</button>
          <button type="button" role="tab" data-tab="figures">Figures</button>
        </div>
        <div id="tab-body" class="mt-4"></div>
      </div>
      </div>`;

    wireCoverFallback(container.querySelector('img[data-cover]'));
    const detailCover = container.querySelector('img.detail-cover');
    if (detailCover) {
      detailCover.addEventListener('click', () => U.openCoverLightbox(paper.id));
    }

    const copyBtn = container.querySelector('[data-copy-path]');
    if (copyBtn) {
      copyBtn.addEventListener('click', () => { U.copy(paper.file_path, 'Path'); });
    }

    container.querySelectorAll('[data-tab]').forEach((b) => {
      const on = b.getAttribute('data-tab') === activeTab;
      b.classList.toggle('active', on);
      b.setAttribute('aria-selected', String(on));
      b.addEventListener('click', () => {
        activeTab = b.getAttribute('data-tab');
        container.querySelectorAll('[data-tab]').forEach((x) => {
          const xOn = x === b;
          x.classList.toggle('active', xOn);
          x.setAttribute('aria-selected', String(xOn));
        });
        showTab(activeTab);
      });
    });

    container.querySelector('[data-act="ask"]').addEventListener('click', () => {
      navigate('#/ask?paper=' + encodeURIComponent(id));
    });
    container.querySelector('[data-act="similar"]').addEventListener('click', openSimilarModal);
    container.querySelector('[data-act="edit"]').addEventListener('click', openEditModal);
    container.querySelector('[data-act="reindex"]').addEventListener('click', () => {
      API.reindex(id).then(() => {
        U.toast('Reindex queued', 'success');
        load();
      }).catch(U.handleApiError);
    });
    container.querySelector('[data-act="reindex-sections"]').addEventListener('click', () => {
      API.reindexSections(id).then(() => {
        U.toast('Section reindex queued', 'success');
      }).catch(U.handleApiError);
    });
    container.querySelector('[data-act="delete"]').addEventListener('click', () => {
      U.confirm({
        title: 'Delete this paper?',
        body: 'This removes "' + (paper.title || 'Untitled') +
          '" and its indexed data. The source file is not touched.',
        confirmLabel: 'Delete', danger: true
      }).then((ok) => {
        if (!ok) return;
        API.deletePaper(id).then(() => {
          U.toast('Paper deleted', 'success');
          navigate('#/library');
        }).catch(U.handleApiError);
      });
    });

    container.querySelector('[data-act="translate"]').addEventListener('click', () => {
      translationPanelOpen = true;
      initTranslationPanel();
    });
    const closeTransPanel = container.querySelector('#close-translation-panel');
    if (closeTransPanel) closeTransPanel.addEventListener('click', () => {
      translationPanelOpen = false;
      const panel = container.querySelector('#translation-panel');
      if (panel) panel.classList.add('hidden');
    });

    wireTransIcons(container);
    populateTransBodies(container);
    showTab(activeTab);
    loadAnnotations();
    // Auto-load translations if config has a target language.
    loadConfigLanguage();
  }

  // ---- translations ----

  function loadConfigLanguage() {
    API.getConfig().then((r) => {
      const cfg = r.config || {};
      translationLang = (cfg.translation || {}).target_language || 'zh-CN';
      if (translationPanelOpen) populateTranslationPanel();
    }).catch(() => { translationLang = 'zh-CN'; });
  }

  function initTranslationPanel() {
    const panel = container.querySelector('#translation-panel');
    if (!panel) return;
    panel.classList.remove('hidden');
    if (!translationLang) {
      loadConfigLanguage();
    } else {
      populateTranslationPanel();
    }
  }

  function populateTranslationPanel() {
    const sel = container.querySelector('#translation-lang-select');
    if (!sel) return;
    const langs = [
      { value: 'zh-CN', label: 'Chinese (Simplified)' },
      { value: 'en', label: 'English' },
      { value: 'fr', label: 'French' },
      { value: 'de', label: 'German' },
      { value: 'es', label: 'Spanish' },
      { value: 'ru', label: 'Russian' }
    ];
    sel.innerHTML = langs.map((l) =>
      `<option value="${U.esc(l.value)}"${l.value === translationLang ? ' selected' : ''}>${U.esc(l.label)}</option>`
    ).join('');
    sel.onchange = () => {
      translationLang = sel.value;
      loadTranslations();
    };
    const translateAllBtn = container.querySelector('#translate-all-btn');
    if (translateAllBtn) translateAllBtn.onclick = translateAll;
    const deleteBtn = container.querySelector('#delete-translations-btn');
    if (deleteBtn) deleteBtn.onclick = deleteAllTranslations;
    loadTranslations();
  }

  function loadTranslations() {
    if (!translationLang) return;
    const statusEl = container.querySelector('#translation-status');
    if (statusEl) statusEl.textContent = 'Loading translations…';
    API.getTranslations(id, translationLang).then((res) => {
      translations = {};
      (res.translations || []).forEach((t) => {
        translations[t.content_type + ':' + t.content_ref] = t.translated_text;
      });
      if (statusEl) {
        const count = Object.keys(translations).length;
        statusEl.textContent = count + ' translation' + (count !== 1 ? 's' : '') + ' loaded';
      }
      // Re-render the active tab and title to show translations.
      refreshTransDisplay();
    }).catch((e) => {
      if (statusEl) statusEl.textContent = 'Error: ' + e.message;
    });
  }

  function translateAll() {
    const btn = container.querySelector('#translate-all-btn');
    const statusEl = container.querySelector('#translation-status');
    if (btn) { btn.disabled = true; btn.textContent = 'Translating…'; }
    if (statusEl) statusEl.textContent = 'Translating all content…';
    API.batchTranslate(id, { target_language: translationLang }).then((res) => {
      const items = res.translations || [];
      items.forEach((t) => {
        translations[t.content_type + ':' + t.content_ref] = t.translated_text;
      });
      const cached = items.filter((t) => t.cached).length;
      const newCount = items.length - cached;
      if (statusEl) {
        statusEl.textContent = items.length + ' translations (' + newCount + ' new, ' + cached + ' cached)';
      }
      U.toast('Translation complete: ' + newCount + ' new, ' + cached + ' cached', 'success');
      refreshTransDisplay();
    }).catch((e) => {
      if (statusEl) statusEl.textContent = 'Error: ' + e.message;
      U.toast(e.message, 'error');
    }).finally(() => {
      if (btn) { btn.disabled = false; btn.textContent = 'Translate all'; }
    });
  }

  function deleteAllTranslations() {
    U.confirm({
      title: 'Delete all translations?',
      body: 'This will remove all translations for this paper. This cannot be undone.',
      confirmLabel: 'Delete', danger: true
    }).then((ok) => {
      if (!ok) return;
      API.deleteTranslations(id).then(() => {
        translations = {};
        U.toast('Translations deleted', 'success');
        const statusEl = container.querySelector('#translation-status');
        if (statusEl) statusEl.textContent = '0 translations';
        refreshTransDisplay();
      }).catch(U.handleApiError);
    });
  }

  function translatedText(contentType, contentRef) {
    return translations[contentType + ':' + contentRef] || null;
  }

  // Fill a translation body element from the loaded translations map.
  function populateTransBody(bodyEl, tType, tRef) {
    const t = translatedText(tType, tRef);
    if (!t) {
      bodyEl.innerHTML = '';
      bodyEl.classList.add('hidden');
      return;
    }
    if (tType === 'section' || tType === 'abstract') {
      bodyEl.innerHTML = renderMarkdown(t);
    } else {
      bodyEl.innerHTML = U.icon('translate', 12) + ' ' + U.esc(t);
    }
    bodyEl.classList.remove('hidden');
  }

  function populateTransBodies(scope) {
    scope.querySelectorAll('[data-trans-body]').forEach((bodyEl) => {
      const key = bodyEl.getAttribute('data-trans-body');
      const parts = key.split(':');
      populateTransBody(bodyEl, parts[0], parts.slice(1).join(':'));
    });
    syncTransIconStates();
  }

  function syncTransIconStates() {
    container.querySelectorAll('[data-trans-icon]').forEach((btn) => {
      const key = btn.getAttribute('data-trans-key');
      const bodyEl = container.querySelector('[data-trans-body="' + CSS.escape(key) + '"]');
      const on = Boolean(bodyEl && bodyEl.innerHTML && !bodyEl.classList.contains('hidden'));
      btn.classList.toggle('active', on);
    });
  }

  // Wire the subtle top-right translate icons. Clicking toggles the translation
  // shown below the original text; if no translation exists yet, it triggers a
  // single-item translation first.
  function wireTransIcons(scope) {
    scope.querySelectorAll('[data-trans-icon]:not([data-wired])').forEach((btn) => {
      btn.setAttribute('data-wired', '1');
      btn.addEventListener('click', () => {
        const key = btn.getAttribute('data-trans-key');
        const bodyEl = container.querySelector('[data-trans-body="' + CSS.escape(key) + '"]');
        if (!bodyEl) return;
        const parts = key.split(':');
        const tType = parts[0];
        const tRef = parts.slice(1).join(':');
        if (translatedText(tType, tRef)) {
          bodyEl.classList.toggle('hidden');
          btn.classList.toggle('active', !bodyEl.classList.contains('hidden'));
          return;
        }
        const lang = translationLang || 'zh-CN';
        btn.disabled = true;
        API.translatePaper(id, { target_language: lang, content_type: tType, content_ref: tRef })
          .then((res) => {
            translations[tType + ':' + tRef] = res.translated_text;
            populateTransBody(bodyEl, tType, tRef);
            btn.classList.add('active');
          })
          .catch(U.handleApiError)
          .finally(() => { btn.disabled = false; });
      });
    });
  }

  // Re-render the active tab and the title translation after the map changes.
  function refreshTransDisplay() {
    if (tabCache[activeTab] && tabCache[activeTab] !== 'error') {
      const body = container.querySelector('#tab-body');
      if (body) paintTab(activeTab, body, tabCache[activeTab]);
    }
    const titleBody = container.querySelector('[data-trans-body="title:_self"]');
    if (titleBody) populateTransBody(titleBody, 'title', '_self');
    syncTransIconStates();
  }

  // ---- tabs (lazy) ----

  function showTab(name) {
    const body = container.querySelector('#tab-body');
    if (!body) return;
    if (tabCache[name] && tabCache[name] !== 'error') {
      paintTab(name, body, tabCache[name]);
      return;
    }
    body.innerHTML = U.spinner('Loading…');
    const req = name === 'sections' ? API.getSections(id)
      : API.getFigures(id);
    req.then((data) => {
      tabCache[name] = data;
      const b = container.querySelector('#tab-body');
      if (b) paintTab(name, b, data);
    }).catch((e) => {
      tabCache[name] = 'error';
      const b = container.querySelector('#tab-body');
      if (!b) return;
      b.innerHTML = U.errorState(e.message, 'Retry');
      b.querySelector('[data-retry]').addEventListener('click', () => {
        delete tabCache[name];
        showTab(name);
      });
    });
  }

  function paintTab(name, body, data) {
    if (name === 'sections') {
      // The metadata section is already shown in the Metadata card above.
      const secs = data.filter((s) => s.section_type !== 'metadata');
      if (!secs.length) {
        body.innerHTML = U.emptyState({
          icon: 'file', title: 'No sections extracted',
          body: 'Sections appear after the paper is indexed. Try Reindex sections above.'
        });
        return;
      }
      body.innerHTML = '<div class="reading">' + secs.map((s) => {
        const stype = String(s.section_type || 'section');
        const heading = stype.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
        // The abstract section's translation is stored as abstract:_self.
        const tKey = stype === 'abstract' ? 'abstract:_self' : 'section:' + stype;
        return '<div class="section-block">' +
          '<button type="button" class="trans-icon-btn" data-trans-icon data-trans-key="' + U.esc(tKey) + '" ' +
          'title="Toggle translation" aria-label="Toggle translation">' + U.icon('translate', 14) + '</button>' +
          '<div class="kv-row"><h3>' + U.esc(heading) + '</h3></div>' +
          '<div class="section-content md-rendered">' + renderMarkdown(s.content) + '</div>' +
          '<div class="section-content md-rendered trans-content hidden" data-trans-body="' + U.esc(tKey) + '"></div>' +
          '</div>';
      }).join('') + '</div>';
      // Populate any loaded translations below the originals and wire the icons.
      populateTransBodies(body);
      wireTransIcons(body);
    } else if (name === 'figures') {
      if (!data.length) {
        body.innerHTML = U.emptyState({
          icon: 'image', title: 'No figures found',
          body: 'Figures are extracted during indexing when the source PDF contains images.'
        });
        return;
      }
      body.innerHTML = '<div class="figure-grid">' + data.map((f, i) => {
        const hasImg = Boolean(f.image_path);
        const media = hasImg
          ? `<img src="${U.figureImageUrl(id, f.id)}" alt="" loading="lazy">`
          : `<div class="figure-noimg">${U.icon('image', 28)}<span>Image unavailable</span></div>`;
        const tKey = 'figure_caption:' + f.id;
        // Only the image area opens the viewer; the caption stays selectable.
        return `<div class="figure-card${hasImg ? '' : ' no-image'}">` +
          `<button type="button" class="trans-icon-btn" data-trans-icon data-trans-key="${U.esc(tKey)}" ` +
          `title="Toggle caption translation" aria-label="Toggle caption translation">${U.icon('translate', 13)}</button>` +
          (hasImg
            ? `<div class="figure-media" data-fig="${i}" role="button" tabindex="0" aria-label="Open figure">${media}</div>`
            : media) +
          `<div class="figure-caption">${U.esc(U.truncate(U.formatFigureLabel(f, i), 140))}</div>` +
          `<div class="trans-caption-body hidden" data-trans-body="${U.esc(tKey)}"></div>` +
          '</div>';
      }).join('') + '</div>';
      populateTransBodies(body);
      wireTransIcons(body);
      body.querySelectorAll('[data-fig]').forEach((media) => {
        const fig = data[Number(media.getAttribute('data-fig'))];
        const tKey = 'figure_caption:' + fig.id;
        const open = () => U.openLightbox({
          src: U.figureImageUrl(id, fig.id),
          caption: fig.caption,
          description: fig.description,
          page: fig.page_number,
          label: 'Figure',
          translation: translatedText('figure_caption', fig.id),
          onTranslate: () => {
            const existing = translatedText('figure_caption', fig.id);
            if (existing) return Promise.resolve(existing);
            const lang = translationLang || 'zh-CN';
            return API.translatePaper(id, {
              target_language: lang, content_type: 'figure_caption', content_ref: fig.id
            }).then((res) => {
              translations[tKey] = res.translated_text;
              // Keep the figure card's caption translation below in sync.
              const cardBody = container.querySelector('[data-trans-body="' + CSS.escape(tKey) + '"]');
              if (cardBody) populateTransBody(cardBody, 'figure_caption', fig.id);
              syncTransIconStates();
              return res.translated_text;
            });
          }
        });
        media.addEventListener('click', open);
        media.addEventListener('keydown', (e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            open();
          }
        });
      });
    }
  }

  // ---- modals ----

  function openEditModal() {
    const val = (v) => U.esc(v || '');
    const body = `
      <div class="field"><label for="ed-title">Title</label>
        <input class="input" id="ed-title" value="${val(paper.title)}"></div>
      <div class="grid cols-2 mt-3">
        <div class="field"><label for="ed-venue">Venue</label>
          <input class="input" id="ed-venue" value="${val(paper.venue)}"></div>
        <div class="field"><label for="ed-doi">DOI</label>
          <input class="input" id="ed-doi" value="${val(paper.doi)}"></div>
        <div class="field"><label for="ed-date">Publication date</label>
          <input class="input" id="ed-date" value="${val(paper.published_date)}" placeholder="2024-03-15"></div>
        <div class="field"><label for="ed-type">Paper type</label>
          <input class="input" id="ed-type" value="${val(paper.paper_type)}" placeholder="journal-article"></div>
      </div>
      <div class="field mt-3"><label for="ed-authors">Authors</label>
        <textarea class="textarea" id="ed-authors" placeholder="One author per line">${val((paper.authors || []).join('\n'))}</textarea></div>
      <div class="field mt-3"><label for="ed-keywords">Keywords</label>
        <input class="input" id="ed-keywords" value="${val((paper.keywords || []).join(', '))}" placeholder="comma, separated"></div>
      <div class="field mt-3"><label for="ed-abstract">Abstract</label>
        <textarea class="textarea" id="ed-abstract">${val(paper.abstract_text)}</textarea></div>
      <div class="field mt-3"><label for="ed-data-avail">Data availability</label>
        <textarea class="textarea" id="ed-data-avail">${val(paper.data_availability)}</textarea></div>
      <div class="field-error" id="ed-error" role="alert"></div>`;

    U.modal({
      title: 'Edit paper',
      body: body, wide: true,
      actions: [
        { label: 'Cancel', value: null, kind: 'ghost' },
        {
          label: 'Save', kind: 'primary',
          onClick: async (close, btn) => {
            const bd = lastModal();
            const get = (sel) => bd.querySelector(sel).value.trim();
            const lines = (sel) =>
              get(sel).split('\n').map((s) => s.trim()).filter(Boolean);
            const commas = (sel) =>
              get(sel).split(',').map((s) => s.trim()).filter(Boolean);
            const payload = {};
            const put = (k, v) => { if (v && (!Array.isArray(v) || v.length)) payload[k] = v; };
            put('title', get('#ed-title'));
            put('venue', get('#ed-venue'));
            put('doi', get('#ed-doi'));
            put('published_date', get('#ed-date'));
            put('paper_type', get('#ed-type'));
            put('authors', lines('#ed-authors'));
            put('keywords', commas('#ed-keywords'));
            put('abstract_text', get('#ed-abstract'));
            put('data_availability', get('#ed-data-avail'));
            btn.disabled = true;
            try {
              await API.updatePaper(id, payload);
              close(true);
            } catch (e) {
              btn.disabled = false;
              bd.querySelector('#ed-error').textContent = e.message;
            }
          }
        }
      ]
    }).then((saved) => {
      if (saved) {
        U.toast('Paper updated', 'success');
        load();
      }
    });
  }

  function openSimilarModal() {
    U.modal({
      title: 'Similar papers',
      body: U.spinner('Finding similar papers…'), wide: true,
      actions: [{ label: 'Close', value: null, kind: 'ghost' }]
    });
    const bd = lastModal();
    if (!bd) return;
    const body = bd.querySelector('.modal-body');
    API.similar(id, 8).then((results) => {
      if (!results || !results.length) {
        body.innerHTML = '<p class="muted small center pad-block">No similar papers found yet.</p>';
        return;
      }
      body.innerHTML = results.map((r) => {
        const p = r.paper || r;
        const pct = U.scorePct(r.score);
        return '<div class="verify-row align-top">' +
          '<div class="flex-1">' +
          `<div class="paper-title"><a href="#/paper/${encodeURIComponent(p.id)}">` +
          `${U.esc(p.title || 'Untitled')}</a></div>` +
          `<div class="paper-meta">${U.paperLine(p)}</div></div>` +
          '<span class="score-bar">' +
          `<span class="progress" role="progressbar" aria-valuenow="${pct}" aria-valuemin="0" aria-valuemax="100">` +
          '<div data-bar></div></span>' +
          `<span class="score-num">${pct}%</span></span></div>`;
      }).join('');
      // Dynamic bar widths are set after insertion (values, not layout).
      body.querySelectorAll('[data-bar]').forEach((bar, i) => {
        bar.style.width = U.scorePct(results[i].score) + '%';
      });
      wireModalLinks(bd);
    }).catch((e) => {
      body.innerHTML = `<p class="small text-err">${U.esc(e.message)}</p>`;
    });
  }

  async function openChunkModal(chunkId) {
    try {
      const res = await API.getChunk(id, chunkId);
      const chunk = res.chunk || {};
      let heading = res.heading_path || '';
      if (Array.isArray(heading)) heading = heading.join(' › ');
      U.modal({
        title: 'Passage',
        wide: true,
        body:
          (heading ? `<div class="muted small mb-2">${U.esc(heading)}</div>` : '') +
          (chunk.page_number != null
            ? `<div class="mb-2">${U.badge('page ' + chunk.page_number, 'muted')}` +
              (chunk.chunk_type ? ' ' + U.badge(chunk.chunk_type, 'info') : '') + '</div>'
            : (chunk.chunk_type
              ? `<div class="mb-2">${U.badge(chunk.chunk_type, 'info')}</div>`
              : '')) +
          `<div class="reading md-rendered">${renderMarkdown(U.cleanVerbatim(chunk.content))}</div>`,
        actions: [{ label: 'Close', value: null, kind: 'primary' }]
      });
    } catch (e) {
      U.toast(e.message, 'error');
    }
  }

  return function destroy() {
    clearTimeout(pollTimer);
    U.closeLightbox();
  };
}

import { route } from './router.js?v=2';
route('/paper/:id', { title: 'Paper', render: renderPaper });
route('/paper/:id/chunk/:chunkId', { title: 'Paper', render: renderPaper });
