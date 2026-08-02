// Add-paper modal (server paths + file upload + drag-and-drop).
// Extracted from library.js — called with a `load` callback to refresh the list.

import * as U from './util.js?v=1';
import { API } from './api.js?v=1';
import { lastModal, showBatchErrors } from './library.js?v=1';

export function openAddModal(load) {
  const body = `
    <div class="field"><label for="add-paths">Add from server path</label>
      <textarea class="textarea" id="add-paths" placeholder="/path/to/paper.pdf — one path per line"></textarea>
      <div class="field-hint">Paths are resolved on the daemon host; pdf, md, txt, tex, docx and images are supported.</div></div>
    <div class="mt-2"><button type="button" class="btn primary sm" id="add-paths-btn">Add paths</button></div>
    <div id="add-paths-result" class="mt-2"></div>
    <hr class="divider">
    <div class="field"><div class="label">Or upload files</div>
      <div class="dropzone" id="add-drop">${U.icon('upload', 20)}
        <div class="mt-1">Drop files here or
          <button type="button" class="btn subtle sm" id="add-browse">browse</button> to pick files</div></div>
      <input type="file" id="add-file" class="hidden" multiple aria-label="Upload paper files"
        accept=".pdf,.md,.txt,.tex,.docx,.png,.jpg,.jpeg,.webp,.gif,.bmp">
      <div id="add-upload-msg" class="field-hint" role="status"></div></div>`;

  U.modal({
    title: 'Add papers',
    body: body, wide: true,
    actions: [{ label: 'Done', value: true, kind: 'primary' }]
  }).then(() => { load(); });

  const backdrop = lastModal();
  if (!backdrop) return;

  // (a) server paths
  const pathsBtn = backdrop.querySelector('#add-paths-btn');
  const resultEl = backdrop.querySelector('#add-paths-result');
  pathsBtn.addEventListener('click', async () => {
    const paths = backdrop.querySelector('#add-paths').value
      .split('\n').map((s) => s.trim()).filter(Boolean);
    if (!paths.length) return;
    pathsBtn.disabled = true;
    resultEl.innerHTML = U.spinner('Queueing…');
    try {
      const res = await API.batchAdd(paths);
      const items = res.results || [];
      resultEl.innerHTML = items.map((r) => {
        const bad = r.status === 'error' || r.error;
        return '<div class="verify-row">' +
          U.badge(bad ? 'error' : (r.status || 'queued'), bad ? 'err' : 'ok') +
          `<span class="small min-w-0 wrap-anywhere">` +
          `${U.esc(r.title || r.file_path || r.id || '')}</span>` +
          (r.error ? `<span class="spacer"></span><span class="small text-err">` +
            `${U.esc(r.error)}</span>` : '') +
          '</div>';
      }).join('') || '<p class="muted small">Nothing queued.</p>';
      const queued = res.queued != null ? res.queued
        : items.filter((r) => !r.error).length;
      U.toast('Queued ' + queued + (queued === 1 ? ' paper' : ' papers') +
        (res.errors ? ' · ' + res.errors + ' failed' : ''), queued ? 'success' : 'info');
      backdrop.querySelector('#add-paths').value = '';
      load();
    } catch (e) {
      resultEl.innerHTML = `<p class="small text-err">${U.esc(e.message)}</p>`;
    } finally {
      pathsBtn.disabled = false;
    }
  });

  // (b) uploads — sequential
  const drop = backdrop.querySelector('#add-drop');
  const fileInput = backdrop.querySelector('#add-file');
  const msg = backdrop.querySelector('#add-upload-msg');
  async function uploadAll(files) {
    let done = 0;
    let failed = 0;
    for (let i = 0; i < files.length; i++) {
      const f = files[i];
      msg.textContent = `Uploading ${f.name} (${i + 1}/${files.length})…`;
      try {
        await API.importPaper(f);
        done++;
      } catch (e) {
        failed++;
      }
    }
    msg.textContent = 'Uploaded ' + done + (failed ? ' · ' + failed + ' failed' : '');
    U.toast('Uploaded ' + done + (done === 1 ? ' file' : ' files') +
      (failed ? ' · ' + failed + ' failed' : ''), failed ? 'info' : 'success');
    load();
  }
  backdrop.querySelector('#add-browse').addEventListener('click', async () => {
    let paths;
    try {
      const res = await API.pickFile();
      paths = res.paths || [];
    } catch (e) {
      // The dialog itself failed (not a cancel) — say so and point at the
      // manual path field above.
      msg.textContent = e.message + ' You can still paste paths above.';
      U.toast('File picker unavailable — paste paths manually', 'error');
      return;
    }
    if (!paths.length) return; // user cancelled the dialog — stay quiet
    msg.textContent = 'Adding ' + paths.length + ' file(s)…';
    try {
      const batchRes = await API.batchAdd(paths);
      const items = batchRes.results || [];
      const failed = items.filter((r) => r.error);
      const queued = batchRes.queued != null ? batchRes.queued : items.length - failed.length;
      msg.textContent = 'Queued ' + queued + (failed.length ? ' · ' + failed.length + ' failed' : '');
      U.toast('Queued ' + queued + (queued === 1 ? ' paper' : ' papers') +
        (failed.length ? ' · ' + failed.length + ' failed' : ''), failed.length ? 'info' : 'success');
      showBatchErrors(failed, 'Some files could not be added');
      load();
    } catch (e) {
      msg.textContent = e.message;
    }
  });
  fileInput.addEventListener('change', () => {
    if (fileInput.files.length) uploadAll(Array.from(fileInput.files));
    fileInput.value = '';
  });
  ['dragover', 'dragenter'].forEach((ev) => {
    drop.addEventListener(ev, (e) => { e.preventDefault(); drop.classList.add('dragover'); });
  });
  ['dragleave', 'drop'].forEach((ev) => {
    drop.addEventListener(ev, (e) => { e.preventDefault(); drop.classList.remove('dragover'); });
  });
  drop.addEventListener('drop', (e) => {
    if (e.dataTransfer.files && e.dataTransfer.files.length) {
      uploadAll(Array.from(e.dataTransfer.files));
    }
  });
}
