// Ask view: chat interface over the RAG endpoint.
// Registered as route('/ask'). The message thread is kept in module state
// so it survives navigation within the session. Supports scoped mode via
// #/ask?paper=<id>, which restricts retrieval to a single paper.

import * as U from './util.js?v=1';
import { API } from './api.js?v=1';
import { route, navigate } from './router.js?v=1';
import { renderMarkdown } from './markdown.js?v=1';

// ---- session state ---------------------------------------------------------

const messages = []; // [{role, question?, answer?, sources?, search_method?, error?, pending?}]
let prompts = null; // cached prompt list
let scope = null; // { id, title } when asking within one paper
let gen = 0; // generation counter: bumped on destroy, stale callbacks bail out

const METHODS = [
  ['', 'Server default'],
  ['hybrid', 'Hybrid'],
  ['semantic', 'Semantic'],
  ['fulltext', 'Full-text']
];

const EXAMPLES = [
  'What methods are used for X?',
  'Summarize recent findings on Y',
  'What are the main limitations reported across these papers?',
  'Compare the results of the studies on Z'
];

// ---- controls card -----------------------------------------------------------

function controlsHTML() {
  const methodOpts = METHODS.map(([value, label]) =>
    '<option value="' + value + '">' + label + '</option>'
  ).join('');
  return '<div class="card"><div class="toolbar">' +
    '<div class="field mb-0"><label for="ask-prompt">Prompt</label>' +
    '<select class="select" id="ask-prompt"><option value="">Server default</option></select></div>' +
    '<div class="field mb-0"><label for="ask-method">Method</label>' +
    '<select class="select" id="ask-method">' + methodOpts + '</select></div>' +
    '<span class="spacer"></span><span id="ask-scope"></span>' +
    '</div></div>';
}

function scopeBannerHTML() {
  if (!scope) return '';
  return '<span class="chip" title="Retrieval is restricted to this paper">Asking within: ' +
    U.esc(U.truncate(scope.title, 50)) + ' ' +
    '<button type="button" class="icon-btn chip-x" id="ask-scope-clear" aria-label="Clear paper scope">' +
    U.icon('x', 12) + '</button></span>';
}

function paintScope(container) {
  const host = container.querySelector('#ask-scope');
  if (!host) return;
  host.innerHTML = scopeBannerHTML();
  const clear = host.querySelector('#ask-scope-clear');
  if (clear) {
    clear.addEventListener('click', () => {
      scope = null;
      navigate('#/ask');
    });
  }
}

function fillPrompts(container) {
  const sel = container.querySelector('#ask-prompt');
  if (!sel || !prompts) return;
  sel.innerHTML = '<option value="">Server default</option>' + prompts.map((p) =>
    '<option value="' + U.esc(p.id) + '">' + U.esc(p.name) +
    (p.is_default ? ' (default)' : '') + '</option>'
  ).join('');
}

// ---- thread rendering -----------------------------------------------------------

function citationChipsHTML(s) {
  const citations = s.citations || [];
  if (!citations.length) return '';
  // Chips are labeled by section path, but several chunks can share one path
  // (or fall back to the same generic label). Collapse duplicates so the row
  // stays scannable, keeping the first (highest-relevance) locator per label.
  const seen = new Set();
  const chips = [];
  for (const c of citations) {
    const label = c.section_path || (c.page_number != null ? 'p. ' + c.page_number : 'Passage');
    if (seen.has(label)) continue;
    seen.add(label);
    chips.push('<a class="chip" href="#/paper/' + encodeURIComponent(s.paper_id) +
      '/chunk/' + encodeURIComponent(c.chunk_id) + '">' + U.esc(label) + '</a>');
  }
  // Citations mean `content` is a verbatim source-text excerpt, not an
  // LLM summary — say so, and let the chips open the exact passage.
  return '<div class="citation-chips"><span class="muted small nowrap">' +
    U.icon('file', 12) + ' Original text:</span>' + chips.join('') + '</div>';
}

// The RAG `content` is the context block fed to the LLM, so it carries internal
// markers ("--- Paper: … ---" headers, "Section path:" lines, "[Page N]" tags,
// referenced-figure blocks, the truncation marker) that should not leak into
// the readable excerpt shown to the user. Strip them before display.
// These markers are produced by `build_rag_context_compact` in
// crates/papered/src/retrieval.rs and reach the UI via `RagSourceView.content`
// (crates/papered/src/llm/rag.rs) — if the context template changes there,
// update this filter to match.
function cleanSourceContent(raw) {
  return (raw || '')
    .split('\n')
    .filter((line) => {
      const t = line.trim();
      return !(
        (t.startsWith('---') && t.endsWith('---')) ||
        t.startsWith('Section path:') ||
        t.startsWith('[Referenced Figure') ||
        t === '...(truncated)'
      );
    })
    .join('\n')
    .replace(/\[Page \d+\]\s*/g, '')
    .replace(/\n{3,}/g, '\n\n')
    .trim();
}

