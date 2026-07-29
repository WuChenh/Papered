// Shared UI helpers: escaping, formatting, icons, toasts, modals.
// ES module — imported by the shell and every view module.

// ---- escaping ---------------------------------------------------------------

const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

export function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => ESC[c]);
}

export function truncate(s, n) {
  s = String(s ?? '');
  return s.length > n ? s.slice(0, n - 1) + '…' : s;
}

// ---- formatting -------------------------------------------------------------

export function fmtInt(n) {
  return (typeof n === 'number' && isFinite(n) ? n : 0).toLocaleString('en-US');
}

// Human-readable size from a byte count.
export function fmtBytes(bytes) {
  bytes = Number(bytes) || 0;
  if (bytes < 1024) return bytes + ' B';
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = bytes;
  let i = -1;
  do { v /= 1024; i++; } while (v >= 1024 && i < units.length - 1);
  return v.toFixed(v >= 100 ? 0 : 1) + ' ' + units[i];
}

// Stats API reports data_dir_size_mb.
export function fmtMB(mb) {
  return fmtBytes((Number(mb) || 0) * 1024 * 1024);
}

// SQLite `datetime('now')` returns UTC without a timezone suffix
// (e.g. "2024-03-15 10:30:00").  `new Date(str)` would then guess the
// timezone inconsistently across browsers.  Append 'Z' when the string
// has no timezone indicator so it is always parsed as UTC; the various
// toLocale* calls then convert to the user's local timezone correctly.
function toUtcDate(s) {
  if (!s) return new Date(NaN);
  const str = String(s).trim();
  // Already has explicit timezone (Z, +HH:MM, -HH:MM) → parse as-is
  const hasTz = /Z$/i.test(str) || /[+-]\d{2}:\d{2}$/.test(str);
  const d = new Date(hasTz ? str : str + 'Z');
  return d;
}

