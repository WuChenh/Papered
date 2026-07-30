// Prompts view: manage RAG system prompts used by Ask.
// Route: /prompts. Self-registers via route() on import.

import * as U from './util.js?v=2';
import { API } from './api.js?v=2';
import { route, navigate } from './router.js?v=2';

// ---- prompt card ---------------------------------------------------------

function promptCardHTML(p) {
  const footer =
    U.badge('temp ' + (typeof p.temperature === 'number' ? p.temperature.toFixed(2) : '—'), 'muted') +
    `<span class="muted small nowrap">${U.esc(U.relTime(p.updated_at))}</span>` +
    '<span class="spacer"></span>' +
    `<button type="button" class="icon-btn" data-use="${U.esc(p.id)}" title="Use in Ask">${U.icon('send', 15)}</button>` +
    (p.is_default ? '' :
      `<button type="button" class="icon-btn" data-default="${U.esc(p.id)}" title="Set default">${U.icon('star', 15)}</button>`) +
    `<button type="button" class="icon-btn" data-copy="${U.esc(p.id)}" title="Copy prompt">${U.icon('copy', 15)}</button>` +
    `<button type="button" class="icon-btn" data-edit="${U.esc(p.id)}" title="Edit">${U.icon('edit', 15)}</button>` +
    `<button type="button" class="icon-btn" data-delete="${U.esc(p.id)}"` +
      (p.is_default ? ' disabled title="The default prompt cannot be deleted"' : ' title="Delete"') +
      `>${U.icon('trash', 15)}</button>`;
  return '<div class="card">' +
    `<div class="card-head"><h3>${U.esc(p.name)}</h3>` +
    (p.is_default ? ' ' + U.badge('default', 'accent') : '') + '</div>' +
    (p.description ? `<p class="muted small">${U.esc(p.description)}</p>` : '') +
    '<pre class="snippet mono muted small pre-wrap snippet-scroll">' +
    U.esc(U.truncate(p.system_prompt || '', 220)) + '</pre>' +
    `<div class="toolbar mt-2">${footer}</div></div>`;
}

// ---- new / edit modal ----------------------------------------------------

function promptFormHTML(p) {
  const temp = p && typeof p.temperature === 'number' ? p.temperature : 0.2;
  return '<div class="field"><label for="p-name">Name</label>' +
    `<input class="input" id="p-name" value="${U.esc(p ? p.name : '')}" placeholder="e.g. Concise summarizer">` +
    '<div class="field-error" id="p-name-err"></div></div>' +
    '<div class="field"><label for="p-desc">Description <span class="muted">(optional)</span></label>' +
    `<input class="input" id="p-desc" value="${U.esc((p && p.description) || '')}" placeholder="What this prompt is for"></div>` +
    '<div class="field"><label for="p-system">System prompt</label>' +
    '<textarea class="textarea" id="p-system" rows="10" placeholder="You are a research assistant…">' +
    U.esc(p ? p.system_prompt : '') + '</textarea>' +
    '<div class="field-error" id="p-system-err"></div></div>' +
    '<div class="field"><label for="p-temp">Temperature: <span id="p-temp-val" class="mono">' +
    temp.toFixed(2) + '</span></label>' +
    `<input type="range" id="p-temp" class="range-block" min="0" max="1" step="0.05" value="${temp}">` +
    '<div class="field-hint">Lower is more deterministic; higher is more creative.</div></div>';
}

function openPromptModal(existing, onSaved) {
  U.modal({
    title: existing ? 'Edit prompt' : 'New prompt',
    wide: true,
    body: promptFormHTML(existing),
    actions: [
      { label: 'Cancel', value: null, kind: 'ghost' },
      {
        label: existing ? 'Save changes' : 'Create prompt',
        kind: 'primary',
        onClick: async (close, btn) => {
          const name = document.getElementById('p-name').value.trim();
          const desc = document.getElementById('p-desc').value.trim();
          const system = document.getElementById('p-system').value.trim();
          const temp = parseFloat(document.getElementById('p-temp').value);
          const nameErr = document.getElementById('p-name-err');
          const sysErr = document.getElementById('p-system-err');
          nameErr.textContent = name ? '' : 'Name is required.';
          sysErr.textContent = system ? '' : 'System prompt is required.';
          if (!name || !system) return;
          btn.disabled = true;
          const body = { name, description: desc || undefined, system_prompt: system, temperature: temp };
          try {
            if (existing) {
              await API.updatePrompt(existing.id, body);
            } else {
              await API.createPrompt(body);
            }
            close(true);
            U.toast(existing ? 'Prompt updated' : 'Prompt created', 'success');
            onSaved();
          } catch (e) {
            btn.disabled = false;
            sysErr.textContent = e.message;
          }
        }
      }
    ]
  });
  const slider = document.getElementById('p-temp');
  const val = document.getElementById('p-temp-val');
  if (slider && val) {
    slider.addEventListener('input', () => {
      val.textContent = parseFloat(slider.value).toFixed(2);
    });
  }
}