function sourceHTML(s) {
  const meta = [];
  const authors = s.authors || [];
  if (authors.length) meta.push(authors.length > 2 ? authors[0] + ' et al.' : authors.join(' & '));
  const year = (s.published_date || '').slice(0, 4);
  if (year) meta.push(year);
  if (s.venue) meta.push(s.venue);
  return '<div class="chat-source">' +
    '<div class="source-head">' +
    '<a class="source-title" href="#/paper/' + encodeURIComponent(s.paper_id) + '">' +
    U.esc(s.title || 'Untitled') + '</a>' +
    (meta.length ? '<span class="muted small">' + meta.map(U.esc).join(' · ') + '</span>' : '') +
    '<span class="spacer"></span>' + U.badge(U.scorePct(s.score) + '%', 'accent') +
    '</div>' +
    '<div class="source-content">' + U.esc(U.truncate(cleanSourceContent(s.content), 300)) + '</div>' +
    citationChipsHTML(s) + '</div>';
}

function assistantBodyHTML(m) {
  if (m.pending) {
    return '<div class="chat-body">' + U.spinner('Searching and reading…') + '</div>';
  }
  if (m.error) {
    return '<div class="chat-body">' + U.badge('error', 'err') + ' ' +
      '<span>' + U.esc(m.error) + '</span> ' +
      '<button type="button" class="btn ghost sm" data-retry-ask>' + U.icon('refresh', 13) +
      ' Retry</button></div>';
  }
  let html = '<div class="chat-body md-rendered">' + renderMarkdown(m.answer) + '</div>';
  html += '<div class="muted small mt-1">Method: ' +
    U.esc(m.search_method || 'default') + ' · ' +
    (m.sources ? m.sources.length : 0) + (m.sources && m.sources.length === 1 ? ' source' : ' sources') +
    '</div>';
  if (m.sources && m.sources.length) {
    html += '<div class="chat-sources">' + m.sources.map(sourceHTML).join('') + '</div>';
  }
  return html;
}

function messageHTML(m, idx) {
  if (m.role === 'user') {
    return '<div class="chat-msg user"><div class="chat-avatar">' + U.icon('user', 15) + '</div>' +
      '<div class="chat-bubble">' + U.esc(m.question) + '</div></div>';
  }
  return '<div class="chat-msg assistant" data-msg="' + idx + '">' +
    '<div class="chat-avatar">' + U.icon('sparkles', 16) + '</div>' +
    '<div class="chat-bubble">' + assistantBodyHTML(m) + '</div></div>';
}

function welcomeHTML() {
  const chips = EXAMPLES.map((q) =>
    '<button type="button" class="chip" data-example="' + U.esc(q) + '">' + U.esc(q) + '</button>'
  ).join(' ');
  return '<div class="empty-state">' +
    '<div class="empty-icon">' + U.icon('sparkles', 28) + '</div>' +
    '<h3>Ask your library anything</h3>' +
    '<p>Answers are grounded in your indexed papers, with sources and citations.</p>' +
    '<p class="wide">' + chips + '</p></div>';
}

function paintThread(container) {
  const thread = container.querySelector('#chat-thread');
  if (!thread) return;
  if (!messages.length) {
    thread.innerHTML = welcomeHTML();
    return;
  }
  thread.innerHTML = messages.map(messageHTML).join('');
  thread.scrollTop = thread.scrollHeight;
}

// Re-paint a single assistant message in place (pending → answer/error).
function repaintMessage(container, idx) {
  const el = container.querySelector('[data-msg="' + idx + '"] .chat-bubble');
  if (el) el.innerHTML = assistantBodyHTML(messages[idx]);
  const thread = container.querySelector('#chat-thread');
  if (thread) thread.scrollTop = thread.scrollHeight;
}

// ---- sending ------------------------------------------------------------

function askBody(question, container) {
  const body = { question };
  const method = container.querySelector('#ask-method');
  const prompt = container.querySelector('#ask-prompt');
  if (method && method.value) body.search_method = method.value;
  if (prompt && prompt.value) body.prompt_id = prompt.value;
  if (scope) body.paper_id = scope.id;
  return body;
}