export function fmtDate(s) {
  if (!s) return '';
  const d = toUtcDate(s);
  if (isNaN(d)) return String(s).slice(0, 10);
  return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

export function fmtDateTime(s) {
  if (!s) return '';
  const d = toUtcDate(s);
  if (isNaN(d)) return String(s);
  return d.toLocaleString(undefined, {
    month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'
  });
}

export function relTime(s) {
  const d = toUtcDate(s);
  if (isNaN(d)) return '';
  const sec = Math.max(0, (Date.now() - d.getTime()) / 1000);
  if (sec < 60) return 'just now';
  if (sec < 3600) return Math.floor(sec / 60) + 'm ago';
  if (sec < 86400) return Math.floor(sec / 3600) + 'h ago';
  if (sec < 86400 * 30) return Math.floor(sec / 86400) + 'd ago';
  return fmtDate(s);
}

export function fmtMs(ms) {
  ms = Number(ms) || 0;
  if (ms < 1000) return Math.round(ms) + ' ms';
  return (ms / 1000).toFixed(1) + ' s';
}

export function debounce(fn, ms) {
  let t = null;
  return function (...args) {
    clearTimeout(t);
    t = setTimeout(() => fn.apply(this, args), ms);
  };
}

// ---- paper display helpers ----------------------------------------------------

// "Smith et al. · 2024 · Nature" style one-liner.
export function paperLine(paper) {
  const parts = [];
  const authors = paper.authors || [];
  if (authors.length) {
    parts.push(authors.length > 2 ? authors[0] + ' et al.' : authors.join(' & '));
  }
  const year = (paper.published_date || '').slice(0, 4);
  if (year) parts.push(year);
  if (paper.venue) parts.push(paper.venue);
  return parts.map(esc).join(' · ');
}

export function statusTone(status) {
  return { indexed: 'ok', processing: 'info', failed: 'err' }[status] || 'muted';
}

/**
 * Render a paper title link.
 * @param {object} paper - Paper object with `id` and `title`.
 * @param {object} [opts] - Options.
 * @param {number} [opts.maxLen] - Truncate title to this length.
 * @param {string} [opts.cls] - CSS class for the anchor.
 * @param {boolean} [opts.titleAttr] - Add a title attribute with the full title.
 */
export function paperLink(paper, opts = {}) {
  const href = '#/paper/' + encodeURIComponent(paper.id);
  const title = paper.title || 'Untitled';
  const label = esc(opts.maxLen ? truncate(title, opts.maxLen) : title);
  const cls = opts.cls ? ' class="' + opts.cls + '"' : '';
  const titleAttr = opts.titleAttr ? ' title="' + esc(title) + '"' : '';
  return '<a' + cls + ' href="' + href + '"' + titleAttr + '>' + label + '</a>';
}

/**
 * Render a paper cover thumbnail image.
 * @param {object} paper - Paper object with `id`.
 * @param {object} [opts] - Options.
 * @param {string} [opts.cls] - CSS class for the img element.
 * @param {string} [opts.extraAttrs] - Additional HTML attributes (e.g. 'data-cover').
 */
export function paperCoverImg(paper, opts = {}) {
  const cls = opts.cls || 'paper-cover';
  const extra = opts.extraAttrs ? ' ' + opts.extraAttrs : '';
  return '<img class="' + cls + '" src="' + coverUrl(paper.id, true) + '" alt="" loading="lazy"' + extra + '>';
}

export function scorePct(score) {
  return Math.round(Math.max(0, Math.min(1, Number(score) || 0)) * 100);
}

// Publisher download-watermarks ("Downloaded from … by … user on <date>") are
// baked into extracted PDF text. They are acquisition artifacts, not paper
// content — strip them before showing verbatim source text to the user.
// Two shapes occur: the watermark on its own line, and one injected
// mid-sentence by the PDF extractor (it splits the surrounding sentence and
// is followed by a paragraph break — removing it rejoins the sentence).
// Keep in sync with clean_verbatim_text() in crates/papered/src/util.rs.
const WATERMARK_LINE_RE = /^[ \t]*Downloaded from\b[^\n]*?\buser on\s+\d{1,2}\s+\w+\s+\d{4}\b[^\n]*\r?$/gim;
const WATERMARK_INLINE_RE = /[ \t]+Downloaded from\b[^\n]*?\buser on\s+\d{1,2}\s+\w+\s+\d{4}\b[ \t]*(?:\r?\n[ \t]*){0,2}/gi;
export function cleanVerbatim(text) {
  return String(text ?? '')
    .replace(WATERMARK_LINE_RE, '')
    .replace(WATERMARK_INLINE_RE, ' ')
    .replace(/\n{3,}/g, '\n\n')
    .replace(/ {2,}/g, ' ');
}

export function coverUrl(paperId, thumb) {
  const base = '/api/v1/papers/' + encodeURIComponent(paperId) + '/cover';
  return thumb ? base + '/thumb' : base;
}

// ---- lightbox (shared full-screen image viewer) ------------------------------

let lightboxEl = null;

export function closeLightbox() {
  if (lightboxEl) {
    lightboxEl.remove();
    lightboxEl = null;
    document.removeEventListener('keydown', onLightboxKey);
  }
}

function onLightboxKey(e) {
  if (e.key === 'Escape') closeLightbox();
}

/** Open the shared image viewer. opts: { src, caption, description, page, label,
 * translation, onTranslate }. `translation` is pre-loaded translated caption text
 * shown below the original; `onTranslate` is an optional async callback that
 * fetches the translation on demand — a toggle button appears when either is
 * present. Only backdrop clicks (plus the × button / Esc) dismiss it, so the
 * caption bar — laid out below the image, never overlaid on it — stays selectable. */
export function openLightbox(opts) {
  closeLightbox();
  const lb = document.createElement('div');
  lb.className = 'lightbox';
  lb.setAttribute('role', 'dialog');
  lb.setAttribute('aria-modal', 'true');
  lb.setAttribute('aria-label', opts.label || 'Image');
  const cap = opts.caption || '';
  const desc = opts.description && opts.description !== opts.caption ? opts.description : '';
  const meta = [desc, opts.page != null ? 'Page ' + fmtInt(opts.page) : null].filter(Boolean);
  const hasTrans = Boolean(opts.translation || opts.onTranslate);
  lb.innerHTML =
    '<button type="button" class="icon-btn lightbox-close" aria-label="Close">' + icon('x', 16) + '</button>' +
    '<figure class="lightbox-inner">' +
    '<img src="' + esc(opts.src) + '" alt="">' +
    (cap || meta.length || hasTrans
      ? '<figcaption class="lightbox-caption' + (hasTrans ? ' has-trans-btn' : '') + '">' +
        (hasTrans
          ? '<button type="button" class="lightbox-trans-btn" title="Toggle caption translation" aria-label="Toggle caption translation">' + icon('translate', 14) + '</button>'
          : '') +
        (cap ? '<div>' + esc(cap) + '</div>' : '') +
        '<div class="lightbox-trans hidden"></div>' +
        (meta.length ? '<div class="lightbox-meta">' + meta.map(esc).join(' · ') + '</div>' : '') +
        '</figcaption>'
      : '') +
    '</figure>';
  document.body.appendChild(lb);
  lb.addEventListener('click', (e) => { if (e.target === lb) closeLightbox(); });
  lb.querySelector('.lightbox-close').addEventListener('click', closeLightbox);
  if (hasTrans) {
    const transEl = lb.querySelector('.lightbox-trans');
    const transBtn = lb.querySelector('.lightbox-trans-btn');
    let transText = opts.translation || '';
    const show = () => {
      transEl.innerHTML = icon('translate', 12) + ' ' + esc(transText);
      transEl.classList.remove('hidden');
      transBtn.classList.add('active');
    };
    if (transText) show();
    transBtn.addEventListener('click', () => {
      if (transText) {
        const showing = !transEl.classList.contains('hidden');
        transEl.classList.toggle('hidden', showing);
        transBtn.classList.toggle('active', !showing);
        return;
      }
      if (!opts.onTranslate) return;
      transBtn.disabled = true;
      opts.onTranslate()
        .then((t) => { transText = t || ''; if (transText) show(); })
        .catch(() => {})
        .finally(() => { transBtn.disabled = false; });
    });
  }
  document.addEventListener('keydown', onLightboxKey);
  lightboxEl = lb;
}

export function openCoverLightbox(paperId) {
  openLightbox({ src: coverUrl(paperId), label: 'Cover' });
}

export function figureImageUrl(paperId, figureId) {
  return '/api/v1/papers/' + encodeURIComponent(paperId) +
    '/figures/' + encodeURIComponent(figureId) + '/image';
}

/** Strip a leading "Figure 1." / "Fig. 1:" / "Supplementary Figure S2." /
 * "Extended Data Fig. 1 |" prefix from a caption — the display label
 * already carries the figure number, so keeping the caption's own prefix
 * reads "Fig. 1: Figure 1. …". Only strips when a number follows the word
 * (a caption legitimately starting with "Figure showing…" is left alone). */
function stripCaptionFigurePrefix(caption) {
  return caption.replace(
    /^\s*(?:extended\s+data\s+|supplementary\s+)?(?:figure|fig)\.?\s+(?:[0-9]+[a-z]*|s[0-9]+[a-z]*)\s*[:.|\-–—]?\s*/i,
    '');
}

/** Render a figure's display label from its stored metadata. */
export function formatFigureLabel(f, index) {
  const label = f.figure_label;
  const caption = stripCaptionFigurePrefix(f.caption || f.description || '').trim();
  if (label) {
    // Labels that already carry a figure word ("Extended Data Fig. 1")
    // are shown as-is — prepending "Fig." would read "Fig. Extended…".
    const labelText = label === 'graphical_abstract' ? 'Graphical abstract'
      : /\bfig(?:ure)?\.?/i.test(label) ? label
      : 'Fig. ' + label;
    if (caption && caption !== labelText) return labelText + ': ' + caption;
    return labelText;
  }
  return caption || ('Figure ' + (index + 1));
}

// ---- icons (inline SVG, stroke = currentColor) --------------------------------

const ICONS = {
  dashboard: '<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>',
  library: '<path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20"/><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/>',
  search: '<circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/>',
  ask: '<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>',
  prompts: '<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/>',
  sync: '<path d="M21 12a9 9 0 1 1-2.64-6.36"/><path d="M21 3v6h-6"/>',
  health: '<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/>',
  settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82.33l.06.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  trash: '<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>',
  edit: '<path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z"/>',
  x: '<path d="M18 6 6 18M6 6l12 12"/>',
  check: '<path d="M20 6 9 17l-5-5"/>',
  alert: '<path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><path d="M12 9v4M12 17h.01"/>',
  image: '<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.09-3.09a2 2 0 0 0-2.82 0L6 21"/>',
  file: '<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/>',
  copy: '<rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>',
  arrowLeft: '<path d="m12 19-7-7 7-7"/><path d="M19 12H5"/>',
  chevronRight: '<path d="m9 18 6-6-6-6"/>',
  chevronDown: '<path d="m6 9 6 6 6-6"/>',
  external: '<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>',
  refresh: '<path d="M3 12a9 9 0 0 1 15.36-6.36L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-15.36 6.36L3 16"/><path d="M3 21v-5h5"/>',
  sparkles: '<path d="m12 3 1.9 5.7 5.7 1.9-5.7 1.9L12 18.2l-1.9-5.7-5.7-1.9 5.7-1.9Z"/><path d="M19 15l.9 2.6 2.6.9-2.6.9-.9 2.6-.9-2.6-2.6-.9 2.6-.9Z"/>',
  upload: '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m17 8-5-5-5 5"/><path d="M12 3v12"/>',
  send: '<path d="m22 2-7 20-4-9-9-4Z"/><path d="M22 2 11 13"/>',
  star: '<polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/>',
  list: '<path d="M8 6h13M8 12h13M8 18h13"/><path d="M3 6h.01M3 12h.01M3 18h.01"/>',
  download: '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/><path d="M12 15V3"/>',
  eye: '<path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-3.5-7-10-7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/>',
  network: '<circle cx="5" cy="5.5" r="2.3"/><circle cx="19" cy="5.5" r="2.3"/><circle cx="12" cy="18.5" r="2.3"/><path d="M6.9 7.3 10.4 16.4"/><path d="M17.1 7.3 13.6 16.4"/><path d="M7.3 5.5h9.4"/>',
  user: '<path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/>',
  translate: '<path d="M5 8h10M8 5v3M4 14c0 3 2.5 5 6 5s6-2 6-5"/><path d="M14 11c1.5 1 3 2.5 3 4.5"/><path d="M10 11c-1.5 1-3 2.5-3 4.5"/>'
};

export function icon(name, size) {
  const body = ICONS[name] || '';
  return '<svg width="' + (size || 18) + '" height="' + (size || 18) +
    '" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" ' +
    'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">' + body + '</svg>';
}

// ---- badges / chips -----------------------------------------------------------

export function badge(text, tone) {
  return '<span class="badge ' + (tone || 'muted') + '">' + esc(text) + '</span>';
}

// ---- toasts -------------------------------------------------------------------

export function toast(msg, type, ms) {
  const host = document.getElementById('toasts');
  if (!host) return;
  const el = document.createElement('div');
  el.className = 'toast ' + (type || 'info');
  el.setAttribute('role', 'status');
  el.innerHTML = '<span class="toast-icon">' +
    icon(type === 'error' ? 'alert' : type === 'success' ? 'check' : 'alert', 16) +
    '</span><span>' + esc(msg) + '</span>';
  host.appendChild(el);
  setTimeout(() => el.classList.add('show'), 10);
  setTimeout(() => {
    el.classList.remove('show');
    setTimeout(() => el.remove(), 250);
  }, ms || (type === 'error' ? 6000 : 3200));
}

/**
 * Standard API error handler for `.catch()` chains.
 * Shows a toast with the error message. Optionally prepend a context label.
 * Usage: `.catch(U.handleApiError)` or `.catch((e) => U.handleApiError(e, 'Delete'))`
 */
export function handleApiError(e, context) {
  const msg = e && e.message ? e.message : String(e);
  toast(context ? context + ': ' + msg : msg, 'error');
}

export function copy(text, label) {
  const done = () => toast((label || 'Copied') + ' to clipboard', 'success');
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(text).then(done, () => toast('Copy failed', 'error'));
  } else {
    const ta = document.createElement('textarea');
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); done(); } catch (e) { toast('Copy failed', 'error'); }
    ta.remove();
  }
}

