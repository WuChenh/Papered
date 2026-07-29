//! Embedding model probing, vector rebuild policies, and re-embed queuing.

use super::AppState;
use std::sync::atomic::Ordering;
use thiserror::Error;

/// What to do with stored vectors when the embedding model is (re)probed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmbeddingRebuildPolicy {
    /// Probe the model only — never touch stored vectors.
    ProbeOnly,
    /// Clear vectors and re-embed only when the detected dimension differs
    /// from the store's current dimension.
    RebuildIfChanged,
    /// Always clear vectors and re-embed.
    ForceRebuild,
}

/// Which stage of [`AppState::handle_embedding_model_change`] failed.
#[derive(Error, Debug)]
pub(crate) enum EmbeddingChangeError {
    /// The model probe failed — the daemon stays in degraded mode.
    #[error("embedding probe failed: {0}")]
    Probe(papered::PaperedError),
    /// The model works, but clearing/resetting the vector store failed.
    #[error("embedding reset failed: {0}")]
    Reset(papered::PaperedError),
}

/// Outcome of a successful [`AppState::handle_embedding_model_change`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct EmbeddingModelChange {
    /// Dimension detected from the model (0 if it was never reported).
    pub(crate) detected_dim: usize,
    /// Vectors were cleared and metadata refreshed — the caller must queue a
    /// full re-embed (`reembed_all_now` / `spawn_reembed_all`).
    pub(crate) rebuilt: bool,
}

impl AppState {
    /// Test the embedding model and return its detected dimension.
    ///
    /// This is the sole writer of `embedding_model_ready`: `true` on a
    /// successful probe, `false` (and `Err`) when the model is unavailable.
    /// It never touches stored vectors.
    pub async fn test_and_prepare_embedding(&self) -> papered::error::Result<usize> {
        let emb = self.embedding.read().await;
        let dim = emb.detected_dimension();
        if dim > 0 {
            return Ok(dim);
        }
        match emb.embed_single("test").await {
            Ok(_) => {
                let dim = emb.detected_dimension();
                tracing::info!("Embedding model tested: dim={dim}");
                self.embedding_model_ready.store(true, Ordering::Relaxed);
                Ok(dim)
            }
            Err(e) => {
                tracing::warn!("Embedding model test failed: {e}");
                self.embedding_model_ready.store(false, Ordering::Relaxed);
                Err(e)
            }
        }
    }

    /// Clear all vectors (re-creating the table at `detected` dimensions) and
    /// refresh the `embedding_dimension` / `embedding_fingerprint` metadata.
    async fn reset_embedding_store(&self, detected: usize) -> papered::error::Result<()> {
        self.store.clear_all_vectors(detected).await?;
        self.store
            .set_meta("embedding_dimension", &detected.to_string())
            .await
            .map_err(|e| {
                papered::PaperedError::Config(
                    format!("Failed to set embedding_dimension meta: {e}"),
                    None,
                )
            })?;
        if let Some(fp) = self.config.read().await.embedding_fingerprint() {
            let _ = self.store.set_meta("embedding_fingerprint", &fp).await;
        }
        Ok(())
    }

    /// Test the embedding model, clear vectors, and update metadata.
    pub async fn test_and_prepare_embedding_store(&self) -> papered::error::Result<usize> {
        let detected = self.test_and_prepare_embedding().await?;
        if detected > 0 {
            self.reset_embedding_store(detected).await?;
        }
        Ok(detected)
    }

    /// Probe the embedding model and, per `policy`, clear stored vectors and
    /// refresh store metadata. When the result's `rebuilt` flag is true the
    /// caller must queue a full re-embed
    /// ([`reembed_all_now`](Self::reembed_all_now) or
    /// [`spawn_reembed_all`](Self::spawn_reembed_all)).
    pub async fn handle_embedding_model_change(
        &self,
        policy: EmbeddingRebuildPolicy,
    ) -> Result<EmbeddingModelChange, EmbeddingChangeError> {
        let detected_dim = self
            .test_and_prepare_embedding()
            .await
            .map_err(EmbeddingChangeError::Probe)?;
        let current_dim = self.store.store_dimension().await;
        let rebuild = match policy {
            EmbeddingRebuildPolicy::ProbeOnly => false,
            EmbeddingRebuildPolicy::RebuildIfChanged => {
                detected_dim > 0 && current_dim != Some(detected_dim)
            }
            EmbeddingRebuildPolicy::ForceRebuild => detected_dim > 0,
        };
        if !rebuild {
            return Ok(EmbeddingModelChange {
                detected_dim,
                rebuilt: false,
            });
        }
        if current_dim != Some(detected_dim) {
            tracing::info!(
                "Embedding dimension changed ({} -> {}) — clearing vectors; re-embed required",
                current_dim.unwrap_or(0),
                detected_dim
            );
        } else {
            tracing::info!("Clearing vectors for dimension {detected_dim} — re-embed required");
        }
        self.reset_embedding_store(detected_dim)
            .await
            .map_err(EmbeddingChangeError::Reset)?;
        Ok(EmbeddingModelChange {
            detected_dim,
            rebuilt: true,
        })
    }

    /// Queue every paper for re-embedding and publish the progress counters.
    /// Returns the number of papers queued.
    pub async fn reembed_all_now(&self) -> usize {
        let total_queued = self.queue_all_papers_for_reembed().await;
        self.reembed_total.store(total_queued, Ordering::Relaxed);
        self.reembed_completed.store(0, Ordering::Relaxed);
        tracing::info!("Re-embed queueing complete (total: {total_queued})");
        total_queued
    }

    /// Spawn [`reembed_all_now`](Self::reembed_all_now) in the background.
    pub fn spawn_reembed_all(self: &std::sync::Arc<Self>) {
        let state = self.clone();
        tokio::spawn(async move {
            state.reembed_all_now().await;
        });
    }

    pub async fn queue_all_papers_for_reembed(&self) -> usize {
        use papered::{PaperPager, StrLabel, paper::PaperStatus};

        let mut total_queued = 0usize;
        let mut pager = PaperPager::new(&self.store, 1000);
        loop {
            let (batch, sections_batch) = match pager.next_batch_with_sections().await {
                Ok(Some(b)) => b,
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Failed to fetch papers for re-embed: {e}");
                    break;
                }
            };
            for (paper, sections) in batch.iter().zip(sections_batch) {
                if paper.status != PaperStatus::Indexed && paper.status != PaperStatus::Failed {
                    continue;
                }
                let has_sections = !sections.sections.is_empty();
                let has_file = paper.file_path.is_some();
                if has_sections || has_file {
                    let _ = self
                        .store
                        .update_paper_status(
                            &paper.id,
                            PaperStatus::Processing.as_str(),
                            None,
                            None,
                        )
                        .await;
                    let (file_path, sections_only, reembed_only) = if has_sections {
                        (String::new(), false, true)
                    } else {
                        (paper.file_path.clone().unwrap_or_default(), true, false)
                    };
                    let job = papered::util::IndexJob {
                        paper_id: paper.id.clone(),
                        file_path,
                        is_reindex: false,
                        retry_count: 0,
                        sections_only,
                        reembed_only,
                    };
                    if self.import_tx.send(job).await.is_ok() {
                        total_queued += 1;
                    } else {
                        tracing::warn!("Failed to queue re-embed for paper {}", paper.id);
                    }
                } else {
                    let _ = self
                        .store
                        .update_paper_status(
                            &paper.id,
                            PaperStatus::Failed.as_str(),
                            Some("Cannot re-embed: no sections and no PDF file"),
                            None,
                        )
                        .await;
                }
            }
        }
        total_queued
    }
}