function setBusy(container, busy) {
  const input = container.querySelector('#ask-input');
  const send = container.querySelector('#ask-send');
  if (input) input.disabled = busy;
  if (send) send.disabled = busy;
}

async function send(container, question) {
  question = String(question || '').trim();
  if (!question) return;
  const myGen = gen;
  const input = container.querySelector('#ask-input');
  if (input) input.value = '';

  messages.push({ role: 'user', question });
  const idx = messages.length;
  messages.push({ role: 'assistant', pending: true, question });
  paintThread(container);
  setBusy(container, true);

  try {
    const res = await API.ask(askBody(question, container));
    if (myGen !== gen) return; // view destroyed — drop the result
    messages[idx] = {
      role: 'assistant',
      question,
      answer: res.answer,
      sources: res.sources || [],
      search_method: res.search_method
    };
    repaintMessage(container, idx);
  } catch (err) {
    if (myGen !== gen) return;
    messages[idx] = { role: 'assistant', question, error: err.message || 'Request failed' };
    repaintMessage(container, idx);
  } finally {
    if (myGen !== gen) return;
    setBusy(container, false);
    const ta = container.querySelector('#ask-input');
    if (ta) ta.focus();
  }
}

// ---- wiring ----------------------------------------------------------------

function wireInput(container) {
  const input = container.querySelector('#ask-input');
  const sendBtn = container.querySelector('#ask-send');

  sendBtn.addEventListener('click', () => send(container, input.value));
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send(container, input.value);
    }
  });

  // Delegated clicks: example chips and error retries live in the thread.
  container.addEventListener('click', (e) => {
    const example = e.target.closest('[data-example]');
    if (example) {
      input.value = example.getAttribute('data-example');
      input.focus();
      return;
    }
    const retry = e.target.closest('[data-retry-ask]');
    if (retry) {
      const msgEl = retry.closest('[data-msg]');
      const idx = msgEl ? Number(msgEl.getAttribute('data-msg')) : -1;
      const q = messages[idx] && messages[idx].question;
      if (q) {
        // Drop the failed exchange and resend the same question.
        messages.splice(idx - 1, 2);
        send(container, q);
      }
    }
  });
}

// ---- route -----------------------------------------------------------------

route('/ask', {
  title: 'Ask',
  render(container, ctx) {
    gen++;
    const myGen = gen;

    // Scoped mode: #/ask?paper=<id> restricts retrieval to one paper.
    const paperId = ctx.query.get('paper');
    if (paperId && (!scope || scope.id !== paperId)) {
      scope = null;
      API.getPaper(paperId).then((paper) => {
        if (myGen !== gen) return;
        scope = { id: paper.id, title: paper.title || 'Untitled' };
        paintScope(container);
      }).catch((err) => {
        if (myGen !== gen) return;
        U.toast('Could not load paper scope: ' + err.message, 'error');
      });
    } else if (!paperId) {
      scope = null;
    }

    container.innerHTML =
      '<div class="page-head">' +
      '<p class="page-sub">Chat with your library — answers cite the papers they come from</p></div>' +
      controlsHTML() +
      '<div class="chat-thread mt-4" id="chat-thread" aria-live="polite"></div>' +
      '<div class="chat-input-bar"><div class="chat-input-row">' +
      '<textarea id="ask-input" class="textarea" rows="1" aria-label="Ask a question about your papers" ' +
      'placeholder="Ask a question about your papers…"></textarea>' +
      '<button type="button" class="btn primary" id="ask-send">' + U.icon('send', 15) + ' Send</button>' +
      '</div></div>';

    paintScope(container);
    paintThread(container);
    wireInput(container);

    // Pre-select a prompt when arriving via #/ask?prompt=<id>.
    const wantedPrompt = ctx.query.get('prompt') || '';
    function applyPromptSelection() {
      if (!wantedPrompt) return;
      const sel = container.querySelector('#ask-prompt');
      if (sel) sel.value = wantedPrompt;
    }

    // Load prompt presets once per session.
    if (!prompts) {
      API.listPrompts().then((list) => {
        prompts = list || [];
        if (myGen !== gen) return;
        fillPrompts(container);
        applyPromptSelection();
      }).catch(() => { /* prompt select keeps the server-default entry */ });
    } else {
      fillPrompts(container);
      applyPromptSelection();
    }

    const input = container.querySelector('#ask-input');
    if (input) input.focus();

    return function destroy() {
      gen++; // in-flight ask/prompt/scope callbacks become no-ops
    };
  }
});
