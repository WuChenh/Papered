// First-run setup wizard: welcome hero + provider → key → providers → models
// → verify state machine. Exports renderWizard(container, onComplete);
// app.js calls it when the daemon reports needs_setup.

import * as U from './util.js?v=1';
import { API } from './api.js?v=1';

// ---- presets --------------------------------------------------------------

const PRESETS = {
  siliconflow: {
    name: 'SiliconFlow',
    desc: 'Hosted models, full stack',
    apiBase: 'https://api.siliconflow.cn/v1',
    needsKey: true,
    providerName: 'SiliconFlow',
    // Defaults mirror examples/config-minimal.toml.
    models: {
      chat: 'Qwen/Qwen3.6-35B-A3B',
      embedding: 'Qwen/Qwen3-VL-Embedding-8B',
      reranker: 'Qwen/Qwen3-VL-Reranker-8B'
    }
  },
  ollama: {
    name: 'Ollama',
    desc: 'Local models, no API key',
    apiBase: 'http://127.0.0.1:11434/v1',
    needsKey: false,
    providerName: 'Ollama',
    models: { chat: '', embedding: '', reranker: '' }
  },
  custom: {
    name: 'Custom',
    desc: 'Any OpenAI-compatible endpoint',
    apiBase: '',
    needsKey: true,
    providerName: 'Custom',
    models: { chat: '', embedding: '', reranker: '' }
  }
};

// ---- helpers ---------------------------------------------------------------

