//! REST API route path constants.
//!
//! All v1 daemon endpoints are declared as `pub const` strings so the router
//! and clients share a single source of truth for path values.

/// Default port the daemon binds to on localhost. Also used by the CLI client
/// and the macOS app for daemon discovery.
pub const DAEMON_DEFAULT_PORT: u16 = 9321;

/// Number of consecutive ports to try when the default is occupied.
pub const DAEMON_MAX_PORT_TRIES: u16 = 10;

/// Directory where the daemon writes its port file (under the user's config dir).
pub fn daemon_port_dir() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("papered")
}

/// Path to the daemon's port file, read by CLI clients to discover the daemon.
pub fn daemon_port_file() -> std::path::PathBuf {
    daemon_port_dir().join("daemon.port")
}

/// Temporary path used while atomically writing the port file.
pub fn daemon_port_tmp_file() -> std::path::PathBuf {
    daemon_port_dir().join(".daemon.port.tmp")
}

/// Path to the daemon's PID file, written at process start and removed on
/// graceful shutdown. Read by CLI clients to tell a live-but-still-starting
/// daemon apart from a stale port file left by an ungraceful exit.
pub fn daemon_pid_file() -> std::path::PathBuf {
    daemon_port_dir().join("daemon.pid")
}

pub const HEALTH: &str = "/health";
pub const API_HEALTH: &str = "/api/v1/health";
pub const API_HEALTH_KB: &str = "/api/v1/health/kb";
pub const API_HEALTH_CLEANUP: &str = "/api/v1/health/cleanup";
pub const API_HEALTH_OPTIMIZE: &str = "/api/v1/health/optimize";
pub const API_HEALTH_QUALITY: &str = "/api/v1/health/quality";
pub const API_HEALTH_IMAGE_QUALITY: &str = "/api/v1/health/image-quality";
pub const API_HEALTH_DUPLICATES: &str = "/api/v1/health/duplicates";
pub const API_HEALTH_CLEANUP_IMAGES: &str = "/api/v1/health/cleanup-images";
pub const API_HEALTH_REGENERATE_COVERS: &str = "/api/v1/health/regenerate-covers";
pub const API_HEALTH_OPTIMIZE_IMAGES: &str = "/api/v1/health/optimize-images";
pub const API_STATS: &str = "/api/v1/stats";
pub const API_METRICS: &str = "/api/v1/metrics";
pub const API_CONFIG: &str = "/api/v1/config";
pub const API_CONFIG_EMBEDDING: &str = "/api/v1/config/embedding";
pub const API_SETUP_STATUS: &str = "/api/v1/setup/status";
pub const API_TEST_ENDPOINT: &str = "/api/v1/test-endpoint";
pub const API_TEST_EMBEDDING: &str = "/api/v1/test-embedding";
pub const API_TEST_RERANKER: &str = "/api/v1/test-reranker";
pub const API_PAPERS: &str = "/api/v1/papers";
pub const API_PAPERS_IMPORT: &str = "/api/v1/papers/import";
pub const API_PAPERS_PICK_FILE: &str = "/api/v1/papers/pick-file";
pub const API_PAPERS_BATCH: &str = "/api/v1/papers/batch";
pub const API_PAPERS_BATCH_STATUS: &str = "/api/v1/papers/batch-status";
pub const API_PAPERS_BATCH_DELETE: &str = "/api/v1/papers/batch-delete";
pub const API_PAPERS_BATCH_REINDEX_SECTIONS: &str = "/api/v1/papers/batch-reindex-sections";
pub const API_SEARCH: &str = "/api/v1/search";
pub const API_SEARCH_FIGURES: &str = "/api/v1/search/figures";
pub const API_SEARCH_PASSAGES: &str = "/api/v1/search/passages";
pub const API_GRAPH: &str = "/api/v1/graph";
pub const API_SIMILAR: &str = "/api/v1/similar";
pub const API_ASK: &str = "/api/v1/ask";
pub const API_PROMPTS: &str = "/api/v1/prompts";
pub const API_EXPORT: &str = "/api/v1/export";
pub const API_IMPORT_QUEUE: &str = "/api/v1/import-queue";
pub const API_LATTICE_STATUS: &str = "/api/v1/lattice/status";
pub const API_LATTICE_COLLECTIONS: &str = "/api/v1/lattice/collections";
pub const API_LATTICE_SEARCH: &str = "/api/v1/lattice/search";
pub const API_LATTICE_SYNC: &str = "/api/v1/lattice/sync";
pub const API_LATTICE_SYNC_CANCEL: &str = "/api/v1/lattice/sync/cancel";
pub const API_LATTICE_SYNC_COLLECTIONS: &str = "/api/v1/lattice/sync-collections";
pub const API_IMPORT_LATTICE: &str = "/api/v1/import/lattice";
pub const API_ZOTERO_STATUS: &str = "/api/v1/zotero/status";
pub const API_ZOTERO_COLLECTIONS: &str = "/api/v1/zotero/collections";
pub const API_ZOTERO_SYNC_COLLECTIONS: &str = "/api/v1/zotero/sync-collections";
pub const API_ZOTERO_SYNC: &str = "/api/v1/zotero/sync";
pub const API_ZOTERO_SYNC_CANCEL: &str = "/api/v1/zotero/sync/cancel";
pub const API_RESET_DATA: &str = "/api/v1/reset-data";

