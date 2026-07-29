use serde::{Deserialize, Serialize};

/// Result of a single library sync cycle.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncReport {
    /// Papers newly imported during this cycle.
    pub imported: usize,
    /// Papers that already existed and were skipped.
    pub skipped: usize,
    /// Papers for which a matching PDF was found and indexing was queued.
    pub pdf_found: usize,
    /// Papers imported as metadata-only (no PDF found).
    pub metadata_only: usize,
    /// Papers removed because they no longer exist in the source.
    pub removed: usize,
    /// Errors encountered during this cycle.
    pub errors: Vec<String>,
    /// Version watermark for incremental sync (Zotero-specific).
    pub new_since: u64,
    /// IDs of papers imported in this sync cycle.
    pub imported_ids: Vec<String>,
}

impl SyncReport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}
