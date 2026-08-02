use crate::error::Result;
use crate::paper::{Paper, PaperStatus};
use crate::store::vector::VectorStore;
use crate::util::IndexJob;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

pub mod report;
pub use report::SyncReport;

/// Statistics from a sync cycle.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SyncStats {
    pub imported: usize,
    pub skipped: usize,
    pub pdf_found: usize,
    pub metadata_only: usize,
    pub errors: Vec<String>,
    pub imported_ids: Vec<String>,
}

impl SyncStats {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// A source of library papers that can be synced into Papered.
#[async_trait::async_trait]
pub trait LibrarySource: Send + Sync {
    type Item: Send + 'static;

    /// Fetch items that should be considered for import in this sync cycle.
    /// Returns the items plus any non-fatal fetch errors.
    async fn fetch_items(&self) -> Result<(Vec<Self::Item>, Vec<String>)>;

    /// Extract the source-specific ID that maps to Papered's `extra` or source-specific key field.
    fn extract_id(item: &Self::Item) -> String;

    /// Build a Paper from a source item. Called for new items only (item is consumed).
    ///
    /// # Errors
    ///
    /// Returns an error if the source item cannot be converted into a valid [`Paper`].
    fn to_paper(item: Self::Item) -> Result<Paper>;

    /// Try to find a PDF file for the given item.
    async fn find_pdf(&self, item: &Self::Item, search_paths: &[PathBuf]) -> Option<PathBuf>;

    /// Return `true` for source items that should be silently skipped (e.g. attachments, notes).
    fn should_skip(_item: &Self::Item) -> bool {
        false
    }

    /// Update an already-imported paper with new source metadata.
    /// Returns `Some(updated_paper)` if changes were made, `None` if nothing changed.
    async fn update_existing(&self, _paper: &Paper, _item: &Self::Item) -> Result<Option<Paper>> {
        Ok(None)
    }

    /// Return `false` if an item without a PDF should be skipped rather than imported as
    /// metadata-only.
    #[must_use]
    fn allow_metadata_only(_paper: &Paper) -> bool {
        true
    }

    /// Hook called when the indexing worker queue is closed but a PDF was found.
    /// Sources can override the paper `status`/`file_path` before it is stored as metadata-only.
    fn on_queue_closed(_paper: &mut Paper, _path: &PathBuf) {}

    /// Return `true` if a `to_paper` error should be counted as skipped instead of an error.
    #[must_use]
    fn is_skip_error(_e: &crate::error::PaperedError) -> bool {
        false
    }

    /// Return `true` to cancel the remainder of the sync cycle.
    fn is_cancelled(&self) -> bool {
        false
    }

    /// Human-readable source name for logging.
    fn source_name() -> &'static str;
}

/// Search `paths` for a PDF whose filename matches `terms` on a blocking
/// thread, racing the search against `cancel`. Shared by
/// [`LibrarySource::find_pdf`] implementations.
pub async fn find_pdf_with_cancel(
    cancel: &tokio_util::sync::CancellationToken,
    paths: Vec<PathBuf>,
    terms: Vec<String>,
) -> Option<PathBuf> {
    let search_handle =
        tokio::task::spawn_blocking(move || crate::util::find_pdf_sync(&paths, &terms));
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        result = search_handle => result.unwrap_or_else(|e| {
            tracing::warn!("PDF search task failed: {e}");
            None
        }).map(PathBuf::from),
    }
}

/// Scan every paper in `$store` in batches of 1000, running `$body` with
/// `$paper` bound to each `&Paper`. Between batches, checks the cancellation
/// token and returns `PaperedError::Cancelled($message)` from the enclosing
/// function when set; store errors propagate with `?`.
///
/// Implemented as a macro rather than a closure-taking function: the syncers
/// are driven from `tokio::spawn`, and neither async closures nor boxed-future
/// callbacks can borrow their captured state (`&mut` accumulators, `&self`)
/// across the higher-ranked batch lifetime while staying provably `Send`.
/// Expanding the loop inline preserves the original borrows exactly.
macro_rules! scan_paper_batches {
    ($store:expr, $cancel:expr, $message:expr, |$paper:ident| $body:block) => {{
        let mut offset = 0;
        loop {
            if $cancel.is_cancelled() {
                return Err($crate::error::PaperedError::Cancelled($message.to_string()));
            }
            let batch = $store.list_papers(1000, offset).await?;
            if batch.is_empty() {
                break;
            }
            for $paper in &batch {
                $body
            }
            offset += batch.len();
        }
    }};
}

pub(crate) use scan_paper_batches;

/// Insert a paper, recording the failure in `stats`. Returns `false` when the
/// insert failed, so the caller can `continue` without counting the item.
async fn insert_paper_tracking(
    store: &Arc<dyn VectorStore>,
    paper: &Paper,
    item_id: &str,
    stats: &mut SyncStats,
) -> bool {
    if let Err(e) = store.insert_paper(paper).await {
        stats
            .errors
            .push(format!("Failed to insert paper {item_id}: {e}"));
        return false;
    }
    true
}