// True when an API base URL points at the local machine (no key needed).
function isLoopbackBase(apiBase) {
  let host = String(apiBase || '').trim().toLowerCase()
    .replace(/^[a-z][a-z0-9+.-]*:\/\//, '')
    .split('/')[0];
  if (host.charAt(0) === '[') {
    host = host.slice(1, host.indexOf(']'));
  } else if (host.indexOf(':') === host.lastIndexOf(':')) {
    host = host.split(':')[0];
  }
  return host === 'localhost' || host === '127.0.0.1' || host === '::1';
}

export function renderWizard(container, onComplete) {
  renderWelcome();

  // ---- welcome hero ----

  function renderWelcome() {
    container.innerHTML = `
      <div class="wizard-wrap wizard-hero">
        <div class="hero-icon">${U.icon('sparkles', 26)}</div>
        <h1 class="hero-title">Papered</h1>
        <p class="muted hero-sub">Your papers, searchable and chat-ready.</p>
        <div class="hero-features">
          <div class="kv-row row-nowrap mb-2">
            <span class="text-accent">${U.icon('file', 16)}</span>
            <span>Index PDFs &amp; images into a local knowledge base</span></div>
          <div class="kv-row row-nowrap mb-2">
            <span class="text-accent">${U.icon('search', 16)}</span>
            <span>Hybrid search across full text and embeddings</span></div>
          <div class="kv-row row-nowrap">
            <span class="text-accent">${U.icon('ask', 16)}</span>
            <span>Ask questions with precise citations</span></div>
        </div>
        <div><button type="button" class="btn primary" id="get-started">Get started</button></div>
        <p class="muted small mt-4">Takes about two minutes. Your key is
          written only to your local config.toml (permissions 0600); the daemon listens on
          127.0.0.1 and sends no telemetry.</p>
      </div>`;
    container.querySelector('#get-started').addEventListener('click', startWizard);
  }

  // ---- wizard state ----

  async function startWizard() {
    let current = {};
    try { current = (await API.getConfig()).config || {}; } catch (e) { /* keep empty */ }
    const w = {
      current: current,
      preset: null,
      providerKey: 'siliconflow',
      providerName: 'SiliconFlow',
      apiBase: '',
      needsKey: true,
      apiKey: '',
      chat: '',
      embedding: '',
      reranker: '',
      modelProvider: { chat: null, embedding: null, reranker: null },
      extraProviders: [],
      providerKeys: {},
      providerSeq: 0,
      targetLanguage: (current.translation || {}).target_language || 'zh-CN',
      step: 0
    };
    renderStep(w);
  }

  // Step 2 (API key) is skipped for keyless presets and for loopback
  // api_base URLs — a local endpoint needs no key.
  function flow(w) {
    return (w.needsKey && !isLoopbackBase(w.apiBase))
      ? ['provider', 'key', 'providers', 'models', 'language', 'verify']
      : ['provider', 'providers', 'models', 'language', 'verify'];
  }

  function renderStep(w) {
    const f = flow(w);
    const name = f[w.step] || 'provider';
    if (name === 'provider') stepProvider(w, f);
    else if (name === 'key') stepKey(w, f);
    else if (name === 'providers') stepProviders(w, f);
    else if (name === 'models') stepModels(w, f);
    else if (name === 'language') stepLanguage(w, f);
    else stepVerify(w, f);
  }

  function setStepError(msg) {
    const el = container.querySelector('#step-error');
    if (el) el.textContent = msg || '';
  }

  // ---- shell: progress segments + card + footer ----

  function shell(w, f, bodyHtml, footerHtml) {
    const segs = f.map((_, i) => {
      const cls = i < w.step ? ' done' : (i === w.step ? ' current' : '');
      return `<button type="button" class="step-seg${cls}"` +
        (i < w.step ? ` data-step-back="${i}"` : ' disabled') +
        (i === w.step ? ' aria-current="step"' : '') +
        ` aria-label="Step ${i + 1} of ${f.length}"></button>`;
    }).join('');
    container.innerHTML = `
      <div class="wizard-wrap">
        <div class="wizard-steps" role="group" aria-label="Setup progress">${segs}</div>
        <div class="card">${bodyHtml}</div>
        <p id="step-error" class="field-error" aria-live="polite"></p>
        <div class="kv-row mt-3">${footerHtml}</div>
      </div>`;
    container.querySelectorAll('[data-step-back]').forEach((seg) => {
      seg.addEventListener('click', () => {
        collectModels(w);
        w.step = Number(seg.getAttribute('data-step-back'));
        renderStep(w);
      });
    });
    const card = container.querySelector('.card');
    if (card) {
      // Enter in a text input triggers the step's primary button.
      card.addEventListener('keydown', (e) => {
        if (e.key !== 'Enter') return;
        const t = e.target;
        if (!t || t.tagName !== 'INPUT') return;
        const type = (t.getAttribute('type') || 'text').toLowerCase();
        if (['text', 'password', 'url', 'number'].indexOf(type) < 0) return;
        const primary = container.querySelector('#next') || container.querySelector('#save') ||
          container.querySelector('#get-started');
        if (primary && !primary.disabled) {
          e.preventDefault();
          primary.click();
        }
      });
    }
  }

  function wireBack(w) {
    const back = container.querySelector('#back');
    if (back) {
      back.addEventListener('click', () => {
        collectModels(w);
        w.step -= 1;
        renderStep(w);
      });
    }
  }

  // ---- state collection ----

  function roleProviderKey(w, role) {
    return w.modelProvider[role] || w.providerKey;
  }

  // All providers selectable in the Models step: the primary provider first,
  // then providers from the fetched config, then providers added this session.
  function providerOptions(w) {
    const opts = [{ key: w.providerKey, label: w.providerName, apiBase: w.apiBase }];
    Object.keys(w.current.providers || {}).forEach((k) => {
      if (k === w.providerKey) return;
      const p = w.current.providers[k];
      opts.push({
        key: k, label: k, apiBase: p.api_base || '',
        keyless: !p.api_key && !isLoopbackBase(p.api_base || '')
      });
    });
    w.extraProviders.forEach((p) => {
      opts.push({ key: p.id, label: p.id, apiBase: p.apiBase || '' });
    });
    return opts;
  }

  // Resolve the concrete { apiBase, apiKey } backing a role — used by the
  // verify step's tests.
  function providerForRole(w, role) {
    const key = roleProviderKey(w, role);
    if (key === w.providerKey) return { apiBase: w.apiBase, apiKey: resolvedKey(w) };
    const existing = (w.current.providers || {})[key];
    if (existing) {
      return { apiBase: existing.api_base, apiKey: w.providerKeys[key] || existing.api_key || null };
    }
    const extra = w.extraProviders.filter((p) => p.id === key)[0];
    if (extra) return { apiBase: extra.apiBase, apiKey: extra.apiKey || null };
    return { apiBase: w.apiBase, apiKey: resolvedKey(w) };
  }

  // An existing provider referenced by a role that has no key and is not a
  // loopback endpoint — the wizard must collect a key for it before saving.
  function keylessProviderFor(w, role) {
    const key = roleProviderKey(w, role);
    if (key === w.providerKey) return null;
    const existing = (w.current.providers || {})[key];
    if (!existing || existing.api_key || isLoopbackBase(existing.api_base || '')) return null;
    return { key: key };
  }

  function resolvedKey(w) {
    const existing = (w.current.providers && w.current.providers[w.providerKey]) || {};
    return w.apiKey || existing.api_key || null;
  }

  // Read every editable wizard field back into `w` — model rows, provider
  // cards, and the primary provider's fields. Safe to call on any step; all
  // lookups are guarded, so fields not on the current step are skipped.
  function collectModels(w) {
    const chat = container.querySelector('#inp-chat');
    const emb = container.querySelector('#inp-embedding');
    const rerank = container.querySelector('#inp-reranker');
    if (chat) w.chat = chat.value.trim();
    if (emb) w.embedding = emb.value.trim();
    if (rerank) w.reranker = rerank.value.trim();
    const pn = container.querySelector('#inp-pname');
    const pb = container.querySelector('#inp-pbase');
    const pk = container.querySelector('#inp-pkey');
    if (pn) w.providerName = pn.value.trim();
    if (pb) w.apiBase = pb.value.trim();
    // A blank key field means "unchanged", never "clear": the same key can be
    // rendered under more than one role/step across re-renders.
    if (pk && pk.value.trim()) w.apiKey = pk.value.trim();
    container.querySelectorAll('[data-role-provider]').forEach((sel) => {
      const role = sel.getAttribute('data-role-provider');
      w.modelProvider[role] = sel.value === w.providerKey ? null : sel.value;
    });
    container.querySelectorAll('[data-pid]').forEach((card) => {
      const id = card.getAttribute('data-pid');
      const p = w.extraProviders.filter((x) => x.id === id)[0];
      if (!p) return;
      card.querySelectorAll('[data-pf]').forEach((inp) => {
        p[inp.getAttribute('data-pf')] = inp.value.trim();
      });
    });
    // Only non-empty values are stored: the same provider's key field can be
    // rendered under more than one role across re-renders, and an empty field
    // must not clobber a value already captured from another one.
    container.querySelectorAll('[data-provider-key]').forEach((inp) => {
      const v = inp.value.trim();
      if (v) w.providerKeys[inp.getAttribute('data-provider-key')] = v;
    });
  }

  // Append a session provider with a collision-safe id (providerSeq restarts
  // each session, so a plain counter can collide with saved providers).
  function addExtraProvider(w) {
    let id;
    do {
      w.providerSeq += 1;
      id = 'ui-provider-' + w.providerSeq;
    } while ((w.current.providers || {})[id] ||
      w.extraProviders.some((p) => p.id === id));
    w.extraProviders.push({ id: id, apiBase: '', apiKey: '' });
    return id;
  }

  // ---- step 1: provider preset ----

  function stepProvider(w, f) {
    const cards = Object.keys(PRESETS).map((key) => {
      const p = PRESETS[key];
      return `<button type="button" class="preset-card${w.preset === key ? ' selected' : ''}"` +
        ` data-preset="${key}" aria-pressed="${w.preset === key}">` +
        `<div class="preset-name">${U.esc(p.name)}</div>` +
        `<div class="preset-desc">${U.esc(p.desc)}</div></button>`;
    }).join('');
    shell(w, f,
      `<div class="card-head"><h3>Choose a provider</h3></div>
       <div class="preset-grid">${cards}</div>
       <div class="field${w.preset === 'custom' ? '' : ' hidden'}" id="apibase-field">
         <label for="inp-apibase">API base URL</label>
         <input class="input" id="inp-apibase" type="url" value="${U.esc(w.apiBase)}"
           placeholder="https://api.example.com/v1"></div>`,
      '<button type="button" class="btn ghost" id="cancel">Cancel</button>' +
      '<span class="spacer"></span>' +
      `<button type="button" class="btn primary" id="next"${w.preset ? '' : ' disabled'}>Continue</button>`);
    container.querySelectorAll('[data-preset]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const key = btn.getAttribute('data-preset');
        const p = PRESETS[key];
        w.preset = key;
        w.providerKey = key;
        w.providerName = p.providerName;
        w.apiBase = p.apiBase;
        w.needsKey = p.needsKey;
        // Prefill sensible model defaults from the preset.
        w.chat = w.chat || p.models.chat;
        w.embedding = w.embedding || p.models.embedding;
        w.reranker = w.reranker || p.models.reranker;
        renderStep(w);
      });
    });
    container.querySelector('#cancel').addEventListener('click', renderWelcome);
    container.querySelector('#next').addEventListener('click', () => {
      if (!w.preset) return;
      if (w.preset === 'custom') {
        const base = container.querySelector('#inp-apibase').value.trim();
        if (!base) { setStepError('Enter the API base URL.'); return; }
        w.apiBase = base;
      }
      w.step += 1;
      renderStep(w);
    });
  }

  // ---- step 2: API key (skipped for keyless / loopback) ----

  function stepKey(w, f) {
    const existing = (w.current.providers && w.current.providers[w.providerKey]) || {};
    const hasSaved = !!existing.api_key;
    shell(w, f,
      `<div class="card-head"><h3>API key</h3></div>
       <div class="field"><label for="inp-key">${U.esc(w.providerName)} API key</label>
         <input class="input" id="inp-key" type="password" value="${U.esc(w.apiKey)}"
           placeholder="${hasSaved ? 'Saved — leave blank to keep' : 'sk-...'}"
           autocomplete="off"></div>
       <p class="field-hint">Stored only in your local config.toml (permissions 0600).</p>`,
      '<button type="button" class="btn ghost" id="back">Back</button>' +
      '<span class="spacer"></span>' +
      '<button type="button" class="btn primary" id="next">Continue</button>');
    wireBack(w);
    container.querySelector('#next').addEventListener('click', () => {
      w.apiKey = container.querySelector('#inp-key').value.trim();
      if (!w.apiKey && !hasSaved) { setStepError('Enter your API key.'); return; }
      w.step += 1;
      renderStep(w);
    });
  }

  // ---- step 3: providers (primary + session-only extras) ----

  function extraProviderEditors(w) {
    if (!w.extraProviders.length) return '';
    return w.extraProviders.map((p) => `
      <div class="provider-card" data-pid="${U.esc(p.id)}">
        <div class="kv-row"><strong>New provider</strong><span class="spacer"></span>
          <button type="button" class="btn ghost sm" data-remove-provider="${U.esc(p.id)}">Remove</button></div>
        <div class="field"><label for="extra-base-${U.esc(p.id)}">API base URL</label>
          <input class="input" type="url" id="extra-base-${U.esc(p.id)}" data-pf="apiBase"
            value="${U.esc(p.apiBase)}" placeholder="https://api.example.com/v1"></div>
        <div class="field mb-0"><label for="extra-key-${U.esc(p.id)}">API key</label>
          <input class="input" type="password" id="extra-key-${U.esc(p.id)}" data-pf="apiKey"
            value="${U.esc(p.apiKey)}" placeholder="sk-..." autocomplete="off"></div>
      </div>`).join('');
  }

  function stepProviders(w, f) {
    const existing = (w.current.providers && w.current.providers[w.providerKey]) || {};
    const keyPlaceholder = existing.api_key
      ? 'Saved — leave blank to keep'
      : (w.needsKey ? 'sk-...' : 'optional for local endpoints');
    shell(w, f,
      `<div class="card-head"><h3>Providers</h3></div>
       <p class="field-hint mb-3">Add every provider you want to use —
         for example DeepSeek for chat and SiliconFlow for embeddings. Models are assigned in
         the next step.</p>
       <div class="provider-card">
         <div class="kv-row"><strong>${U.esc(w.providerName || 'Provider')}</strong>
           ${U.badge('primary', 'accent')}</div>
         <div class="field"><label for="inp-pname">Name</label>
           <input class="input" id="inp-pname" type="text" value="${U.esc(w.providerName)}"></div>
         <div class="field"><label for="inp-pbase">API base URL</label>
           <input class="input" id="inp-pbase" type="url" value="${U.esc(w.apiBase)}"></div>
         <div class="field mb-0"><label for="inp-pkey">API key</label>
           <input class="input" id="inp-pkey" type="password" value="${U.esc(w.apiKey)}"
             placeholder="${keyPlaceholder}" autocomplete="off"></div></div>
       <div class="mt-2"></div>
       ${extraProviderEditors(w)}
       <button type="button" class="btn ghost sm mt-2" id="add-wiz-provider">
         ${U.icon('plus', 14)} Add another provider</button>`,
      '<button type="button" class="btn ghost" id="back">Back</button>' +
      '<span class="spacer"></span>' +
      '<button type="button" class="btn primary" id="next">Continue</button>');
    wireBack(w);

    container.querySelector('#add-wiz-provider').addEventListener('click', () => {
      collectModels(w);
      addExtraProvider(w);
      renderStep(w);
    });
    container.querySelectorAll('[data-remove-provider]').forEach((btn) => {
      btn.addEventListener('click', () => {
        collectModels(w);
        const id = btn.getAttribute('data-remove-provider');
        w.extraProviders = w.extraProviders.filter((p) => p.id !== id);
        ['chat', 'embedding', 'reranker'].forEach((role) => {
          if (w.modelProvider[role] === id) w.modelProvider[role] = null;
        });
        renderStep(w);
      });
    });
    container.querySelector('#next').addEventListener('click', () => {
      collectModels(w);
      if (!w.providerName || !w.apiBase) {
        setStepError('Enter the primary provider name and API base URL.');
        return;
      }
      if (!resolvedKey(w) && !isLoopbackBase(w.apiBase)) {
        setStepError('The primary provider needs an API key.');
        return;
      }
      const incomplete = w.extraProviders.filter((p) =>
        !p.apiBase || (!p.apiKey && !isLoopbackBase(p.apiBase)))[0];
      if (incomplete) {
        setStepError("Complete every added provider's API base URL and API key, or remove it.");
        return;
      }
      w.step += 1;
      renderStep(w);
    });
  }

  // ---- step 4: models per role ----

  function modelRow(w, role, label, value, placeholder, note, keyField) {
    const selected = roleProviderKey(w, role);
    const options = providerOptions(w).map((o) =>
      `<option value="${U.esc(o.key)}"${o.key === selected ? ' selected' : ''}>` +
      U.esc(o.label) + (o.apiBase ? ' — ' + U.esc(o.apiBase) : '') +
      (o.keyless ? ' (needs API key)' : '') + '</option>').join('');
    const keyHtml = keyField
      ? `<div class="field mb-0"><label for="provkey-${U.esc(keyField.key)}">API key for
           ${U.esc(keyField.key)}</label>
         <input class="input" type="password" id="provkey-${U.esc(keyField.key)}"
           data-provider-key="${U.esc(keyField.key)}"
           value="${U.esc(w.providerKeys[keyField.key] || '')}"
           placeholder="sk-..." autocomplete="off"></div>`
      : '';
    return `<div class="model-card">
      <div class="grid cols-2">
        <div class="field"><label for="sel-${role}">${U.esc(label)} provider</label>
          <select class="select" id="sel-${role}" data-role-provider="${role}">${options}</select></div>
        <div class="field"><label for="inp-${role}">${U.esc(label)}${note ? ' ' + note : ''}</label>
          <input class="input" id="inp-${role}" type="text" value="${U.esc(value)}"
            placeholder="${U.esc(placeholder)}"></div>
      </div>${keyHtml}</div>`;
  }

  function stepModels(w, f) {
    // Show the key field for a keyless existing provider only under the
    // first role that references it, so it never asks twice.
    const claimed = {};
    function rowFor(role, label, value, placeholder, note) {
      const kp = keylessProviderFor(w, role);
      const showKey = kp && !claimed[kp.key] ? kp : null;
      if (showKey) claimed[showKey.key] = true;
      return modelRow(w, role, label, value, placeholder, note, showKey);
    }
    shell(w, f,
      `<div class="card-head"><h3>Models</h3></div>
       ${rowFor('chat', 'Chat model', w.chat, 'model name', '')}
       <div class="mt-2"></div>
       ${rowFor('embedding', 'Embedding model', w.embedding, 'model name', '')}
       <div class="mt-2"></div>
       ${rowFor('reranker', 'Reranker model', w.reranker,
        'leave blank to reuse the embedding endpoint', '<span class="muted">(optional)</span>')}
       <p class="field-hint mt-3">Each role can use a different provider.
         You can adjust these later in Settings.</p>`,
      '<button type="button" class="btn ghost" id="back">Back</button>' +
      '<span class="spacer"></span>' +
      '<button type="button" class="btn primary" id="next">Continue</button>');
    wireBack(w);
    container.querySelectorAll('[data-role-provider]').forEach((sel) => {
      sel.addEventListener('change', () => {
        collectModels(w);
        renderStep(w);
      });
    });
    container.querySelector('#next').addEventListener('click', () => {
      collectModels(w);
      if (!w.chat || !w.embedding) {
        setStepError('Chat and embedding models are required.');
        return;
      }
      const keylessRole = ['chat', 'embedding', 'reranker'].filter((role) => {
        const p = providerForRole(w, role);
        return !p.apiKey && !isLoopbackBase(p.apiBase);
      })[0];
      if (keylessRole) {
        setStepError('The ' + keylessRole +
          ' provider needs an API key — enter it in the key field for that provider, or choose another provider.');
        return;
      }
      w.step += 1;
      renderStep(w);
    });
  }

  // ---- step 4b: language preference ----

  const WIZARD_LANGUAGES = [
    { value: 'zh-CN', label: 'Chinese (Simplified)' },
    { value: 'en', label: 'English' },
    { value: 'fr', label: 'French' },
    { value: 'de', label: 'German' },
    { value: 'es', label: 'Spanish' },
    { value: 'ru', label: 'Russian' }
  ];

  function stepLanguage(w, f) {
    const langOpts = WIZARD_LANGUAGES.map((l) =>
      `<option value="${U.esc(l.value)}"${l.value === w.targetLanguage ? ' selected' : ''}>${U.esc(l.label)}</option>`
    ).join('');
    const isCustom = !WIZARD_LANGUAGES.some((l) => l.value === w.targetLanguage);
    shell(w, f,
      `<div class="card-head"><h3>Preferred language</h3></div>
       <p class="field-hint mb-3">Choose the language for translating papers. You can change this later in Settings.</p>
       <div class="field">
         <label class="label" for="wiz-lang">Target language</label>
         <select class="select select-md" id="wiz-lang">${langOpts}
           <option value="_custom"${isCustom ? ' selected' : ''}>Custom…</option>
         </select>
       </div>
       <div class="field${isCustom ? '' : ' hidden'}" id="wiz-lang-custom-field">
         <label class="label" for="wiz-lang-custom">Custom language code</label>
         <input class="input input-md" id="wiz-lang-custom" value="${isCustom ? U.esc(w.targetLanguage) : ''}"
           placeholder="e.g. pt-BR, ru, ar">
       </div>`,
      '<button type="button" class="btn ghost" id="back">Back</button>' +
      '<span class="spacer"></span>' +
      '<button type="button" class="btn primary" id="next">Continue</button>');
    wireBack(w);
    const sel = container.querySelector('#wiz-lang');
    const customField = container.querySelector('#wiz-lang-custom-field');
    sel.addEventListener('change', () => {
      if (sel.value === '_custom') {
        customField.classList.remove('hidden');
      } else {
        customField.classList.add('hidden');
        w.targetLanguage = sel.value;
      }
    });
    container.querySelector('#next').addEventListener('click', () => {
      if (sel.value === '_custom') {
        const custom = container.querySelector('#wiz-lang-custom');
        w.targetLanguage = (custom.value.trim()) || 'zh-CN';
      } else {
        w.targetLanguage = sel.value;
      }
      w.step += 1;
      renderStep(w);
    });
  }

  // ---- step 5: verify + save ----

  function stepVerify(w, f) {
    // One endpoint test per provider referenced by a role, plus embedding
    // and reranker model tests.
    const providerKeys = [];
    ['chat', 'embedding', 'reranker'].forEach((role) => {
      const k = roleProviderKey(w, role);
      if (providerKeys.indexOf(k) < 0) providerKeys.push(k);
    });
    // Resolve each referenced provider once, for the endpoint tests.
    const providerTargets = {};
    providerKeys.forEach((k) => {
      if (k === w.providerKey) {
        providerTargets[k] = { apiBase: w.apiBase, apiKey: resolvedKey(w) };
      } else {
        const existing = (w.current.providers || {})[k];
        const extra = w.extraProviders.filter((p) => p.id === k)[0];
        if (existing) {
          providerTargets[k] = {
            apiBase: existing.api_base,
            apiKey: w.providerKeys[k] || existing.api_key || null
          };
        } else if (extra) {
          providerTargets[k] = { apiBase: extra.apiBase, apiKey: extra.apiKey || null };
        } else {
          providerTargets[k] = { apiBase: w.apiBase, apiKey: resolvedKey(w) };
        }
      }
    });
    const rows = providerKeys.map((k) =>
      verifyRowHtml('provider:' + k, 'Provider: ' + k, 'Endpoint')).join('') +
      verifyRowHtml('embedding', 'Embedding model', w.embedding) +
      verifyRowHtml('reranker', 'Reranker model', w.reranker || w.embedding + ' (embedding endpoint)');

    shell(w, f,
      `<div class="card-head"><h3>Test your connection</h3></div>${rows}`,
      '<button type="button" class="btn ghost" id="back">Back</button>' +
      '<span class="spacer"></span>' +
      '<button type="button" class="btn primary" id="save">Save and continue</button>');
    wireBack(w);

    function verifyRowHtml(id, label, detail) {
      return `<div class="verify-row">
        <div class="flex-1"><strong>${U.esc(label)}</strong>
          <div class="muted small mono">${U.esc(detail)}</div>
          <div class="field-hint" data-verify-result="${U.esc(id)}" role="status">Not tested</div></div>
        <button type="button" class="btn ghost sm" data-verify-test="${U.esc(id)}">Re-test</button></div>`;
    }

    function resultEl(id) {
      return container.querySelector('[data-verify-result="' + id + '"]');
    }

    async function runTest(id) {
      const res = resultEl(id);
      if (!res) return;
      res.innerHTML = '<span class="spinner small"></span> Testing…';
      try {
        if (id.indexOf('provider:') === 0) {
          const prov = providerTargets[id.slice('provider:'.length)];
          const r = await API.testEndpoint({ api_base: prov.apiBase, api_key: prov.apiKey });
          res.innerHTML = r.reachable
            ? U.badge('Connected' + (r.http_status ? ' · HTTP ' + r.http_status : ''), 'ok') +
              (r.warning ? ` <span class="small text-warn">${U.esc(r.warning)}</span>` : '')
            : U.badge('Failed', 'err') +
              ` <span class="small text-err">${U.esc(r.error || 'Unreachable')}</span>`;
        } else if (id === 'embedding') {
          const ep = providerForRole(w, 'embedding');
          if (!ep.apiKey && !isLoopbackBase(ep.apiBase)) {
            res.innerHTML = U.badge('Failed', 'err') +
              ' <span class="small text-err">No API key configured for this provider.</span>';
            return;
          }
          const re = await API.testEmbedding({
            api_base: ep.apiBase, api_key: ep.apiKey, model: w.embedding
          });
          res.innerHTML = re.status === 'ok'
            ? U.badge('Connected · ' + U.fmtInt(re.dimension) + ' dimensions', 'ok')
            : U.badge('Failed', 'err') +
              ` <span class="small text-err">${U.esc(re.error || 'Failed')}</span>`;
        } else {
          const rp = providerForRole(w, 'reranker');
          if (!rp.apiKey && !isLoopbackBase(rp.apiBase)) {
            res.innerHTML = U.badge('Failed', 'err') +
              ' <span class="small text-err">No API key configured for this provider.</span>';
            return;
          }
          const rr = await API.testReranker({
            api_base: rp.apiBase, api_key: rp.apiKey, model: w.reranker || w.embedding
          });
          res.innerHTML = rr.status === 'ok'
            ? U.badge('Connected', 'ok')
            : U.badge('Failed', 'err') +
              ` <span class="small text-err">${U.esc(rr.error || 'Failed')}</span>`;
        }
      } catch (e) {
        res.innerHTML = U.badge('Failed', 'err') +
          ` <span class="small text-err">${U.esc(e.message)}</span>`;
      }
    }

    container.querySelectorAll('[data-verify-test]').forEach((btn) => {
      btn.addEventListener('click', () => {
        runTest(btn.getAttribute('data-verify-test'));
      });
    });
    // Auto-run every test on entering the step.
    providerKeys.forEach((k) => { runTest('provider:' + k); });
    runTest('embedding');
    runTest('reranker');

    container.querySelector('#save').addEventListener('click', async () => {
      const btn = container.querySelector('#save');
      btn.disabled = true;
      btn.textContent = 'Saving…';
      setStepError('');
      try {
        // Merge onto the freshest config (and use its ETag) so the PUT
        // round-trips the whole AppConfig and rarely conflicts.
        const fresh = await API.getConfig();
        w.current = fresh.config || {};
        await API.putConfig(buildConfig(w), fresh.etag);
        onComplete();
      } catch (e) {
        btn.disabled = false;
        btn.textContent = 'Save and continue';
        setStepError(e.message + ' — click Save and continue to retry.');
      }
    });
  }

  // ---- build the AppConfig body ----

  function buildConfig(w) {
    const current = w.current && typeof w.current === 'object' ? w.current : {};
    const existingProvider = (current.providers && current.providers[w.providerKey]) || {};
    const primary = { api_base: w.apiBase };
    const key = w.apiKey || existingProvider.api_key || null;
    if (key) primary.api_key = key;

    const providers = Object.assign({}, current.providers || {});
    providers[w.providerKey] = primary;
    w.extraProviders.forEach((p) => {
      providers[p.id] = { api_base: p.apiBase, api_key: p.apiKey };
    });
    // Keys entered for existing keyless providers in the Models step. Copy
    // the provider object before overlaying so w.current stays untouched.
    // The primary provider is skipped: its key is already resolved
    // authoritatively above.
    Object.keys(w.providerKeys).forEach((k) => {
      if (k === w.providerKey) return;
      if (w.providerKeys[k] && providers[k]) {
        providers[k] = Object.assign({}, providers[k], { api_key: w.providerKeys[k] });
      }
    });

    const models = Object.assign({}, current.models || {}, {
      'ui-chat': { provider: roleProviderKey(w, 'chat'), model: w.chat },
      'ui-embedding': { provider: roleProviderKey(w, 'embedding'), model: w.embedding },
      'ui-reranker': { provider: roleProviderKey(w, 'reranker'), model: w.reranker || w.embedding }
    });
    // PurposesConfig has exactly six fields: embedding, reranker, section,
    // rag, vision, enhancement. The old UI's main/fast assignments were
    // silently dropped server-side — do not set them.
    const purposes = Object.assign({}, current.purposes || {}, {
      section: 'ui-chat',
      rag: 'ui-chat',
      embedding: 'ui-embedding',
      reranker: 'ui-reranker'
    });
    return Object.assign({}, current, {
      providers: providers,
      models: models,
      purposes: purposes,
      translation: Object.assign({}, current.translation || {}, { target_language: w.targetLanguage || 'zh-CN' })
    });
  }
}
