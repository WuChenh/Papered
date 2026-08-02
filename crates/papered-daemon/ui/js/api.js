// REST API client for the Papered daemon.
// Every method returns a promise; failures reject with Error(server message).

export function qs(params) {
  if (!params) return '';
  const parts = [];
  Object.keys(params).forEach((k) => {
    const v = params[k];
    if (v === undefined || v === null || v === '') return;
    parts.push(encodeURIComponent(k) + '=' + encodeURIComponent(v));
  });
  return parts.length ? '?' + parts.join('&') : '';
}

async function request(method, path, body, opts = {}) {
  const init = { method, headers: {} };
  if (body !== undefined) {
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  if (opts.headers) Object.assign(init.headers, opts.headers);
  const resp = await fetch(path, init);
  if (resp.status === 204) return null;
  let data = null;
  try { data = await resp.json(); } catch (e) { /* non-JSON body */ }
  if (!resp.ok) {
    const msg = (data && data.message) || ('Request failed (' + resp.status + ')');
    const err = new Error(msg);
    err.status = resp.status;
    err.code = data && data.code;
    throw err;
  }
  return data;
}

export const API = {
  qs,
  get: (path) => request('GET', path),
  post: (path, body, opts) => request('POST', path, body === undefined ? {} : body, opts),
  put: (path, body, opts) => request('PUT', path, body, opts),
  del: (path) => request('DELETE', path),

  upload: async (path, file) => {
    const fd = new FormData();
    fd.append('file', file, file.name);
    const resp = await fetch(path, { method: 'POST', body: fd });
    let data = null;
    try { data = await resp.json(); } catch (e) { /* ignore */ }
    if (!resp.ok) throw new Error((data && data.message) || ('Upload failed (' + resp.status + ')'));
    return data;
  },

  // GET /api/v1/config returns an ETag for optimistic concurrency.
  getConfig: async () => {
    const resp = await fetch('/api/v1/config');
    const data = await resp.json();
    if (!resp.ok) throw new Error((data && data.message) || 'Failed to load config');
    return { config: data, etag: resp.headers.get('ETag') };
  },
  putConfig: (config, etag) =>
    request('PUT', '/api/v1/config', config, etag ? { headers: { 'If-Match': etag } } : {}),

  // ---- papers ----
  listPapers: (params) => API.get('/api/v1/papers' + qs(params)),
  getPaper: (id) => API.get('/api/v1/papers/' + encodeURIComponent(id)),
  updatePaper: (id, body) => API.put('/api/v1/papers/' + encodeURIComponent(id), body),
  deletePaper: (id) => API.del('/api/v1/papers/' + encodeURIComponent(id)),
  addPaper: (filePath) => API.post('/api/v1/papers', { file_path: filePath }),
  importPaper: (file) => API.upload('/api/v1/papers/import', file),
  pickFile: () => API.post('/api/v1/papers/pick-file'),
  batchAdd: (paths) => API.post('/api/v1/papers/batch', { file_paths: paths }),
  batchDelete: (ids) => API.post('/api/v1/papers/batch-delete', { paper_ids: ids }),
  batchReindexSections: (ids) => API.post('/api/v1/papers/batch-reindex-sections', { paper_ids: ids }),
  reindex: (id) => API.post('/api/v1/papers/' + encodeURIComponent(id) + '/reindex'),
  reindexSections: (id) => API.post('/api/v1/papers/' + encodeURIComponent(id) + '/reindex-sections'),
  getSections: (id) => API.get('/api/v1/papers/' + encodeURIComponent(id) + '/sections'),
  getFigures: (id) => API.get('/api/v1/papers/' + encodeURIComponent(id) + '/figures'),
  getChunk: (id, chunkId) =>
    API.get('/api/v1/papers/' + encodeURIComponent(id) + '/chunks/' + encodeURIComponent(chunkId)),

  // ---- ratings & comments (user annotations) ----
  getRating: (id) => API.get('/api/v1/papers/' + encodeURIComponent(id) + '/rating'),
  setRating: (id, rating) =>
    API.put('/api/v1/papers/' + encodeURIComponent(id) + '/rating', { rating }),
  clearRating: (id) => API.del('/api/v1/papers/' + encodeURIComponent(id) + '/rating'),
  listComments: (id) => API.get('/api/v1/papers/' + encodeURIComponent(id) + '/comments'),
  addComment: (id, content) =>
    API.post('/api/v1/papers/' + encodeURIComponent(id) + '/comments', { content }),
  deleteComment: (id, commentId) =>
    API.del('/api/v1/papers/' + encodeURIComponent(id) + '/comments/' + encodeURIComponent(commentId)),
  generateInsight: (id) =>
    API.post('/api/v1/papers/' + encodeURIComponent(id) + '/insights'),

  // ---- search / rag ----
  search: (body) => API.post('/api/v1/search', body),
  searchFigures: (body) => API.post('/api/v1/search/figures', body),
  searchPassages: (body) => API.post('/api/v1/search/passages', body),
  graph: (params) => API.get('/api/v1/graph' + qs(params)),
  similar: (paperId, limit) => API.post('/api/v1/similar', { paper_id: paperId, limit: limit || 5 }),
  ask: (body) => API.post('/api/v1/ask', body),

  // ---- prompts ----
  listPrompts: () => API.get('/api/v1/prompts'),
  createPrompt: (body) => API.post('/api/v1/prompts', body),
  updatePrompt: (id, body) => API.put('/api/v1/prompts/' + encodeURIComponent(id), body),
  deletePrompt: (id) => API.del('/api/v1/prompts/' + encodeURIComponent(id)),
  setDefaultPrompt: (id) => API.post('/api/v1/prompts/' + encodeURIComponent(id) + '/default'),

  // ---- stats / health ----
  stats: () => API.get('/api/v1/stats'),
  metrics: () => API.get('/api/v1/metrics'),
  importQueue: () => API.get('/api/v1/import-queue'),
  setIndexingPaused: (paused) => API.post('/api/v1/index-queue/pause', { paused: !!paused }),
  health: () => API.get('/api/v1/health'),
  kbHealth: () => API.get('/api/v1/health/kb'),
  duplicates: () => API.get('/api/v1/health/duplicates'),
  dataQuality: () => API.get('/api/v1/health/quality'),
  imageQuality: () => API.get('/api/v1/health/image-quality'),
  cleanup: () => API.post('/api/v1/health/cleanup'),
  cleanupImages: () => API.post('/api/v1/health/cleanup-images'),
  optimize: () => API.post('/api/v1/health/optimize'),
  optimizeImages: (dryRun) => API.post('/api/v1/health/optimize-images', { dry_run: !!dryRun }),
  exportData: (body) => API.post('/api/v1/export', body),

  // ---- sync ----
  zoteroStatus: () => API.get('/api/v1/zotero/status'),
  zoteroCollections: () => API.get('/api/v1/zotero/collections'),
  zoteroSyncCollections: (keys, recursive) =>
    API.put('/api/v1/zotero/sync-collections', { collection_keys: keys, recursive_collections: recursive }),
  zoteroSync: () => API.post('/api/v1/zotero/sync'),
  zoteroSyncStatus: (id) => API.get('/api/v1/zotero/sync/' + encodeURIComponent(id)),
  zoteroSyncCancel: () => API.post('/api/v1/zotero/sync/cancel'),
  latticeStatus: () => API.get('/api/v1/lattice/status'),
  latticeCollections: () => API.get('/api/v1/lattice/collections'),
  latticeSyncCollections: (names) =>
    API.put('/api/v1/lattice/sync-collections', { collection_names: names }),
  latticeSearch: (q, limit) => API.get('/api/v1/lattice/search' + qs({ q, limit: limit || 20 })),
  latticeSync: () => API.post('/api/v1/lattice/sync'),
  latticeSyncCancel: () => API.post('/api/v1/lattice/sync/cancel'),
  latticeImport: (paperId) => API.post('/api/v1/import/lattice', { paper_id: paperId }),

  // ---- setup / config ----
  setupStatus: () => API.get('/api/v1/setup/status'),
  testEndpoint: (body) => API.post('/api/v1/test-endpoint', body),
  testEmbedding: (body) => API.post('/api/v1/test-embedding', body),
  testReranker: (body) => API.post('/api/v1/test-reranker', body),
  resetData: (force, all) => API.post('/api/v1/reset-data', { force, all }),

  // ---- translations ----
  translatePaper: (id, body) =>
    API.post('/api/v1/papers/' + encodeURIComponent(id) + '/translate', body),
  batchTranslate: (id, body) =>
    API.post('/api/v1/papers/' + encodeURIComponent(id) + '/translate/batch', body),
  getTranslations: (id, lang) =>
    API.get('/api/v1/papers/' + encodeURIComponent(id) + '/translations' + qs({ lang })),
  deleteTranslations: (id) =>
    API.del('/api/v1/papers/' + encodeURIComponent(id) + '/translations')
};
