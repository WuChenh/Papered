use async_trait::async_trait;
use papered::error::PaperedError;
use papered::sync::SyncReport;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Consecutive run-level sync failures before the auto-sync circuit breaker
/// trips: automatic sync is paused until a manual sync succeeds.
pub(crate) const MAX_CONSECUTIVE_SYNC_FAILURES: u32 = 5;

/// A source of sync work that can be driven by [`SyncRunner`].
#[async_trait]
pub(crate) trait SyncSource: Send + Sync + 'static {
    /// Run one sync cycle. A successful cycle that nevertheless reports a
    /// run-level failure should still return `Ok(report)`; the runner decides
    /// whether the report counts as a failure via [`sync_run_failure_reason`].
    async fn run_once(&self, cancel: CancellationToken) -> Result<SyncReport, PaperedError>;

    /// Human-readable source name for logging.
    fn name(&self) -> &'static str;
}

/// Drives a [`SyncSource`] on a fixed interval, tracking consecutive failures
/// and tripping a circuit breaker after [`MAX_CONSECUTIVE_SYNC_FAILURES`].
pub(crate) struct SyncRunner<S: SyncSource> {
    source: S,
    interval: Duration,
    cancel: CancellationToken,
    failures: Arc<AtomicU32>,
}

impl<S: SyncSource> SyncRunner<S> {
    pub(crate) fn new(
        source: S,
        interval: Duration,
        cancel: CancellationToken,
        failures: Arc<AtomicU32>,
    ) -> Self {
        Self {
            source,
            interval,
            cancel,
            failures,
        }
    }

    /// Spawn a task that runs the source immediately and then every `interval`
    /// until cancelled or the circuit breaker trips. While the breaker is
    /// tripped the task keeps looping and skipping runs, so a successful manual
    /// sync that resets the shared failure counter automatically resumes
    /// automatic sync.
    pub(crate) fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if self.cancel.is_cancelled() {
                    break;
                }
                if auto_sync_paused(&self.failures) {
                    tracing::debug!("{} auto-sync paused, skipping", self.source.name());
                } else {
                    match self.source.run_once(self.cancel.child_token()).await {
                        Ok(report) => {
                            if let Some(reason) = sync_run_failure_reason(&report) {
                                record_sync_failure(&self.failures, self.source.name(), &reason);
                            } else {
                                record_sync_success(&self.failures, self.source.name());
                            }
                            tracing::info!("{} sync completed: {:?}", self.source.name(), report);
                        }
                        Err(PaperedError::Cancelled(_)) => {
                            tracing::debug!("{} sync cancelled", self.source.name());
                        }
                        Err(e) => {
                            let count = self.failures.load(Ordering::Relaxed) + 1;
                            tracing::error!(
                                "{} sync failed (#{}): {}",
                                self.source.name(),
                                count,
                                e
                            );
                            record_sync_failure(&self.failures, self.source.name(), &e.to_string());
                        }
                    }
                }
                tokio::select! {
                    _ = self.cancel.cancelled() => break,
                    _ = tokio::time::sleep(self.interval) => {}
                }
            }
        })
    }
}

/// Whether a finished sync cycle counts as a run-level failure for the
/// circuit breaker: the cycle reported errors without making any progress
/// (the source was unreachable or the cycle aborted early), as opposed to
/// per-item errors collected in `report.errors`. Returns the first error
/// message as the failure reason.
pub(crate) fn sync_run_failure_reason(report: &SyncReport) -> Option<String> {
    let made_progress = report.imported + report.skipped + report.pdf_found + report.removed > 0;
    if !made_progress {
        return report.errors.first().cloned();
    }
    None
}

/// Record a run-level sync failure. Logs a single error when the failure
/// count reaches [`MAX_CONSECUTIVE_SYNC_FAILURES`] (circuit breaker trips);
/// further failures only bump the counter without logging again.
pub(crate) fn record_sync_failure(failures: &AtomicU32, source: &str, reason: &str) {
    let count = failures.fetch_add(1, Ordering::Relaxed) + 1;
    if count == MAX_CONSECUTIVE_SYNC_FAILURES {
        tracing::error!(
            "{source} auto-sync paused after {count} consecutive sync failures (last error: {reason}) — manual intervention required; a successful manual sync resumes automatic sync"
        );
    }
}

/// Record a successful sync: reset the failure counter and, if the circuit
/// breaker had tripped, log that automatic sync has resumed.
pub(crate) fn record_sync_success(failures: &AtomicU32, source: &str) {
    if failures.swap(0, Ordering::Relaxed) >= MAX_CONSECUTIVE_SYNC_FAILURES {
        tracing::info!(
            "{source} sync succeeded — auto-sync circuit breaker reset, automatic sync resumed"
        );
    }
}

/// Update the circuit-breaker counter from the outcome of a sync cycle.
pub(crate) fn update_sync_circuit_breaker(
    failures: &AtomicU32,
    source: &str,
    failure_reason: Option<&str>,
) {
    match failure_reason {
        Some(reason) => record_sync_failure(failures, source, reason),
        None => record_sync_success(failures, source),
    }
}

/// Whether the auto-sync circuit breaker has tripped.
pub(crate) fn auto_sync_paused(failures: &AtomicU32) -> bool {
    failures.load(Ordering::Relaxed) >= MAX_CONSECUTIVE_SYNC_FAILURES
}