// ---- modal ----------------------------------------------------------------------

const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]), ' +
  'select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

// modal({ title, body, actions: [{label, kind, value, danger, onClick}], wide })
// Returns a promise resolving to the clicked action value, or null on dismiss.
// Traps focus inside the dialog and restores focus to the invoker on close.
export function modal(opts) {
  return new Promise((resolve) => {
    const invoker = document.activeElement;
    const backdrop = document.createElement('div');
    backdrop.className = 'modal-backdrop';
    const actions = opts.actions || [{ label: 'OK', value: true, kind: 'primary' }];
    backdrop.innerHTML =
      '<div class="modal' + (opts.wide ? ' wide' : '') + '" role="dialog" aria-modal="true" aria-label="' + esc(opts.title || 'Dialog') + '">' +
      '<div class="modal-head"><h3>' + esc(opts.title || '') + '</h3>' +
      '<button type="button" class="icon-btn modal-close" aria-label="Close">' + icon('x', 16) + '</button></div>' +
      '<div class="modal-body">' + (opts.body || '') + '</div>' +
      '<div class="modal-foot">' + actions.map((a, i) =>
        '<button type="button" class="btn ' + (a.kind || (a.danger ? 'danger' : 'ghost')) + '" data-action="' + i + '">' + esc(a.label) + '</button>'
      ).join('') + '</div></div>';
    document.body.appendChild(backdrop);

    let closed = false;
    function close(value) {
      if (closed) return;
      closed = true;
      backdrop.remove();
      document.removeEventListener('keydown', onKey, true);
      if (invoker && invoker.focus) invoker.focus();
      resolve(value);
    }
    function onKey(e) {
      if (e.key === 'Escape') { close(null); return; }
      if (e.key !== 'Tab') return;
      // Focus trap: wrap Tab / Shift+Tab inside the dialog.
      const items = Array.from(backdrop.querySelectorAll(FOCUSABLE))
        .filter((el) => el.offsetParent !== null);
      if (!items.length) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
    document.addEventListener('keydown', onKey, true);
    backdrop.addEventListener('mousedown', (e) => { if (e.target === backdrop) close(null); });
    backdrop.querySelector('.modal-close').addEventListener('click', () => close(null));
    backdrop.querySelectorAll('[data-action]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const a = actions[Number(btn.getAttribute('data-action'))];
        if (a.onClick) { a.onClick(close, btn); } else { close(a.value); }
      });
    });
    const firstBtn = backdrop.querySelector('.modal-foot .btn.primary, .modal-foot .btn.danger');
    if (firstBtn) firstBtn.focus();
  });
}

