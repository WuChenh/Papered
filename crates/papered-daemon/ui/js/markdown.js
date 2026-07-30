// Markdown + LaTeX math rendering for paper content.
// Pipeline: protect code → extract math → marked → DOMPurify → KaTeX.
// Vendor libs are embedded alongside this file (no CDN, works offline).

import { marked } from './vendor/marked.esm.js?v=2';
import katex from './vendor/katex.mjs?v=2';
import DOMPurify from './vendor/purify.es.mjs?v=2';
import { esc } from './util.js?v=2';

// ---- configuration ----------------------------------------------------------

marked.setOptions({ gfm: true, breaks: true });

const PURIFY_CFG = {
  USE_PROFILES: { html: true, mathMl: true },
  ADD_ATTR: ['aria-hidden', 'aria-label', 'role']
};

const KATEX_OPTS = {
  throwOnError: false,
  strict: false,
  trust: false,
  maxSize: 500 // px — guard against pathological layouts
};

// ---- math extraction ----------------------------------------------------------

// Segment kinds: 'text' | 'code' | 'math'
// Code is protected first so `$` inside code fences/inline code is never
// treated as a math delimiter.

const FENCE_RE = /^(```[\s\S]*?```|~~~[\s\S]*?~~~)/gm;
const INLINE_CODE_RE = /(`[^`\n]+`)/g;

// Display: $$...$$ (greedy across newlines) and \[...\]
const DISPLAY_MATH_RE = /(\$\$[\s\S]+?\$\$|\\\[[\s\S]+?\\\])/g;
// Inline: $...$ and \(...\). Guards against currency like "between $5 and
// $10": the opening $ must not be followed by whitespace or a digit, the
// content must not end in whitespace, and the closing $ must not be
// immediately followed by a digit.
const INLINE_MATH_RE = /(\$(?![\s\d])[^\$\n]+?(?<!\s)\$(?!\d)|\\\([\s\S]+?\\\))/g;

function segmentize(text) {
  const segments = [];
  let rest = text;

  // Pass 1 — split out fenced code blocks.
  const codeParts = [];
  let last = 0;
  for (const m of rest.matchAll(FENCE_RE)) {
    if (m.index > last) codeParts.push({ t: 'text', v: rest.slice(last, m.index) });
    codeParts.push({ t: 'code', v: m[0] });
    last = m.index + m[0].length;
  }
  if (last < rest.length) codeParts.push({ t: 'text', v: rest.slice(last) });
  if (!codeParts.length) codeParts.push({ t: 'text', v: rest });

  // Pass 2 — within text parts, split out inline code.
  const noCode = [];
  for (const seg of codeParts) {
    if (seg.t !== 'text') { noCode.push(seg); continue; }
    let l2 = 0;
    for (const m of seg.v.matchAll(INLINE_CODE_RE)) {
      if (m.index > l2) noCode.push({ t: 'text', v: seg.v.slice(l2, m.index) });
      noCode.push({ t: 'code', v: m[0] });
      l2 = m.index + m[0].length;
    }
    if (l2 < seg.v.length) noCode.push({ t: 'text', v: seg.v.slice(l2) });
  }

  // Pass 3 — within remaining text, extract display math then inline math.
  for (const seg of noCode) {
    if (seg.t !== 'text') { segments.push(seg); continue; }
    let l3 = 0;
    const s = seg.v;
    // Merge display + inline into one pass by scanning display first.
    const mathParts = [];
    for (const m of s.matchAll(DISPLAY_MATH_RE)) {
      mathParts.push({ idx: m.index, len: m[0].length, v: m[0], display: true });
    }
    for (const m of s.matchAll(INLINE_MATH_RE)) {
      // Skip if overlapping with an already-found display block.
      const overlaps = mathParts.some((d) => m.index < d.idx + d.len && m.index + m[0].length > d.idx);
      if (!overlaps) mathParts.push({ idx: m.index, len: m[0].length, v: m[0], display: false });
    }
    mathParts.sort((a, b) => a.idx - b.idx);
    for (const mp of mathParts) {
      if (mp.idx > l3) segments.push({ t: 'text', v: s.slice(l3, mp.idx) });
      segments.push({ t: 'math', v: mp.v, display: mp.display });
      l3 = mp.idx + mp.len;
    }
    if (l3 < s.length) segments.push({ t: 'text', v: s.slice(l3) });
  }
  return segments;
}

// Strip the surrounding delimiters from a raw math token.
function mathBody(raw, display) {
  if (raw.startsWith('$$')) return raw.slice(2, -2);
  if (raw.startsWith('\\[')) return raw.slice(2, -2);
  if (raw.startsWith('\\(')) return raw.slice(2, -2);
  if (raw.startsWith('$')) return raw.slice(1, -1);
  return raw;
}

// ---- rendering ----------------------------------------------------------

/**
 * Render Markdown text (with LaTeX math) to sanitized HTML.
 * Safe to assign to innerHTML. Plain-text input is fine too — it still gets
 * math rendering, and stray Markdown syntax is largely harmless.
 */
export function renderMarkdown(text) {
  const src = String(text ?? '');
  if (!src.trim()) return '';

  const segments = segmentize(src);

  // Build protected source: math → placeholder tokens (Unicode brackets,
  // virtually never occur in paper text) that survive marked + DOMPurify.
  const mathList = [];
  let protectedSrc = '';
  for (const seg of segments) {
    if (seg.t === 'math') {
      const i = mathList.length;
      mathList.push(seg);
      protectedSrc += `⟦M${i}⟧`;
    } else {
      protectedSrc += seg.v;
    }
  }

  let html = marked.parse(protectedSrc);
  html = DOMPurify.sanitize(html, PURIFY_CFG);

  // Replace placeholders with KaTeX-rendered HTML.
  if (mathList.length) {
    html = html.replace(/⟦M(\d+)⟧/g, (_, idx) => {
      const seg = mathList[Number(idx)];
      if (!seg) return '';
      try {
        return katex.renderToString(mathBody(seg.v, seg.display), {
          ...KATEX_OPTS,
          displayMode: seg.display
        });
      } catch {
        // Unparseable TeX — show the raw source styled as code.
        return `<code class="math-fallback">${esc(seg.v)}</code>`;
      }
    });
  }
  return html;
}
