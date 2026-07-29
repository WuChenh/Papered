use crate::AppState;
use papered::StrLabel;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use tokio::task::JoinSet;

async fn paper_exists_or_log(
    store: &dyn papered::store::vector::VectorStore,
    paper_id: &str,
    context: &str,
) -> bool {
    match store.get_paper(paper_id).await {
        Ok(p) => p.is_some(),
        Err(e) => {
            tracing::warn!(
                "Failed to fetch paper {} {} (treating as existing): {}",
                paper_id,
                context,
                e
            );
            true
        }
    }
}

pub(crate) fn spawn_indexing_worker_pool(
    state: Arc<AppState>,
    mut import_rx: tokio::sync::mpsc::Receiver<papered::util::IndexJob>,
    import_tx: tokio::sync::mpsc::Sender<papered::util::IndexJob>,
    concurrency: usize,
    tasks: &mut JoinSet<()>,
) {
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let retry_tx = import_tx.clone();
    let startup_tx = import_tx;
    let state2 = state.clone();
    let in_progress: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    tasks.spawn(async move {
        while let Some(job) = import_rx.recv().await {
            {
                let mut guard = in_progress.lock().unwrap();
                if !guard.insert(job.paper_id.clone()) {
                    tracing::warn!(
                        "Skipping duplicate job for {} — already being processed",
                        job.paper_id
                    );
                    continue;
                }
            }
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    in_progress.lock().unwrap().remove(&job.paper_id);
                    tracing::error!("Semaphore closed, stopping indexing worker");
                    break;
                }
            };
            let state_for_job = state2.clone();
            let tx = retry_tx.clone();
            let in_progress = in_progress.clone();
            tokio::spawn(async move {
                let _permit = permit;
                tracing::info!("Processing index job: {}", job.paper_id);
                let is_reembed = job.reembed_only;
                let result = if job.reembed_only {
                    state_for_job.indexer.read().await.reembed_paper(&job.paper_id).await
                } else if job.is_reindex {
                    if job.sections_only {
                        state_for_job.indexer.read().await.reindex_sections_only(&job.paper_id).await
                    } else {
                        state_for_job.indexer.read().await.reindex_paper(&job.paper_id).await
                    }
                } else {
                    let path = std::path::PathBuf::from(&job.file_path);
                    state_for_job.indexer.read()
                        .await
                        .add_document(&path, &job.paper_id)
                        .await
                };
                match result {
                    Ok(paper) => {
                        tracing::info!("Indexed paper: {} ({})", paper.title, paper.id);
                        if is_reembed {
                            let current = state_for_job.reembed_completed.load(Ordering::Relaxed);
                            let total = state_for_job.reembed_total.load(Ordering::Relaxed);
                            if current < total {
                                state_for_job.reembed_completed.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        let paper_exists = paper_exists_or_log(&*state_for_job.store, &paper.id, "after indexing").await;
                        if paper_exists {
                            if let Err(e) = state_for_job.store.update_paper_status(&paper.id, papered::paper::PaperStatus::Indexed.as_str(), None, None).await {
                                tracing::warn!(
                                    "Failed to update paper status for {}: {}",
                                    paper.id,
                                    e
                                );
                            }
                        } else {
                            tracing::warn!(
                                "Paper {} was deleted during indexing; skipping status update",
                                paper.id
                            );
                        }
                    }
                    Err(e) => {
                        let error_msg = e.to_string();
                        tracing::error!("Indexing failed for {}: {}", job.paper_id, error_msg);
                        let paper_exists = paper_exists_or_log(&*state_for_job.store, &job.paper_id, "after indexing error").await;
                        let is_quota_exhausted = error_msg.contains("AllocationQuota")
                            || error_msg.contains("Free quota exhausted");
                        if is_quota_exhausted {
                            tracing::warn!(
                                "API quota exhausted for paper {} — skipping retry. \
                                 Add funds or change model.",
                                job.paper_id
                            );
                        }
                        if paper_exists {
                            let mut status_ok = false;
                            for attempt in 0..2 {
                                if let Err(e) = state_for_job.store
                                    .update_paper_status(
                                        &job.paper_id,
                                        papered::paper::PaperStatus::Failed.as_str(),
                                        Some(&error_msg),
                                        Some(job.retry_count + 1),
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        "Failed to update failed status for paper {} (attempt {}): {}",
                                        job.paper_id,
                                        attempt + 1,
                                        e
                                    );
                                    if attempt == 0 {
                                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                    }
                                } else {
                                    tracing::info!(
                                        "Marked paper {} as failed (retry {}/{})",
                                        job.paper_id,
                                        job.retry_count + 1,
                                        crate::state::MAX_RETRIES
                                    );
                                    status_ok = true;
                                    break;
                                }
                            }
                            if !status_ok {
                                tracing::error!(
                                    "Gave up updating failed status for paper {} — paper stuck at 'processing'",
                                    job.paper_id
                                );
                            }
                        }
                        if !is_quota_exhausted && job.retry_count < crate::state::MAX_RETRIES {
                            let retry_job = papered::util::IndexJob {
                                paper_id: job.paper_id.clone(),
                                file_path: job.file_path.clone(),
                                is_reindex: true,
                                retry_count: job.retry_count + 1,
                                sections_only: job.sections_only,
                                reembed_only: job.reembed_only,
                            };
                            tracing::info!(
                                "Scheduling retry {}/{} for paper {} in {}s",
                                retry_job.retry_count,
                                crate::state::MAX_RETRIES,
                                retry_job.paper_id,
                                crate::state::RETRY_DELAY_SECS
                            );
                            tokio::spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_secs(
                                    crate::state::RETRY_DELAY_SECS,
                                ))
                                .await;
                                if tx.send(retry_job).await.is_err() {
                                    tracing::warn!("Retry channel closed, giving up on paper");
                                }
                            });
                        }
                    }
                }
                in_progress.lock().unwrap().remove(&job.paper_id);
            });
        }
    });

    tasks.spawn(async move {
        let failed_papers = match state
            .store
            .get_papers_by_status_with_retry_below(
                papered::paper::PaperStatus::Failed.as_str(),
                crate::state::MAX_RETRIES,
            )
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to query failed papers for startup retry: {}", e);
                return;
            }
        };
        for paper in &failed_papers {
            if let Some(ref file_path) = paper.file_path {
                let job = papered::util::IndexJob {
                    paper_id: paper.id.clone(),
                    file_path: file_path.clone(),
                    is_reindex: true,
                    retry_count: paper.retry_count,
                    sections_only: false,
                    reembed_only: false,
                };
                tracing::info!(
                    "Queuing failed paper for auto-retry on startup: {} (retry {}/{})",
                    paper.id,
                    paper.retry_count + 1,
                    crate::state::MAX_RETRIES
                );
                if startup_tx.send(job).await.is_err() {
                    tracing::error!(
                        "Startup retry channel closed — paper {} will not be retried",
                        paper.id
                    );
                }
            }
        }
    });
}
