// Settings editor: faithful draft editor for providers / models / purposes,
// plus data management (paths, reset). Registers the /settings route.

import * as U from './util.js?v=2';
import { API } from './api.js?v=2';
import { route, setLeaveGuard, clearLeaveGuard } from './router.js?v=2';

// ---- helpers -----------------------------------------------------------

const deepClone = (o) => JSON.parse(JSON.stringify(o));

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

const KEY_RE = /^[a-z0-9][a-z0-9-]*$/;

// PurposesConfig on the server has exactly these six fields — nothing else
// (the old UI's main/fast selects were silently dropped server-side).
const PURPOSE_LABELS = {
  embedding: 'Text and image embeddings',
  reranker: 'Result reranking',
  section: 'Section analysis during indexing',
  rag: 'Answer generation',
  vision: 'Image description (optional)',
  enhancement: 'Query enhancement — rewriting + HyDE (optional)',
  translation: 'Translation (optional — defaults to answer generation model)'
};
const CORE_PURPOSES = ['embedding', 'reranker', 'section', 'rag'];
const ADVANCED_PURPOSES = ['vision', 'enhancement', 'translation'];

// ---- route --------------------------------------------------------------

route('/settings', {
  title: 'Settings',
  render(container) {
    const st = {
      snapshot: null,   // last saved config from the server
      draft: null,      // deep clone the user edits
      etag: null,
      renames: { providers: {}, models: {} }, // oldKey -> newKey
      destroyed: false,
      guardSet: false
    };

    container.innerHTML = U.spinner('Loading settings…');
    load();

    function load() {
      API.getConfig().then((r) => {
        if (st.destroyed) return;
        st.snapshot = r.config;
        st.etag = r.etag;
        st.draft = deepClone(r.config);
        st.renames = { providers: {}, models: {} };
        paint();
      }).catch((e) => {
        if (st.destroyed) return;
        container.innerHTML = U.errorState(e.message, 'Retry');
        const retry = container.querySelector('[data-retry]');
        if (retry) retry.addEventListener('click', () => {
          container.innerHTML = U.spinner('Loading settings…');
          load();
        });
      });
    }

    // ---- dirty tracking / leave guard ----

    function isDirty() {
      if (!st.snapshot || !st.draft) return false;
      return JSON.stringify(st.draft.providers) !== JSON.stringify(st.snapshot.providers) ||
        JSON.stringify(st.draft.models) !== JSON.stringify(st.snapshot.models) ||
        JSON.stringify(st.draft.purposes) !== JSON.stringify(st.snapshot.purposes) ||
        JSON.stringify(st.draft.translation) !== JSON.stringify(st.snapshot.translation);
    }

    function guard() {
      if (!isDirty()) return true;
      return window.confirm('Discard unsaved changes?');
    }

    function updateGuard() {
      if (isDirty() && !st.guardSet) {
        setLeaveGuard(guard);
        st.guardSet = true;
      } else if (!isDirty() && st.guardSet) {
        clearLeaveGuard();
        st.guardSet = false;
      }
    }

    function clearGuard() {
      if (st.guardSet) {
        clearLeaveGuard();
        st.guardSet = false;
      }
    }

    function updateSaveBar() {
      const bar = container.querySelector('#save-bar');
      if (bar) bar.classList.toggle('hidden', !isDirty());
      updateGuard();
    }

    // ---- paint ------------------------------------------------------------

    function paint() {
      if (st.destroyed || !st.draft) return;
      container.innerHTML = `
        <div class="settings-nav">
          <button type="button" class="chip" data-scroll="sec-providers">Providers</button>
          <button type="button" class="chip" data-scroll="sec-models">Models</button>
          <button type="button" class="chip" data-scroll="sec-purposes">Purposes</button>
          <button type="button" class="chip" data-scroll="sec-translation">Translation</button>
          <button type="button" class="chip" data-scroll="sec-data">Data</button>
        </div>
        ${providersSectionHtml()}
        ${modelsSectionHtml()}
        ${purposesSectionHtml()}
        ${translationSectionHtml()}
        ${dataSectionHtml()}
        <div class="save-bar${isDirty() ? '' : ' hidden'}" id="save-bar">
          <span class="small muted">Unsaved changes</span>
          <span class="spacer"></span>
          <button type="button" class="btn ghost" id="settings-revert">Revert</button>
          <button type="button" class="btn primary" id="settings-save">Save</button>
        </div>`;
      wireNav();
      wireProvidersSection();
      wireModelsSection();
      wirePurposesSection();
      wireTranslationSection();
      wireDataSection();
      container.querySelector('#settings-revert').addEventListener('click', () => {
        st.draft = deepClone(st.snapshot);
        st.renames = { providers: {}, models: {} };
        paint();
        updateGuard();
      });
      container.querySelector('#settings-save').addEventListener('click', save);
      updateGuard();
    }

    function wireNav() {
      container.querySelectorAll('[data-scroll]').forEach((btn) => {
        btn.addEventListener('click', () => {
          const el = container.querySelector('#' + btn.getAttribute('data-scroll'));
          if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' });
        });
      });
    }

    // ---- providers section ----

    function providerReferencedBy(providerKey) {
      return Object.keys(st.draft.models || {}).filter((mk) =>
        st.draft.models[mk].provider === providerKey);
    }

    function providersSectionHtml() {
      const providers = st.draft.providers || {};
      const cards = Object.keys(providers).map((k) => {
        const p = providers[k];
        const hasKey = !!p.api_key;
        const cleared = !!p.api_key_cleared;
        const refs = providerReferencedBy(k);
        const delAttrs = refs.length
          ? ` disabled title="Referenced by: ${U.esc(refs.join(', '))}"` : '';
        const placeholder = hasKey && !cleared ? 'Saved — leave blank to keep' : 'sk-...';
        return `
          <div class="provider-card" data-provider="${U.esc(k)}">
            <div class="kv-row">
              <input class="input mono input-md" data-field="key" value="${U.esc(k)}"
                aria-label="Provider key">
              ${cleared ? U.badge('key cleared on save', 'warn') : ''}
              <span class="spacer"></span>
              ${hasKey && !cleared
                ? `<button type="button" class="btn ghost sm" data-clear-key="${U.esc(k)}">Clear key</button>`
                : ''}
              <button type="button" class="btn ghost sm" data-del-provider="${U.esc(k)}"${delAttrs}>Delete</button>
            </div>
            <div class="field">
              <label class="label" for="prov-base-${U.esc(k)}">API base URL</label>
              <input class="input" type="url" id="prov-base-${U.esc(k)}" data-field="api_base"
                value="${U.esc(p.api_base || '')}" placeholder="https://api.example.com/v1">
            </div>
            <div class="field mb-0">
              <label class="label" for="prov-secret-${U.esc(k)}">API key</label>
              <input class="input" type="password" id="prov-secret-${U.esc(k)}" data-field="api_key"
                value="" placeholder="${placeholder}" autocomplete="off">
              <div class="field-hint">${cleared
                ? 'The stored key will be removed when you save.'
                : 'Blank keeps the stored key; entering a value replaces it.'}</div>
            </div>
          </div>`;
      }).join('');
      return `
        <section id="sec-providers">
          <h3>Providers</h3>
          ${cards}
          <button type="button" class="btn ghost sm mt-2" id="add-provider">${U.icon('plus', 14)} Add provider</button>
        </section>`;
    }

    function wireProvidersSection() {
      container.querySelectorAll('[data-provider]').forEach((card) => {
        const origKey = card.getAttribute('data-provider');
        card.querySelectorAll('[data-field]').forEach((inp) => {
          inp.addEventListener('change', () => {
            const field = inp.getAttribute('data-field');
            const providers = st.draft.providers;
            if (field === 'key') {
              const newKey = inp.value.trim();
              if (!KEY_RE.test(newKey) || (providers[newKey] && newKey !== origKey)) {
                inp.value = origKey;
                U.toast('Keys must match [a-z0-9-] and be unique.', 'error');
                return;
              }
              if (newKey === origKey) return;
              providers[newKey] = providers[origKey];
              delete providers[origKey];
              st.renames.providers[origKey] = newKey;
              // Cascade the rename into every model referencing this provider.
              Object.keys(st.draft.models || {}).forEach((mk) => {
                if (st.draft.models[mk].provider === origKey) {
                  st.draft.models[mk].provider = newKey;
                }
              });
              paint();
              return;
            }
            if (field === 'api_key') {
              providers[origKey].api_key = inp.value; // blank = keep stored
              delete providers[origKey].api_key_cleared;
            } else {
              providers[origKey][field] = inp.value.trim();
            }
            updateSaveBar();
          });
        });
      });
      container.querySelectorAll('[data-clear-key]').forEach((btn) => {
        btn.addEventListener('click', () => {
          const p = st.draft.providers[btn.getAttribute('data-clear-key')];
          delete p.api_key;
          p.api_key_cleared = true;
          paint();
        });
      });
      container.querySelectorAll('[data-del-provider]').forEach((btn) => {
        btn.addEventListener('click', () => {
          delete st.draft.providers[btn.getAttribute('data-del-provider')];
          paint();
        });
      });
      const add = container.querySelector('#add-provider');
      if (add) {
        add.addEventListener('click', () => {
          let n = 1;
          while (st.draft.providers['provider-' + n]) n += 1;
          st.draft.providers['provider-' + n] = { api_base: '' };
          paint();
        });
      }
    }

    // ---- models section ----

    function modelUsage(modelKey) {
      return Object.keys(st.draft.purposes || {}).filter((p) =>
        st.draft.purposes[p] === modelKey);
    }

    // Which test endpoint a model exercises, by the purpose referencing it.
    function testKindFor(modelKey) {
      const usage = modelUsage(modelKey);
      if (usage.indexOf('embedding') >= 0) return 'embedding';
      if (usage.indexOf('reranker') >= 0) return 'reranker';
      return 'chat';
    }

    // Resolve a draft provider key back to its snapshot entry, following
    // the rename chain (renames.providers maps oldKey -> newKey).
    function snapshotProvider(k) {
      let cur = k;
      for (;;) {
        const snap = (st.snapshot.providers || {})[cur];
        if (snap) return snap;
        let orig = null;
        Object.keys(st.renames.providers).forEach((o) => {
          if (st.renames.providers[o] === cur && o !== cur) orig = o;
        });
        if (!orig) return null;
        cur = orig;
      }
    }

    // Effective { apiBase, apiKey } for a provider: an entered key wins
    // over the stored one; a cleared key wins over both (returns null).
    function providerResolution(providerKey) {
      const draft = (st.draft.providers || {})[providerKey] || {};
      const snap = snapshotProvider(providerKey) || {};
      const key = draft.api_key_cleared ? null : (draft.api_key || snap.api_key || null);
      return { apiBase: draft.api_base || '', apiKey: key };
    }

    function extraBodyText(m) {
      if (m.extra_body === undefined || m.extra_body === null) return '';
      return typeof m.extra_body === 'string'
        ? m.extra_body
        : JSON.stringify(m.extra_body, null, 2);
    }

    function modelsSectionHtml() {
      const models = st.draft.models || {};
      const providerKeys = Object.keys(st.draft.providers || {});
      const cards = Object.keys(models).map((k) => {
        const m = models[k];
        const usage = modelUsage(k);
        const usageBadge = usage.length
          ? U.badge('used by: ' + usage.join(', '), 'info')
          : U.badge('unused', 'muted');
        const providerOpts = providerKeys.map((pk) =>
          `<option value="${U.esc(pk)}"${pk === m.provider ? ' selected' : ''}>${U.esc(pk)}</option>`
        ).join('');
        const delAttrs = usage.length
          ? ` disabled title="Referenced by: ${U.esc(usage.join(', '))}"` : '';
        const effort = m.reasoning_effort || '';
        const effortOpts = ['low', 'medium', 'high'].map((v) =>
          `<option value="${v}"${effort === v ? ' selected' : ''}>${v}</option>`
        ).join('');
        return `
          <div class="model-card" data-model="${U.esc(k)}">
            <div class="kv-row">
              <input class="input mono input-md" data-mfield="key" value="${U.esc(k)}"
                aria-label="Model key">
              ${usageBadge}
              <span class="spacer"></span>
              <button type="button" class="btn ghost sm" data-test-model="${U.esc(k)}">Test</button>
              <button type="button" class="btn ghost sm" data-del-model="${U.esc(k)}"${delAttrs}>Delete</button>
            </div>
            <div class="grid cols-2">
              <div class="field">
                <label class="label" for="m-provider-${U.esc(k)}">Provider</label>
                <select class="select" id="m-provider-${U.esc(k)}" data-mfield="provider">${providerOpts}</select>
              </div>
              <div class="field">
                <label class="label" for="m-name-${U.esc(k)}">Model name</label>
                <input class="input" id="m-name-${U.esc(k)}" data-mfield="model" value="${U.esc(m.model || '')}">
              </div>
            </div>
            <div class="field-hint" data-test-result="${U.esc(k)}" role="status">${m.extra_body_invalid
              ? '<span class="field-error">Invalid JSON in extra body — fix before saving.</span>'
              : ''}</div>
            <details class="advanced">
              <summary>Advanced</summary>
              <div class="grid cols-2">
                <div class="field">
                  <label class="label" for="m-conc-${U.esc(k)}">Concurrency (0 = default)</label>
                  <input class="input" type="number" min="0" id="m-conc-${U.esc(k)}"
                    data-mfield="concurrency" value="${U.esc(m.concurrency || 0)}">
                </div>
                <div class="field">
                  <label class="label" for="m-rpm-${U.esc(k)}">Requests / min (0 = unlimited)</label>
                  <input class="input" type="number" min="0" id="m-rpm-${U.esc(k)}"
                    data-mfield="rpm" value="${U.esc(m.rpm || 0)}">
                </div>
                <div class="field">
                  <label class="label" for="m-tpm-${U.esc(k)}">Tokens / min (0 = unlimited)</label>
                  <input class="input" type="number" min="0" id="m-tpm-${U.esc(k)}"
                    data-mfield="tpm" value="${U.esc(m.tpm || 0)}">
                </div>
                <div class="field">
                  <label class="label" for="m-ctx-${U.esc(k)}">Context window (blank = unset)</label>
                  <input class="input" type="number" min="0" id="m-ctx-${U.esc(k)}"
                    data-mfield="context_window" value="${U.esc(m.context_window == null ? '' : m.context_window)}">
                </div>
                <div class="field">
                  <label class="label" for="m-maxout-${U.esc(k)}">Max output tokens (blank = unset)</label>
                  <input class="input" type="number" min="0" id="m-maxout-${U.esc(k)}"
                    data-mfield="max_output_tokens" value="${U.esc(m.max_output_tokens == null ? '' : m.max_output_tokens)}">
                </div>
                <div class="field">
                  <label class="label" for="m-effort-${U.esc(k)}">Reasoning effort</label>
                  <select class="select" id="m-effort-${U.esc(k)}" data-mfield="reasoning_effort">
                    <option value=""${effort === '' ? ' selected' : ''}>— unset —</option>
                    ${effortOpts}
                  </select>
                </div>
              </div>
              <div class="field mb-0">
                <label class="label" for="m-extra-${U.esc(k)}">Extra body (JSON, blank = unset)</label>
                <textarea class="textarea${m.extra_body_invalid ? ' invalid' : ''}" id="m-extra-${U.esc(k)}"
                  data-mfield="extra_body" rows="3" spellcheck="false">${U.esc(extraBodyText(m))}</textarea>
              </div>
            </details>
          </div>`;
      }).join('');
      return `
        <section class="section" id="sec-models">
          <h3>Models</h3>
          ${cards}
          <button type="button" class="btn ghost sm mt-2" id="add-model">${U.icon('plus', 14)} Add model</button>
        </section>`;
    }

    const numOrNull = (v) => (v === '' ? null : Number(v));

    function wireModelsSection() {
      container.querySelectorAll('[data-model]').forEach((card) => {
        const origKey = card.getAttribute('data-model');
        card.querySelectorAll('[data-mfield]').forEach((inp) => {
          const field = inp.getAttribute('data-mfield');
          // extra_body validates live on input without a repaint, so the
          // textarea keeps focus and its content while typing.
          if (field === 'extra_body') {
            inp.addEventListener('input', () => {
              const m = st.draft.models[origKey];
              const raw = inp.value.trim();
              const hint = card.querySelector('[data-test-result]');
              if (!raw) {
                delete m.extra_body;
                delete m.extra_body_invalid;
              } else {
                try {
                  m.extra_body = JSON.parse(raw);
                  delete m.extra_body_invalid;
                } catch (e) {
                  m.extra_body_invalid = true;
                }
              }
              inp.classList.toggle('invalid', !!m.extra_body_invalid);
              if (hint) {
                hint.innerHTML = m.extra_body_invalid
                  ? '<span class="field-error">Invalid JSON in extra body — fix before saving.</span>'
                  : '';
              }
              updateSaveBar();
            });
            return;
          }
          inp.addEventListener('change', () => {
            const models = st.draft.models;
            if (field === 'key') {
              const newKey = inp.value.trim();
              if (!KEY_RE.test(newKey) || (models[newKey] && newKey !== origKey)) {
                inp.value = origKey;
                U.toast('Keys must match [a-z0-9-] and be unique.', 'error');
                return;
              }
              if (newKey === origKey) return;
              models[newKey] = models[origKey];
              delete models[origKey];
              st.renames.models[origKey] = newKey;
              // Cascade the rename into every purpose referencing this model.
              Object.keys(st.draft.purposes || {}).forEach((p) => {
                if (st.draft.purposes[p] === origKey) st.draft.purposes[p] = newKey;
              });
              paint();
              return;
            }
            const m = models[origKey];
            if (field === 'provider' || field === 'model') {
              m[field] = inp.value.trim();
            } else if (field === 'concurrency' || field === 'rpm' || field === 'tpm') {
              m[field] = Number(inp.value) || 0;
            } else if (field === 'context_window' || field === 'max_output_tokens') {
              const n = numOrNull(inp.value.trim());
              if (n === null) delete m[field]; else m[field] = n;
            } else if (field === 'reasoning_effort') {
              if (!inp.value) delete m.reasoning_effort; else m.reasoning_effort = inp.value;
            }
            updateSaveBar();
          });
        });
      });

      container.querySelectorAll('[data-test-model]').forEach((btn) => {
        btn.addEventListener('click', async () => {
          const k = btn.getAttribute('data-test-model');
          const res = container.querySelector(`[data-test-result="${k}"]`);
          const m = st.draft.models[k];
          const prov = providerResolution(m.provider);
          if (!prov.apiKey && !isLoopbackBase(prov.apiBase)) {
            res.innerHTML = '<span class="field-error">No API key configured for this provider.</span>';
            return;
          }
          btn.disabled = true;
          res.innerHTML = '<span class="spinner small"></span> Testing…';
          const kind = testKindFor(k);
          try {
            if (kind === 'embedding') {
              const r = await API.testEmbedding({
                api_base: prov.apiBase, api_key: prov.apiKey, model: m.model
              });
              res.innerHTML = r.status === 'ok'
                ? U.badge(`Connected · ${U.fmtInt(r.dimension)} dimensions`, 'ok')
                : `<span class="field-error">${U.esc(r.error || 'Failed')}</span>`;
            } else if (kind === 'reranker') {
              const rr = await API.testReranker({
                api_base: prov.apiBase, api_key: prov.apiKey, model: m.model
              });
              res.innerHTML = rr.status === 'ok'
                ? U.badge('Connected', 'ok')
                : `<span class="field-error">${U.esc(rr.error || 'Failed')}</span>`;
            } else {
              const rc = await API.testEndpoint({ api_base: prov.apiBase, api_key: prov.apiKey });
              res.innerHTML = rc.reachable
                ? U.badge('Connected' + (rc.http_status ? ` · HTTP ${rc.http_status}` : ''), 'ok') +
                  (rc.warning ? ` <span class="small text-warn">${U.esc(rc.warning)}</span>` : '')
                : `<span class="field-error">${U.esc(rc.error || 'Unreachable')}</span>`;
            }
          } catch (e) {
            res.innerHTML = `<span class="field-error">${U.esc(e.message)}</span>`;
          } finally {
            btn.disabled = false;
          }
        });
      });

      container.querySelectorAll('[data-del-model]').forEach((btn) => {
        btn.addEventListener('click', () => {
          delete st.draft.models[btn.getAttribute('data-del-model')];
          paint();
        });
      });
      const add = container.querySelector('#add-model');
      if (add) {
        add.addEventListener('click', () => {
          const firstProvider = Object.keys(st.draft.providers || {})[0] || '';
          let n = 1;
          while (st.draft.models['model-' + n]) n += 1;
          st.draft.models['model-' + n] = {
            provider: firstProvider, model: '', concurrency: 0, rpm: 0, tpm: 0
          };
          paint();
        });
      }
    }

    // ---- purposes section ----

    function purposeRowHtml(purpose, modelKeys) {
      const current = (st.draft.purposes || {})[purpose] || '';
      const opts = '<option value="">—</option>' + modelKeys.map((mk) =>
        `<option value="${U.esc(mk)}"${mk === current ? ' selected' : ''}>${U.esc(mk)}</option>`
      ).join('');
      return `
        <div class="kv-row mb-2">
          <span class="flex-label"><strong>${U.esc(purpose)}</strong>
            <span class="muted small">${U.esc(PURPOSE_LABELS[purpose] || '')}</span></span>
          <select class="select select-md" data-purpose="${U.esc(purpose)}"
            aria-label="${U.esc(purpose)}">${opts}</select>
        </div>`;
    }

    function purposesSectionHtml() {
      const modelKeys = Object.keys(st.draft.models || {});
      const core = CORE_PURPOSES.map((p) => purposeRowHtml(p, modelKeys)).join('');
      const adv = ADVANCED_PURPOSES.map((p) => purposeRowHtml(p, modelKeys)).join('');
      return `
        <section class="section" id="sec-purposes">
          <h3>Purposes</h3>
          ${core}
          <details class="advanced"><summary>Advanced purposes</summary>${adv}</details>
        </section>`;
    }

    function wirePurposesSection() {
      container.querySelectorAll('[data-purpose]').forEach((sel) => {
        sel.addEventListener('change', () => {
          const p = sel.getAttribute('data-purpose');
          if (!sel.value) delete st.draft.purposes[p];
          else st.draft.purposes[p] = sel.value;
          // Repaint: model usage badges reflect purpose assignments.
          paint();
        });
      });
    }

    // ---- translation section ----

    const LANGUAGE_OPTIONS = [
      { value: 'zh-CN', label: 'Chinese (Simplified)' },
      { value: 'en', label: 'English' },
      { value: 'fr', label: 'French' },
      { value: 'de', label: 'German' },
      { value: 'es', label: 'Spanish' },
      { value: 'ru', label: 'Russian' }
    ];

    function translationSectionHtml() {
      const t = st.draft.translation || {};
      const currentLang = t.target_language || 'zh-CN';
      const langOpts = LANGUAGE_OPTIONS.map((l) =>
        `<option value="${U.esc(l.value)}"${l.value === currentLang ? ' selected' : ''}>${U.esc(l.label)}</option>`
      ).join('');
      const isCustom = !LANGUAGE_OPTIONS.some((l) => l.value === currentLang);
      return `
        <section class="section" id="sec-translation">
          <h3>Translation</h3>
          <div class="field">
            <label class="label" for="translation-lang">Target language</label>
            <select class="select select-md" id="translation-lang" data-tfield="target_language">${langOpts}
              <option value="_custom"${isCustom ? ' selected' : ''}>Custom…</option>
            </select>
          </div>
          <div class="field${isCustom ? '' : ' hidden'}" id="custom-lang-field">
            <label class="label" for="translation-lang-custom">Custom language code</label>
            <input class="input input-md" id="translation-lang-custom" value="${isCustom ? U.esc(currentLang) : ''}"
              placeholder="e.g. pt-BR, ru, ar">
          </div>
          <p class="field-hint">The translation model is configured under Advanced purposes above. When unset, the answer generation model is used.</p>
        </section>`;
    }

    function wireTranslationSection() {
      const sel = container.querySelector('#translation-lang');
      const customField = container.querySelector('#custom-lang-field');
      if (!sel) return;
      sel.addEventListener('change', () => {
        if (sel.value === '_custom') {
          customField && customField.classList.remove('hidden');
          // Keep the current value until the user types something.
        } else {
          customField && customField.classList.add('hidden');
          if (!st.draft.translation) st.draft.translation = {};
          st.draft.translation.target_language = sel.value;
          updateSaveBar();
        }
      });
      const customInput = container.querySelector('#translation-lang-custom');
      if (customInput) {
        customInput.addEventListener('change', () => {
          if (!st.draft.translation) st.draft.translation = {};
          st.draft.translation.target_language = customInput.value.trim() || 'zh-CN';
          updateSaveBar();
        });
      }
    }

    // ---- data section ----

    function dataSectionHtml() {
      return `
        <section class="section" id="sec-data">
          <h3>Data</h3>
          <dl class="meta-grid">
            <dt>Data directory</dt>
            <dd class="kv-row"><span class="mono" id="data-dir">…</span>
              <button type="button" class="icon-btn bordered" data-copy-path="data-dir"
                aria-label="Copy data directory">${U.icon('copy', 14)}</button></dd>
            <dt>Config file</dt>
            <dd class="kv-row"><span class="mono" id="config-path">…</span>
              <button type="button" class="icon-btn bordered" data-copy-path="config-path"
                aria-label="Copy config path">${U.icon('copy', 14)}</button></dd>
          </dl>
          <hr class="divider">
          <p class="field-hint">Permanently delete the index database (all papers, vectors, figures and tables) and derived data. This cannot be undone.</p>
          <button type="button" class="btn danger sm" id="reset-btn">Reset data…</button>
        </section>`;
    }

    function wireDataSection() {
      API.stats().then((s) => {
        const dd = container.querySelector('#data-dir');
        const cp = container.querySelector('#config-path');
        if (dd) dd.textContent = s.data_dir || '';
        if (cp) cp.textContent = s.config_path || '';
      }).catch(() => { /* paths stay as ellipsis */ });
      container.querySelectorAll('[data-copy-path]').forEach((btn) => {
        btn.addEventListener('click', () => {
          const el = container.querySelector('#' + btn.getAttribute('data-copy-path'));
          if (el && el.textContent && el.textContent !== '…') U.copy(el.textContent, 'Path');
        });
      });
      const btn = container.querySelector('#reset-btn');
      if (btn) btn.addEventListener('click', openResetDialog);
    }

    // ---- reset data dialog ----

    function openResetDialog() {
      let includeAll = false;
      let preview = null;
      const promise = U.modal({
        title: 'Reset data',
        wide: true,
        body: U.spinner('Loading preview…'),
        actions: [
          { label: 'Cancel', value: null, kind: 'ghost' },
          {
            label: 'Delete permanently', kind: 'danger',
            onClick: (close, btn) => { doDelete(close, btn); }
          }
        ]
      });
      promise.catch(() => { /* U.modal never rejects */ });
      const backdrops = document.querySelectorAll('.modal-backdrop');
      const backdrop = backdrops[backdrops.length - 1];
      const body = backdrop.querySelector('.modal-body');

      function paintPreview() {
        if (!preview) {
          body.innerHTML = U.spinner('Loading preview…');
          return;
        }
        const paths = preview.removed_paths || [];
        body.innerHTML = `
          <p class="modal-text">This will permanently remove ${U.fmtInt(paths.length)} paths, freeing ${U.fmtBytes(preview.bytes_freed)}.</p>
          ${preview.message ? `<p class="modal-text muted small">${U.esc(preview.message)}</p>` : ''}
          ${paths.length
            ? `<div class="mono small muted scroll-box mt-2 mb-2">${paths.map((p) => U.esc(p)).join('<br>')}</div>`
            : ''}
          <p class="modal-text small text-err">The index database (all papers, vectors, figures and tables) is also cleared. This cannot be undone.</p>
          <label class="checkbox-row"><input type="checkbox" id="reset-all"${includeAll ? ' checked' : ''}>
            Also remove logs and cache</label>`;
        body.querySelector('#reset-all').addEventListener('change', (e) => {
          fetchPreview(e.target.checked);
        });
      }

      function fetchPreview(all) {
        body.innerHTML = U.spinner('Loading preview…');
        API.resetData(false, all).then((r) => {
          preview = r;
          includeAll = all;
          paintPreview();
        }).catch((e) => {
          body.innerHTML = `<p class="modal-text text-err">${U.esc(e.message)}</p>`;
        });
      }

      function doDelete(close, btn) {
        btn.disabled = true;
        btn.textContent = 'Deleting…';
        API.resetData(true, includeAll).then((r) => {
          close(true);
          U.modal({
            title: 'Data reset complete',
            body: `<p class="modal-text">${U.esc(r.message || 'Data reset complete.')}</p>
              <p class="modal-text muted small">Reload this page to see updated stats.</p>`,
            actions: [{ label: 'OK', value: true, kind: 'primary' }]
          });
        }).catch((e) => {
          btn.disabled = false;
          btn.textContent = 'Delete permanently';
          U.toast(e.message, 'error');
        });
      }

      fetchPreview(false);
    }

    // ---- save ----

    // Identity of the configured embedding model, compared as provider/model
    // so a pure model-key rename does not trigger the re-embed confirmation.
    function resolvedEmbeddingLabel(cfg) {
      const key = (cfg.purposes || {}).embedding;
      const m = key && (cfg.models || {})[key];
      if (!m) return '';
      return (m.provider || '') + '/' + (m.model || '');
    }

    function validate() {
      const providers = st.draft.providers || {};
      const models = st.draft.models || {};
      const badProvider = Object.keys(providers).filter((k) => !KEY_RE.test(k));
      if (badProvider.length) {
        return `Provider keys must match [a-z0-9-]: '${badProvider[0]}'.`;
      }
      const badModel = Object.keys(models).filter((k) => !KEY_RE.test(k));
      if (badModel.length) {
        return `Model keys must match [a-z0-9-]: '${badModel[0]}'.`;
      }
      const invalid = Object.keys(models).filter((k) => models[k].extra_body_invalid);
      if (invalid.length) {
        return `Model '${invalid[0]}' has invalid JSON in extra body.`;
      }
      return null;
    }

    // Applies pending key renames with cascading reference updates, then
    // returns the PUT body (full AppConfig, round-tripping untouched parts).
    function buildPayload() {
      const d = deepClone(st.draft);
      Object.keys(d.providers || {}).forEach((k) => {
        const p = d.providers[k];
        const cleared = !!p.api_key_cleared;
        if (cleared) delete p.api_key;
        delete p.api_key_cleared;
        // The password input starts empty and only writes through on change:
        // undefined = untouched, '' = user emptied the field — both mean
        // "keep the stored key", restored from the snapshot. A cleared key
        // (Clear key button) stays removed.
        if (!cleared && (p.api_key === undefined || p.api_key === '')) {
          const snap = snapshotProvider(k);
          const snapKey = (snap || {}).api_key;
          if (snapKey) p.api_key = snapKey; else delete p.api_key;
        }
      });
      Object.keys(st.renames.providers).forEach((oldKey) => {
        const newKey = st.renames.providers[oldKey];
        if (oldKey === newKey) return;
        Object.keys(d.models || {}).forEach((mk) => {
          if (d.models[mk].provider === oldKey) d.models[mk].provider = newKey;
        });
      });
      Object.keys(st.renames.models).forEach((oldKey) => {
        const newKey = st.renames.models[oldKey];
        if (oldKey === newKey) return;
        Object.keys(d.purposes || {}).forEach((p) => {
          if (d.purposes[p] === oldKey) d.purposes[p] = newKey;
        });
      });
      Object.keys(d.models || {}).forEach((k) => { delete d.models[k].extra_body_invalid; });
      return d;
    }

    async function save() {
      const problem = validate();
      if (problem) {
        U.toast(problem, 'error');
        return;
      }
      const payload = buildPayload();

      // Re-embed guard: changing the embedding provider/model pair means
      // every paper must be re-embedded — confirm before saving.
      if (resolvedEmbeddingLabel(st.snapshot) !== resolvedEmbeddingLabel(payload)) {
        let papers = 0;
        try { papers = (await API.stats()).papers || 0; } catch (e) { /* best-effort count */ }
        if (st.destroyed) return;
        const proceed = await U.confirm({
          title: 'Change embedding model?',
          body: `Changing the embedding model will re-embed ${U.fmtInt(papers)} papers using the API. Continue?`,
          confirmLabel: 'Re-embed and save'
        });
        if (!proceed) return;
      }

      const btn = container.querySelector('#settings-save');
      if (btn) { btn.disabled = true; btn.textContent = 'Saving…'; }
      try {
        await API.putConfig(payload, st.etag);
        // putConfig does not surface the new ETag — re-fetch fresh state.
        const fresh = await API.getConfig();
        if (st.destroyed) return;
        st.snapshot = fresh.config;
        st.etag = fresh.etag;
        st.draft = deepClone(fresh.config);
        st.renames = { providers: {}, models: {} };
        paint();
        U.toast('Settings saved', 'success');
      } catch (e) {
        if (st.destroyed) return;
        if (btn) { btn.disabled = false; btn.textContent = 'Save'; }
        if (e.status === 409 || e.code === 'config_conflict') {
          const choice = await U.modal({
            title: 'Config changed elsewhere',
            body: '<p class="modal-text">The configuration was modified since you opened this page. Reload the latest version (discarding your edits) or keep editing.</p>',
            actions: [
              { label: 'Keep editing', value: 'keep', kind: 'ghost' },
              { label: 'Reload latest', value: 'reload', kind: 'primary' }
            ]
          });
          if (st.destroyed) return;
          if (choice === 'reload') {
            container.innerHTML = U.spinner('Loading settings…');
            load();
          }
        } else {
          U.toast(e.message, 'error');
        }
      }
    }

    return function destroy() {
      st.destroyed = true;
      clearGuard();
    };
  }
});
