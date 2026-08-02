//! Sync job types, sync sources, and sync spawning logic.

use super::AppState;
use crate::sync_runner::{
    SyncRunner, SyncSource, sync_run_failure_reason, update_sync_circuit_breaker,
};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Status of a Zotero sync job tracked by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncJobStatus {
    Pending,
    Running,
    Completed,
    Cancelled,
}

impl SyncJobStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for SyncJobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A tracked Zotero sync job.
#[derive(Debug, Clone)]
pub(crate) struct SyncJob {
    pub(crate) id: String,
    pub(crate) status: SyncJobStatus,
    pub(crate) report: Option<papered::sync::SyncReport>,
}

impl SyncJob {
    pub(crate) fn new(id: String) -> Self {
        Self {
            id,
            status: SyncJobStatus::Pending,
            report: None,
        }
    }

    pub(crate) fn set_status(&mut self, status: SyncJobStatus) {
        self.status = status;
    }

    pub(crate) fn set_report(&mut self, report: papered::sync::SyncReport) {
        self.report = Some(report);
    }
}

/// Request type sent to the Zotero manual sync worker.
#[derive(Debug)]
pub(crate) enum ZoteroSyncRequest {
    Manual {
        job_id: String,
        response_tx: tokio::sync::oneshot::Sender<papered::sync::SyncReport>,
    },
}

/// Maximum number of finished (completed/cancelled) Zotero sync jobs retained
/// for status polling. Older finished jobs are evicted so the job map can't
/// grow forever; eviction order is arbitrary since job ids are random UUIDs.
const MAX_FINISHED_SYNC_JOBS: usize = 100;

/// Log sync results with the appropriate level (info on success, warn on errors).
fn log_sync_results(
    source: &str,
    imported: usize,
    pdf_found: usize,
    skipped: usize,
    errors: &[String],
) {
    if errors.is_empty() {
        tracing::info!(
            "{source} sync: {imported} imported ({pdf_found} with PDF), {skipped} skipped"
        );
    } else {
        tracing::warn!(
            "{source} sync: {imported} imported, {skipped} skipped, {} errors: {:?}",
            errors.len(),
            errors
        );
    }
}

/// Build an empty report carrying a single run-level error message.
fn sync_error_report(msg: String) -> papered::sync::SyncReport {
    let mut r = papered::sync::SyncReport::new();
    r.errors.push(msg);
    r
}

/// Evict finished sync jobs beyond [`MAX_FINISHED_SYNC_JOBS`] so the job map
/// can't grow forever.
fn prune_finished_sync_jobs(jobs: &mut std::collections::HashMap<String, SyncJob>) {
    let finished = jobs
        .values()
        .filter(|j| {
            matches!(
                j.status,
                SyncJobStatus::Completed | SyncJobStatus::Cancelled
            )
        })
        .count();
    if finished <= MAX_FINISHED_SYNC_JOBS {
        return;
    }
    let evict: Vec<String> = jobs
        .iter()
        .filter(|(_, j)| {
            matches!(
                j.status,
                SyncJobStatus::Completed | SyncJobStatus::Cancelled
            )
        })
        .map(|(id, _)| id.clone())
        .take(finished - MAX_FINISHED_SYNC_JOBS)
        .collect();
    for id in evict {
        jobs.remove(&id);
    }
}

/// Thin [`SyncSource`] wrapper around [`papered::lattice::syncer::LatticeSyncer`].
pub(crate) struct DaemonLatticeSource {
    store: Arc<dyn papered::store::vector::VectorStore>,
    import_tx: tokio::sync::mpsc::Sender<papered::util::IndexJob>,
    state: std::sync::Weak<AppState>,
}