// Parameterized route patterns used by the axum router.
pub const API_PAPERS_ID: &str = "/api/v1/papers/{id}";
pub const API_PAPERS_ID_SECTIONS: &str = "/api/v1/papers/{id}/sections";
pub const API_PAPERS_ID_FIGURES: &str = "/api/v1/papers/{id}/figures";
pub const API_PAPERS_ID_FIGURES_IMAGE: &str = "/api/v1/papers/{id}/figures/{figure_id}/image";
pub const API_PAPERS_ID_COVER: &str = "/api/v1/papers/{id}/cover";
pub const API_PAPERS_ID_COVER_THUMB: &str = "/api/v1/papers/{id}/cover/thumb";
pub const API_PAPERS_ID_COVER_REGENERATE: &str = "/api/v1/papers/{id}/cover/regenerate";
pub const API_PAPERS_ID_CHUNKS_CHUNK_ID: &str = "/api/v1/papers/{id}/chunks/{chunk_id}";
pub const API_PAPERS_ID_RATING: &str = "/api/v1/papers/{id}/rating";
pub const API_PAPERS_ID_COMMENTS: &str = "/api/v1/papers/{id}/comments";
pub const API_PAPERS_ID_COMMENTS_ID: &str = "/api/v1/papers/{id}/comments/{comment_id}";
pub const API_PAPERS_ID_INSIGHTS: &str = "/api/v1/papers/{id}/insights";
pub const API_PAPERS_ID_REINDEX: &str = "/api/v1/papers/{id}/reindex";
pub const API_PAPERS_ID_REINDEX_SECTIONS: &str = "/api/v1/papers/{id}/reindex-sections";
pub const API_PROMPTS_ID: &str = "/api/v1/prompts/{id}";
pub const API_PROMPTS_ID_DEFAULT: &str = "/api/v1/prompts/{id}/default";
pub const API_ZOTERO_SYNC_ID: &str = "/api/v1/zotero/sync/{id}";

// Translation routes.
pub const API_PAPERS_ID_TRANSLATE: &str = "/api/v1/papers/{id}/translate";
pub const API_PAPERS_ID_TRANSLATE_BATCH: &str = "/api/v1/papers/{id}/translate/batch";
pub const API_PAPERS_ID_TRANSLATIONS: &str = "/api/v1/papers/{id}/translations";
pub const API_TRANSLATIONS_SEARCH: &str = "/api/v1/translations/search";