export function confirm(opts) {
  return modal({
    title: opts.title || 'Are you sure?',
    body: '<p class="modal-text">' + (opts.body || '') + '</p>',
    actions: [
      { label: opts.cancelLabel || 'Cancel', value: false, kind: 'ghost' },
      { label: opts.confirmLabel || 'Confirm', value: true, kind: opts.danger ? 'danger' : 'primary' }
    ]
  });
}

// ---- misc -----------------------------------------------------------------------

export function spinner(label) {
  return '<div class="loading" role="status"><span class="spinner"></span>' +
    (label ? '<span class="loading-label">' + esc(label) + '</span>' : '') + '</div>';
}

export function emptyState(opts) {
  return '<div class="empty-state">' +
    '<div class="empty-icon">' + icon(opts.icon || 'file', 28) + '</div>' +
    '<h3>' + esc(opts.title || 'Nothing here yet') + '</h3>' +
    (opts.body ? '<p>' + opts.body + '</p>' : '') +
    (opts.action || '') + '</div>';
}

export function errorState(msg, retryLabel) {
  return '<div class="empty-state">' +
    '<div class="empty-icon err">' + icon('alert', 28) + '</div>' +
    '<h3>Something went wrong</h3><p>' + esc(msg) + '</p>' +
    (retryLabel ? '<button type="button" class="btn ghost" data-retry>' + esc(retryLabel) + '</button>' : '') +
    '</div>';
}

// Highlight occurrences of query terms in already-escaped HTML text.
export function highlight(escapedText, query) {
  const terms = String(query || '').trim().split(/\s+/).filter((t) => t.length >= 2);
  if (!terms.length) return escapedText;
  const pattern = terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|');
  try {
    return escapedText.replace(new RegExp('(' + pattern + ')', 'gi'), '<mark>$1</mark>');
  } catch (e) {
    return escapedText;
  }
}