#[async_trait]
impl SyncSource for DaemonLatticeSource {
    fn name(&self) -> &'static str {
        "lattice"
    }

    async fn run_once(
        &self,
        cancel: CancellationToken,
    ) -> Result<papered::sync::SyncReport, papered::PaperedError> {
        let Some(state) = self.state.upgrade() else {
            return Err(papered::PaperedError::Unknown(
                "AppState dropped".to_string(),
            ));
        };
        let config = state.config.read().await.lattice_sync.clone();
        let syncer = papered::lattice::syncer::LatticeSyncer::with_collections(
            self.store.clone(),
            self.import_tx.clone(),
            config.base.batch_limit,
            config.base.pdf_search_paths,
            config.collections,
            cancel.clone(),
        )?;
        let report = syncer.sync().await;
        log_sync_results(
            "Lattice",
            report.imported,
            report.pdf_found,
            report.skipped,
            &report.errors,
        );
        if cancel.is_cancelled() {
            return Err(papered::PaperedError::Cancelled(format!(
                "{} sync cancelled",
                self.name()
            )));
        }
        Ok(report)
    }
}

/// Thin [`SyncSource`] wrapper around [`papered::zotero::syncer::ZoteroSyncer`].
pub(crate) struct DaemonZoteroSource {
    store: Arc<dyn papered::store::vector::VectorStore>,
    import_tx: tokio::sync::mpsc::Sender<papered::util::IndexJob>,
    save_since: bool,
    state: std::sync::Weak<AppState>,
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl DaemonZoteroSource {
    pub(crate) fn new(state: &Arc<AppState>, save_since: bool) -> Self {
        Self {
            store: state.store.clone(),
            import_tx: state.import_tx.clone(),
            save_since,
            state: Arc::downgrade(state),
            lock: state.zotero_sync_lock.clone(),
        }
    }
}

#[async_trait]
impl SyncSource for DaemonZoteroSource {
    fn name(&self) -> &'static str {
        "zotero"
    }

    async fn run_once(
        &self,
        cancel: CancellationToken,
    ) -> Result<papered::sync::SyncReport, papered::PaperedError> {
        let _guard = self.lock.lock().await;
        let Some(state) = self.state.upgrade() else {
            return Err(papered::PaperedError::Unknown(
                "AppState dropped".to_string(),
            ));
        };
        let config = state.config.read().await.zotero_sync.clone();
        let mut syncer = papered::zotero::syncer::ZoteroSyncer::new(
            self.store.clone(),
            self.import_tx.clone(),
            config.base.batch_limit,
            config.base.pdf_search_paths,
            config.collection_keys,
            config.download_pdf,
            config.last_sync_version,
            config.recursive_collections,
            cancel.clone(),
        );
        let report = syncer.sync().await;
        if self.save_since && report.errors.is_empty() && report.new_since > 0 {
            state.save_zotero_since(report.new_since).await;
        }
        log_sync_results(
            "Zotero",
            report.imported,
            report.pdf_found,
            report.skipped,
            &report.errors,
        );
        if cancel.is_cancelled() {
            return Err(papered::PaperedError::Cancelled(format!(
                "{} sync cancelled",
                self.name()
            )));
        }
        Ok(report)
    }
}

impl AppState {
    /// Cancel the current auto-sync run (if any), abort its task, and respawn
    /// the runner with fresh configuration. Shared by the Lattice and Zotero
    /// auto-sync paths — they differ only in the config snapshot, the source,
    /// and the cancel/task/failure bookkeeping slots.
    async fn respawn_auto_sync<S: SyncSource>(
        &self,
        config: papered::config::BaseSyncConfig,
        source: S,
        cancel_slot: &Arc<tokio::sync::Mutex<CancellationToken>>,
        task_slot: &Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
        failures: &Arc<std::sync::atomic::AtomicU32>,
        label: &str,
    ) {
        let cancel = {
            let mut guard = cancel_slot.lock().await;
            guard.cancel();
            let new = CancellationToken::new();
            *guard = new.clone();
            new
        };
        {
            let mut task_guard = task_slot.lock().await;
            if let Some(handle) = task_guard.take() {
                handle.abort();
            }
        }

        if !config.enabled {
            tracing::info!("{label} auto-sync disabled — aborted any existing task");
            return;
        }

        let interval_secs = config.interval_secs.max(60);
        tracing::info!(
            "{label} auto-sync enabled (interval: {}s, batch_limit: {})",
            interval_secs,
            config.batch_limit
        );

        let runner = SyncRunner::new(
            source,
            Duration::from_secs(interval_secs),
            cancel,
            failures.clone(),
        );
        let handle = runner.spawn();

        let mut guard = task_slot.lock().await;
        guard.replace(handle);
    }