/// Run one sync cycle: fetch source items, update existing papers, filter out duplicates,
/// convert to Paper, discover PDFs, insert into store, and queue indexing jobs.
///
/// Kept as a single linear pipeline because splitting it would require threading
/// most of its state through many small helpers without improving readability.
#[allow(clippy::too_many_lines)]
#[allow(clippy::implicit_hasher)]
pub async fn sync_library<S: LibrarySource>(
    source: &S,
    store: &Arc<dyn VectorStore>,
    import_tx: &mpsc::Sender<IndexJob>,
    existing_papers: &HashMap<String, Paper>,
    existing_by_path: &HashMap<String, Paper>,
    pdf_search_paths: &[PathBuf],
) -> SyncStats {
    let mut stats = SyncStats::new();

    let (items, fetch_errors) = match source.fetch_items().await {
        Ok(v) => v,
        Err(e) => {
            stats
                .errors
                .push(format!("{} fetch failed: {e}", S::source_name()));
            return stats;
        }
    };
    stats.errors.extend(fetch_errors);

    for item in items {
        if source.is_cancelled() {
            stats
                .errors
                .push("Sync cancelled by user request.".to_string());
            break;
        }

        if S::should_skip(&item) {
            stats.skipped += 1;
            continue;
        }

        let item_id = S::extract_id(&item);

        if let Some(existing) = existing_papers.get(&item_id) {
            match source.update_existing(existing, &item).await {
                Ok(Some(updated)) => {
                    if let Err(e) = store.update_paper(&updated).await {
                        stats
                            .errors
                            .push(format!("Failed to update paper {item_id}: {e}"));
                    }
                }
                Ok(None) => {
                    stats.skipped += 1;
                }
                Err(e) => {
                    stats
                        .errors
                        .push(format!("Failed to sync metadata for {item_id}: {e}"));
                }
            }
            continue;
        }

        let pdf_path = source.find_pdf(&item, pdf_search_paths).await;

        if let Some(ref path) = pdf_path {
            let path_str = path.to_string_lossy().into_owned();
            if let Some(existing) = existing_by_path.get(&path_str) {
                stats.skipped += 1;
                tracing::info!(
                    "Skipping {} item {}: PDF already imported as '{}' ({})",
                    S::source_name(),
                    item_id,
                    existing.title,
                    existing.id
                );
                continue;
            }
        }

        let mut paper = match S::to_paper(item) {
            Ok(p) => p,
            Err(e) => {
                if S::is_skip_error(&e) {
                    stats.skipped += 1;
                } else {
                    stats
                        .errors
                        .push(format!("Failed to convert item {item_id}: {e}"));
                }
                continue;
            }
        };

        if let Some(ref path) = pdf_path {
            paper.file_path = Some(path.to_string_lossy().into_owned());
            paper.status = PaperStatus::Processing;

            let job = IndexJob::new(paper.id.clone(), path.to_string_lossy().into_owned());

            if import_tx.send(job).await.is_ok() {
                if !insert_paper_tracking(store, &paper, &item_id, &mut stats).await {
                    continue;
                }
                stats.pdf_found += 1;
            } else {
                tracing::warn!(
                    "Indexing queue closed, importing {} ({}) as metadata-only",
                    paper.title,
                    item_id,
                );
                S::on_queue_closed(&mut paper, path);
                if !insert_paper_tracking(store, &paper, &item_id, &mut stats).await {
                    continue;
                }
                stats.metadata_only += 1;
            }
        } else {
            if !S::allow_metadata_only(&paper) {
                stats.skipped += 1;
                continue;
            }
            if !insert_paper_tracking(store, &paper, &item_id, &mut stats).await {
                continue;
            }
            stats.metadata_only += 1;
        }

        stats.imported += 1;
        stats.imported_ids.push(paper.id);
    }

    stats
}

/// Remove papers from `store` that are no longer present in the source library.
///
/// `to_remove` is an iterator of `(source_key, paper_id)` pairs. Each paper is
/// deleted from the store and a tracing log is emitted. Returns the number of
/// successfully removed papers.
pub async fn remove_missing_papers(
    store: &Arc<dyn VectorStore>,
    to_remove: &[(String, String)],
    source_name: &str,
) -> usize {
    let mut removed = 0;
    for (source_key, paper_id) in to_remove {
        match store.delete_paper(paper_id).await {
            Ok(()) => {
                removed += 1;
                tracing::info!(
                    "Removed paper {paper_id} ({source_name} key {source_key}) — no longer in {source_name}"
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to delete paper {paper_id} ({source_name} key {source_key}): {e}"
                );
            }
        }
    }
    removed
}