// ---- view ----------------------------------------------------------------

function renderPrompts(container) {
  let prompts = [];

  container.innerHTML =
    '<div class="page-head">' +
    '<div class="page-sub">System prompts used when answering questions in Ask.</div></div>' +
    `<div class="toolbar mb-4"><span class="spacer"></span>` +
    `<button type="button" class="btn primary" id="new-prompt">${U.icon('sparkles', 15)} New prompt</button></div>` +
    `<div id="prompts-body">${U.spinner('Loading prompts…')}</div>`;

  const body = () => container.querySelector('#prompts-body');

  function paint() {
    const el = body();
    if (!el) return;
    if (!prompts.length) {
      el.innerHTML = U.emptyState({
        icon: 'prompts',
        title: 'No prompts yet',
        body: 'Create a system prompt to steer how Ask answers questions over your library.',
        action: '<button type="button" class="btn primary" data-new>Create your first prompt</button>'
      });
      const b = el.querySelector('[data-new]');
      if (b) b.addEventListener('click', () => openPromptModal(null, load));
      return;
    }
    el.innerHTML = `<div class="grid cols-3">${prompts.map(promptCardHTML).join('')}</div>`;
    wireCards(el);
  }

  function wireCards(el) {
    el.querySelectorAll('[data-use]').forEach((b) => {
      b.addEventListener('click', () => {
        navigate('#/ask?prompt=' + encodeURIComponent(b.getAttribute('data-use')));
      });
    });
    el.querySelectorAll('[data-default]').forEach((b) => {
      b.addEventListener('click', async () => {
        const id = b.getAttribute('data-default');
        b.disabled = true;
        try {
          await API.setDefaultPrompt(id);
          U.toast('Default prompt updated', 'success');
          load();
        } catch (e) {
          b.disabled = false;
          U.toast(e.message, 'error');
        }
      });
    });
    el.querySelectorAll('[data-copy]').forEach((b) => {
      b.addEventListener('click', () => {
        const p = find(b.getAttribute('data-copy'));
        if (p) U.copy(p.system_prompt, 'Prompt');
      });
    });
    el.querySelectorAll('[data-edit]').forEach((b) => {
      b.addEventListener('click', () => {
        const p = find(b.getAttribute('data-edit'));
        if (p) openPromptModal(p, load);
      });
    });
    el.querySelectorAll('[data-delete]').forEach((b) => {
      b.addEventListener('click', async () => {
        const p = find(b.getAttribute('data-delete'));
        if (!p || p.is_default) return;
        const ok = await U.confirm({
          title: 'Delete prompt',
          body: `Delete prompt ‘${p.name}’? This cannot be undone.`,
          confirmLabel: 'Delete',
          danger: true
        });
        if (!ok) return;
        try {
          await API.deletePrompt(p.id);
          U.toast('Prompt deleted', 'success');
          load();
        } catch (e) {
          U.toast(e.message, 'error');
        }
      });
    });
  }

  const find = (id) => prompts.find((p) => String(p.id) === String(id)) || null;

  async function load() {
    const el = body();
    if (!el) return;
    el.innerHTML = U.spinner('Loading prompts…');
    try {
      const list = await API.listPrompts();
      prompts = list || [];
      paint();
    } catch (e) {
      const el2 = body();
      if (!el2) return;
      el2.innerHTML = U.errorState(e.message, 'Retry');
      const r = el2.querySelector('[data-retry]');
      if (r) r.addEventListener('click', load);
    }
  }

  const newBtn = container.querySelector('#new-prompt');
  if (newBtn) newBtn.addEventListener('click', () => openPromptModal(null, load));

  load();
}

route('/prompts', { title: 'Prompts', render: renderPrompts });