    pub async fn spawn_lattice_sync(self: &Arc<Self>) {
        let config = self.config.read().await.lattice_sync.clone();
        let source = DaemonLatticeSource {
            store: self.store.clone(),
            import_tx: self.import_tx.clone(),
            state: Arc::downgrade(self),
        };
        self.respawn_auto_sync(
            config.base,
            source,
            &self.lattice_cancel,
            &self.lattice_sync_task,
            &self.lattice_sync_failures,
            "Lattice",
        )
        .await;
    }

    /// Start the Zotero manual sync worker. Manual sync requests are queued
    /// through this worker so that each request gets a `sync_id`, can be
    /// polled for status, and is serialized with automatic syncs via
    /// [`Self::zotero_sync_lock`].
    pub async fn start_zotero_sync_worker(
        state: Arc<Self>,
        mut rx: tokio::sync::mpsc::Receiver<ZoteroSyncRequest>,
    ) {
        let worker_state = state.clone();
        let handle = tokio::spawn(async move {
            tracing::info!("Zotero sync worker started");
            while let Some(req) = rx.recv().await {
                let ZoteroSyncRequest::Manual {
                    job_id,
                    response_tx,
                } = req;

                if let Some(job) = worker_state.zotero_sync_jobs.write().await.get_mut(&job_id) {
                    job.set_status(SyncJobStatus::Running);
                }

                let cancel = {
                    let mut guard = worker_state.zotero_cancel.lock().await;
                    *guard = CancellationToken::new();
                    guard.clone()
                };
                let source = DaemonZoteroSource::new(&worker_state, true);
                // Isolate each sync run in its own task: a panic in the sync
                // path must fail this one job, not kill the worker — a dead
                // worker rejects every later manual sync as sync_busy until
                // the daemon restarts.
                let run_cancel = cancel.clone();
                let report =
                    match tokio::spawn(async move { source.run_once(run_cancel).await }).await {
                        Ok(Ok(report)) => report,
                        Ok(Err(e)) => sync_error_report(e.to_string()),
                        Err(e) => {
                            tracing::error!("Zotero sync task died unexpectedly: {e}");
                            sync_error_report(format!("sync task died unexpectedly: {e}"))
                        }
                    };

                let was_cancelled = cancel.is_cancelled();
                if !was_cancelled {
                    update_sync_circuit_breaker(
                        &worker_state.zotero_sync_failures,
                        "Zotero",
                        sync_run_failure_reason(&report).as_deref(),
                    );
                }

                {
                    let mut jobs = worker_state.zotero_sync_jobs.write().await;
                    if let Some(job) = jobs.get_mut(&job_id) {
                        if was_cancelled {
                            job.set_status(SyncJobStatus::Cancelled);
                        } else {
                            job.set_status(SyncJobStatus::Completed);
                            job.set_report(report.clone());
                        }
                    }
                    prune_finished_sync_jobs(&mut jobs);
                }

                let _ = response_tx.send(report);
            }
            tracing::info!("Zotero sync worker shutting down");
        });
        *state.zotero_sync_worker_handle.lock().await = Some(handle);
    }

    /// Spawn (or respawn) the Zotero auto-sync runner.
    pub async fn spawn_zotero_sync(self: &Arc<Self>) {
        let config = self.config.read().await.zotero_sync.clone();
        let source = DaemonZoteroSource::new(self, true);
        self.respawn_auto_sync(
            config.base,
            source,
            &self.zotero_cancel,
            &self.zotero_sync_task,
            &self.zotero_sync_failures,
            "Zotero",
        )
        .await;
    }

    pub(crate) async fn save_zotero_since(&self, new_since: u64) {
        if new_since == 0 {
            return;
        }
        let _guard = self.config_write_lock.lock().await;
        let mut config = self.config.write().await;
        if config.zotero_sync.last_sync_version >= new_since {
            return;
        }
        config.zotero_sync.last_sync_version = new_since;
        if let Err(e) = config.save() {
            tracing::warn!("Failed to save Zotero last_sync_version to config: {}", e);
        }
    }
}
