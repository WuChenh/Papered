//! Daemon application state: the central `AppState` struct and its sub-modules.

pub(crate) mod builders;
pub(crate) mod config_watcher;
pub(crate) mod embedding;
pub(crate) mod sync;

// Re-export items that external code references via `crate::state::*`.
pub(crate) use builders::{
    build_embedding_client, build_embedding_or_placeholder, build_engines,
    build_reranker_or_placeholder, init_store, queue_paper_for_indexing,
};
pub(crate) use config_watcher::{LOG_RELOAD_HANDLE, reload_log_level, start_config_watcher};
pub(crate) use embedding::{EmbeddingChangeError, EmbeddingRebuildPolicy};
pub(crate) use sync::{SyncJob, SyncJobStatus, ZoteroSyncRequest};

use papered::{
    AppConfig, llm::embed::EmbeddingClient, llm::reranker::RerankerClient,
    store::vector::VectorStore,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_RETRIES: u32 = 3;
pub(crate) const RETRY_DELAY_SECS: u64 = 10;

/// Flag file recording that the indexing worker pool is paused. Its mere
/// presence means paused, so the pause survives daemon restarts.
pub(crate) fn index_pause_flag_path(config: &AppConfig) -> std::path::PathBuf {
    config.data_dir.join("indexing.paused")
}

pub(crate) struct AppState {
    pub store: Arc<dyn VectorStore>,
    pub embedding: Arc<RwLock<EmbeddingClient>>,
    pub reranker: Arc<RwLock<RerankerClient>>,
    pub config: Arc<RwLock<AppConfig>>,
    pub search_engine: Arc<RwLock<papered::search::SearchEngine>>,
    pub rag_engine: Arc<RwLock<papered::llm::rag::RagEngine>>,
    pub indexer: Arc<RwLock<papered::Indexer>>,
    pub import_tx: tokio::sync::mpsc::Sender<papered::util::IndexJob>,
    pub embedding_model_ready: AtomicBool,
    pub lattice_sync_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    pub lattice_cancel: Arc<tokio::sync::Mutex<CancellationToken>>,
    pub zotero_sync_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    pub zotero_sync_worker_handle: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    pub zotero_sync_tx: tokio::sync::mpsc::Sender<ZoteroSyncRequest>,
    pub zotero_cancel: Arc<tokio::sync::Mutex<CancellationToken>>,
    pub zotero_sync_jobs: Arc<RwLock<std::collections::HashMap<String, SyncJob>>>,
    /// Consecutive run-level Zotero sync failures (auto-sync circuit breaker).
    pub zotero_sync_failures: Arc<AtomicU32>,
    /// Consecutive run-level Lattice sync failures (auto-sync circuit breaker).
    pub lattice_sync_failures: Arc<AtomicU32>,
    /// Serializes Zotero sync executions so automatic and manual syncs never
    /// run concurrently.
    pub zotero_sync_lock: Arc<tokio::sync::Mutex<()>>,
    pub config_needs_restart: AtomicBool,
    pub reembed_total: AtomicUsize,
    pub reembed_completed: AtomicUsize,
    /// True while the indexing worker pool is paused (pause/resume endpoint).
    /// Persisted in the `indexing.paused` flag file so it survives restarts.
    /// The watch channel wakes the worker loop on resume.
    pub indexing_paused: AtomicBool,
    pub index_pause_tx: tokio::sync::watch::Sender<bool>,
    pub config_write_lock: Arc<tokio::sync::Mutex<()>>,
    pub lattice_client: Option<papered::lattice::LatticeClient>,
    pub zotero_client: Option<papered::zotero::ZoteroClient>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<dyn VectorStore>,
        embedding: Arc<RwLock<EmbeddingClient>>,
        reranker: Arc<RwLock<RerankerClient>>,
        config: Arc<RwLock<AppConfig>>,
        search_engine: Arc<RwLock<papered::search::SearchEngine>>,
        rag_engine: Arc<RwLock<papered::llm::rag::RagEngine>>,
        indexer: Arc<RwLock<papered::Indexer>>,
        import_tx: tokio::sync::mpsc::Sender<papered::util::IndexJob>,
        zotero_sync_tx: tokio::sync::mpsc::Sender<ZoteroSyncRequest>,
        indexing_paused: bool,
    ) -> Self {
        Self {
            store,
            embedding,
            reranker,
            config,
            search_engine,
            rag_engine,
            indexer,
            import_tx,
            embedding_model_ready: AtomicBool::new(false),
            lattice_sync_task: Arc::new(tokio::sync::Mutex::new(None)),
            lattice_cancel: Arc::new(tokio::sync::Mutex::new(CancellationToken::new())),
            zotero_sync_task: Arc::new(tokio::sync::Mutex::new(None)),
            zotero_sync_worker_handle: Arc::new(tokio::sync::Mutex::new(None)),
            zotero_sync_tx,
            zotero_cancel: Arc::new(tokio::sync::Mutex::new(CancellationToken::new())),
            zotero_sync_jobs: Arc::new(RwLock::new(std::collections::HashMap::new())),
            zotero_sync_failures: Arc::new(AtomicU32::new(0)),
            lattice_sync_failures: Arc::new(AtomicU32::new(0)),
            zotero_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_needs_restart: AtomicBool::new(false),
            reembed_total: AtomicUsize::new(0),
            reembed_completed: AtomicUsize::new(0),
            indexing_paused: AtomicBool::new(indexing_paused),
            index_pause_tx: tokio::sync::watch::channel(indexing_paused).0,
            config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            lattice_client: papered::lattice::LatticeClient::new().ok(),
            zotero_client: Some(papered::zotero::ZoteroClient::new()),
        }
    }

    pub async fn reload_clients(&self) -> papered::error::Result<()> {
        let config = self.config.read().await.clone();
        let metrics = papered::llm::metrics::store_metrics_sink(&self.store);

        let embedding =
            build_embedding_or_placeholder(&config, metrics.clone(), "using placeholder client");
        *self.embedding.write().await = embedding.clone();

        let reranker =
            build_reranker_or_placeholder(&config, metrics.clone(), "using placeholder client");
        *self.reranker.write().await = reranker.clone();

        let (search_engine, rag_engine, indexer) = build_engines(
            self.store.clone(),
            embedding.clone(),
            reranker.clone(),
            &config,
        )
        .await?;
        *self.search_engine.write().await = search_engine;
        *self.rag_engine.write().await = rag_engine;
        *self.indexer.write().await = indexer;

        Ok(())
    }

    pub async fn apply_config_update(
        self: &Arc<Self>,
        new_config: &AppConfig,
        old_config: &AppConfig,
    ) {
        let old_embedding_fp = old_config.embedding_fingerprint();
        let new_embedding_fp = new_config.embedding_fingerprint();

        let mut effective_new_config = new_config.clone();

        // If the selected Zotero collections changed, reset last_sync_version so the next sync
        // fetches the full contents of the new collections instead of skipping old items.
        let mut old_keys = old_config.zotero_sync.collection_keys.clone();
        old_keys.sort();
        let mut new_keys = new_config.zotero_sync.collection_keys.clone();
        new_keys.sort();
        let collections_changed = old_keys != new_keys;
        let recursive_changed = old_config.zotero_sync.recursive_collections
            != new_config.zotero_sync.recursive_collections;
        if collections_changed || recursive_changed {
            effective_new_config.zotero_sync.last_sync_version = 0;
            tracing::info!(
                "Zotero sync scope changed (collections={collections_changed}, recursive={recursive_changed}) — resetting last_sync_version to 0 for full re-sync"
            );
            if let Err(e) = effective_new_config.save() {
                tracing::warn!(
                    "Failed to persist Zotero last_sync_version reset to config: {}",
                    e
                );
            }
        }

        *self.config.write().await = effective_new_config.clone();

        if old_config.log_level != new_config.log_level {
            reload_log_level(&new_config.log_level);
        }

        if old_embedding_fp != new_embedding_fp {
            tracing::info!("Embedding model changed — testing new model");
            match build_embedding_client(new_config) {
                Ok(embedding) => {
                    *self.embedding.write().await = embedding
                        .with_metrics(papered::llm::metrics::store_metrics_sink(&self.store));
                    match self
                        .handle_embedding_model_change(EmbeddingRebuildPolicy::RebuildIfChanged)
                        .await
                    {
                        Ok(change) => {
                            if change.rebuilt {
                                self.spawn_reembed_all();
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "New embedding model test failed: {e}. Setting degraded mode."
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to build embedding client for new config: {}", e);
                    self.embedding_model_ready.store(false, Ordering::Relaxed);
                }
            }
        }

        if let Err(e) = self.reload_clients().await {
            tracing::warn!("Failed to reload clients after config update: {}", e);
        }

        if old_config.lattice_sync != new_config.lattice_sync {
            self.spawn_lattice_sync().await;
        }

        if old_config.zotero_sync.base.enabled != effective_new_config.zotero_sync.base.enabled
            || (effective_new_config.zotero_sync.base.enabled
                && (old_config.zotero_sync.base.interval_secs
                    != effective_new_config.zotero_sync.base.interval_secs
                    || old_keys != new_keys
                    || old_config.zotero_sync.recursive_collections
                        != effective_new_config.zotero_sync.recursive_collections))
        {
            self.spawn_zotero_sync().await;
        }

        if old_config.indexing != new_config.indexing {
            tracing::warn!(
                "Indexing concurrency/queue_size changed — requires daemon restart to take effect"
            );
            self.config_needs_restart.store(true, Ordering::Relaxed);
        }
    }

    /// Mutate the active config under the write lock, propagate side-effects,
    /// and persist. Returns the saved config.
    ///
    /// Shared by the Lattice and Zotero sync-scope endpoints, which both
    /// follow: lock → clone → mutate → apply → save.
    pub(crate) async fn update_config_saved(
        self: &Arc<Self>,
        mutate: impl FnOnce(&mut AppConfig),
    ) -> Result<AppConfig, papered::PaperedError> {
        let _guard = self.config_write_lock.lock().await;
        let old_config = self.config.read().await.clone();
        let mut new_config = old_config.clone();
        mutate(&mut new_config);
        self.apply_config_update(&new_config, &old_config).await;
        let updated_config = self.config.read().await.clone();
        updated_config.save()?;
        Ok(updated_config)
    }
}

/// Build a minimal [`AppState`] backed by a temp-dir store, for tests.
///
/// Returns the state together with the [`tempfile::TempDir`] backing it.
/// The caller must keep the guard alive for the duration of the test; on
/// drop it removes the whole data directory (db + WAL sidecars), so tests
/// leave no `papered-test-state-*` residue behind — even on panic.
#[cfg(test)]
pub(crate) async fn test_app_state() -> (Arc<AppState>, tempfile::TempDir) {
    use tokio::sync::mpsc;

    use papered::config::{ModelConfig, ProviderConfig};

    let mut config = papered::AppConfig::default();
    config.providers.insert(
        "local".into(),
        ProviderConfig {
            api_base: "http://localhost:11434".into(),
            api_key: None,
        },
    );
    let local_model = ModelConfig {
        provider: "local".into(),
        model: "stub".into(),
        concurrency: 0,
        rpm: 0,
        tpm: 0,
        extra_body: None,
        reasoning_effort: None,
        context_window: None,
        max_output_tokens: None,
    };
    config.models.insert("stub".into(), local_model.clone());
    config.purposes.embedding = "stub".into();
    config.purposes.reranker = "stub".into();
    config.purposes.section = "stub".into();
    config.purposes.rag = "stub".into();
    // Multimodal embedding is now handled by the unified EmbeddingClient.
    let temp_dir = tempfile::Builder::new()
        .prefix("papered-test-state-")
        .tempdir()
        .expect("create temp dir");
    config.data_dir = temp_dir.path().to_path_buf();

    let config_arc = Arc::new(RwLock::new(config.clone()));
    let store = papered::create_store(&config.db_path())
        .await
        .expect("create test store");

    let embedding = build_embedding_client(&config).expect("build embedding client");
    let reranker = builders::build_reranker_client(&config).expect("build reranker client");
    let embedding_arc = Arc::new(RwLock::new(embedding.clone()));
    let reranker_arc = Arc::new(RwLock::new(reranker.clone()));

    let (search_engine, rag_engine, indexer) =
        build_engines(store.clone(), embedding.clone(), reranker.clone(), &config)
            .await
            .expect("create engines");
    let search_engine = Arc::new(RwLock::new(search_engine));
    let rag_engine = Arc::new(RwLock::new(rag_engine));
    let indexer = Arc::new(RwLock::new(indexer));

    let (import_tx, _import_rx) = mpsc::channel::<papered::util::IndexJob>(1);
    let (zotero_sync_tx, _zotero_sync_rx) = mpsc::channel::<ZoteroSyncRequest>(1);

    let state = Arc::new(AppState::new(
        store,
        embedding_arc,
        reranker_arc,
        config_arc,
        search_engine,
        rag_engine,
        indexer,
        import_tx,
        zotero_sync_tx,
        false,
    ));
    (state, temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_runner::{
        MAX_CONSECUTIVE_SYNC_FAILURES, auto_sync_paused, record_sync_failure, record_sync_success,
        sync_run_failure_reason,
    };
    use std::sync::atomic::AtomicU32;

    fn report(imported: usize, skipped: usize, errors: &[&str]) -> papered::sync::SyncReport {
        papered::sync::SyncReport {
            imported,
            skipped,
            errors: errors.iter().map(|e| (*e).to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn run_failure_requires_errors_without_progress() {
        // Whole-cycle failure: errors and zero progress (e.g. source down).
        let r = report(0, 0, &["Zotero fetch failed: connection refused"]);
        assert_eq!(
            sync_run_failure_reason(&r).as_deref(),
            Some("Zotero fetch failed: connection refused")
        );
        // Per-item errors alongside progress do not count as a run failure.
        let r = report(3, 1, &["Failed to convert item x: bad metadata"]);
        assert_eq!(sync_run_failure_reason(&r), None);
        // Clean cycle with only skips.
        let r = report(0, 5, &[]);
        assert_eq!(sync_run_failure_reason(&r), None);
        // Empty report (nothing fetched, no errors) is not a failure.
        assert_eq!(
            sync_run_failure_reason(&papered::sync::SyncReport::new()),
            None
        );
    }

    #[test]
    fn circuit_breaker_trips_at_threshold_and_resets() {
        let failures = AtomicU32::new(0);
        assert!(!auto_sync_paused(&failures));
        for i in 1..MAX_CONSECUTIVE_SYNC_FAILURES {
            record_sync_failure(&failures, "Zotero", "boom");
            assert_eq!(failures.load(Ordering::Relaxed), i);
            assert!(!auto_sync_paused(&failures));
        }
        record_sync_failure(&failures, "Zotero", "boom");
        assert!(auto_sync_paused(&failures));
        // Further failures keep counting while paused (no re-trip).
        record_sync_failure(&failures, "Zotero", "boom");
        assert!(auto_sync_paused(&failures));
        // A successful sync resets the counter and clears the pause.
        record_sync_success(&failures, "Zotero");
        assert_eq!(failures.load(Ordering::Relaxed), 0);
        assert!(!auto_sync_paused(&failures));
    }
}
