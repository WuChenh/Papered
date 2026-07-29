//! Background synchronizer with the Lattice desktop application.
//!
//! `LatticeSyncer` periodically enumerates the full Lattice library through
//! the search API, imports papers not yet in Papered (optionally discovering
//! matching PDF files on disk for full indexing), and removes previously
//! imported papers that no longer exist in Lattice.

use crate::error::{PaperedError, Result};
use crate::paper::{Paper, PaperSource};
use crate::store::vector::VectorStore;
use crate::sync::{LibrarySource, sync_library};

use crate::sync::SyncReport;
use crate::util;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use super::client::LatticeClient;
use super::types::{LatticeCollection, LatticePaperDetail};
use tokio_util::sync::CancellationToken;

/// Capabilities the Lattice Local API must advertise for sync to work.
const REQUIRED_CAPABILITIES: &[&str] = &["search", "paper-detail"];

/// Hard cap on pagination rounds, so a misbehaving API (always returning a
/// full page) cannot spin the sync loop forever.
const MAX_ENUMERATION_PAGES: u32 = 100;

/// Split of one sync cycle's work, computed from the full enumeration and
/// the already-imported Lattice ids.
struct SyncPlan {
    /// Enumerated ids not yet imported — fetch details and import these.
    to_fetch: Vec<String>,
    /// Imported ids missing from the enumeration — remove these.
    to_remove: Vec<String>,
    /// Enumerated ids already imported — counted as skipped, details are
    /// not refreshed.
    up_to_date: usize,
}

/// Diff the enumerated library ids against the already-imported ones.
///
/// Pure and order-preserving (the fetch list follows enumeration order, the
/// remove list is sorted) so it can be unit-tested without HTTP.
fn plan_sync(enumerated_ids: &[String], existing_ids: &HashSet<String>) -> SyncPlan {
    let enumerated_set: HashSet<&String> = enumerated_ids.iter().collect();
    let mut seen = HashSet::new();
    let to_fetch: Vec<String> = enumerated_ids
        .iter()
        .filter(|id| !existing_ids.contains(*id) && seen.insert(*id))
        .cloned()
        .collect();
    let mut to_remove: Vec<String> = existing_ids
        .iter()
        .filter(|id| !enumerated_set.contains(id))
        .cloned()
        .collect();
    to_remove.sort_unstable();
    SyncPlan {
        up_to_date: enumerated_set.len() - to_fetch.len(),
        to_fetch,
        to_remove,
    }
}

/// Map configured collection names to collection ids.
///
/// Names are matched case-sensitively. Unknown names are logged as warnings
/// and ignored. Pure so it can be unit-tested without HTTP.
fn resolve_collection_names_to_ids(
    names: &[String],
    collections: &[LatticeCollection],
) -> Vec<String> {
    let mut ids = Vec::new();
    for name in names {
        let matches: Vec<&LatticeCollection> =
            collections.iter().filter(|c| c.name == *name).collect();
        if matches.is_empty() {
            tracing::warn!(
                "Configured Lattice collection '{}' not found; ignoring it",
                name
            );
            continue;
        }
        ids.extend(matches.iter().map(|c| c.id.clone()));
    }
    ids
}

/// Synchronizes papers from a Lattice library into Papered.
pub struct LatticeSyncer {
    client: LatticeClient,
    store: Arc<dyn VectorStore>,
    import_tx: tokio::sync::mpsc::Sender<crate::util::IndexJob>,
    batch_limit: u32,
    pdf_search_paths: Vec<PathBuf>,
    /// Collection names to sync. Empty means sync all collections.
    collection_names: Vec<String>,
    cancel: CancellationToken,
}

impl LatticeSyncer {
    /// Create a new syncer.
    pub fn new(
        store: Arc<dyn VectorStore>,
        import_tx: tokio::sync::mpsc::Sender<crate::util::IndexJob>,
        batch_limit: u32,
        pdf_search_paths: Vec<PathBuf>,
    ) -> Result<Self> {
        Self::with_collections(
            store,
            import_tx,
            batch_limit,
            pdf_search_paths,
            Vec::new(),
            CancellationToken::new(),
        )
    }

    /// Create a new syncer scoped to specific collection names.
    pub fn with_collections(
        store: Arc<dyn VectorStore>,
        import_tx: tokio::sync::mpsc::Sender<crate::util::IndexJob>,
        batch_limit: u32,
        pdf_search_paths: Vec<PathBuf>,
        collection_names: Vec<String>,
        cancel: CancellationToken,
    ) -> Result<Self> {
        Ok(Self {
            client: LatticeClient::new()?,
            store,
            import_tx,
            batch_limit,
            pdf_search_paths,
            collection_names,
            cancel,
        })
    }

    /// Run one sync cycle: enumerate the Lattice library (optionally scoped
    /// to configured collections), import papers not yet in Papered, and
    /// remove imported papers that are no longer in the selected sync domain.
    pub async fn sync(&self) -> SyncReport {
        let mut report = SyncReport::new();
        if self.is_cancelled() {
            report
                .errors
                .push("Lattice sync cancelled before start.".to_string());
            return report;
        }

        if let Err(e) = self.check_status().await {
            report.errors.push(e);
            return report;
        }

        let existing_papers = match self.collect_imported_lattice_papers().await {
            Ok(map) => map,
            Err(e) => {
                report
                    .errors
                    .push(format!("Failed to read imported papers: {e}"));
                return report;
            }
        };

        // Resolve configured collection names to ids. Empty configuration
        // means "sync all collections" and keeps the existing full-library
        // behaviour.
        let collection_ids = match self.resolve_collection_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                report.errors.push(e);
                return report;
            }
        };

        // Enumeration scoped to the configured collections. If it fails (or
        // is cancelled) partway the id set is incomplete, so abort the cycle
        // without importing or removing anything — an API failure must never
        // look like a mass deletion.
        //
        // When `collection_ids` is non-empty, deletion reconciliation is
        // deliberately limited to those collections: any previously imported
        // Lattice paper that is not in the selected collections is treated as
        // removed, because the user explicitly narrowed the sync domain.
        // This matches Zotero collection-filter semantics.
        let enumerated_ids = match self.enumerate_library(collection_ids.as_deref()).await {
            Ok(ids) => ids,
            Err(e) => {
                report.errors.push(e);
                return report;
            }
        };

        let existing_ids: HashSet<String> = existing_papers.keys().cloned().collect();
        let plan = plan_sync(&enumerated_ids, &existing_ids);

        let (items, fetch_errors) = self.fetch_new_details(&plan.to_fetch).await;

        let effective_paths = if self.pdf_search_paths.is_empty() {
            vec![default_lattice_data_dir()]
        } else {
            self.pdf_search_paths.clone()
        };

        let source = LatticeSource::new(items, fetch_errors, self.cancel.clone());
        let stats = sync_library(
            &source,
            &self.store,
            &self.import_tx,
            &existing_papers,
            &HashMap::new(),
            &effective_paths,
        )
        .await;

        report.imported = stats.imported;
        report.skipped = stats.skipped + plan.up_to_date;
        report.pdf_found = stats.pdf_found;
        report.metadata_only = stats.metadata_only;
        report.imported_ids = stats.imported_ids;
        report.errors.extend(stats.errors);

        // Deletion reconciliation needs a complete, uncancelled enumeration;
        // both were guaranteed above, so only the cancel-during-import case
        // remains to be checked here.
        if !self.cancel.is_cancelled() {
            match self
                .remove_missing_papers(&plan.to_remove, &existing_papers)
                .await
            {
                Ok(count) => report.removed = count,
                Err(e) => report
                    .errors
                    .push(format!("Failed to remove missing papers: {e}")),
            }
        }

        report
    }

    /// Verify Lattice is reachable and advertises the capabilities sync
    /// needs. Returns the error message to report on failure.
    async fn check_status(&self) -> std::result::Result<(), String> {
        let status = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                return Err("Lattice sync cancelled during status check.".to_string());
            }
            result = self.client.status() => result,
        };
        let status = status.map_err(|e| format!("Lattice status check failed: {e}"))?;
        let missing: Vec<&str> = REQUIRED_CAPABILITIES
            .iter()
            .copied()
            .filter(|cap| !status.capabilities.iter().any(|c| c == cap))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "Lattice API {} (app {}) lacks required capabilities: {}",
                status.api_version,
                status.app_version,
                missing.join(", ")
            ));
        }
        tracing::info!(
            "Lattice status ok: app {} (API {}), capabilities: {}",
            status.app_version,
            status.api_version,
            status.capabilities.join(", ")
        );
        Ok(())
    }

    /// Resolve configured collection names to collection ids.
    ///
    /// Returns `Ok(None)` when no collection filter is configured (sync all).
    /// Returns `Ok(Some(Vec<String>))` when one or more valid names were
    /// resolved. Unknown names are logged as warnings and ignored instead of
    /// aborting the whole sync.
    async fn resolve_collection_ids(&self) -> std::result::Result<Option<Vec<String>>, String> {
        if self.collection_names.is_empty() {
            return Ok(None);
        }
        let collections = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                return Err("Lattice sync cancelled while listing collections.".to_string());
            }
            result = self.client.list_collections() => result,
        };
        let collections =
            collections.map_err(|e| format!("Failed to list Lattice collections: {e}"))?;
        let ids = resolve_collection_names_to_ids(&self.collection_names, &collections);
        if ids.is_empty() {
            return Err("No configured Lattice collections were found; aborting sync.".to_string());
        }
        Ok(Some(ids))
    }

    /// Enumerate paper ids by paging the search API with an empty query.
    ///
    /// If `collection_ids` is `None`, the whole library is enumerated.
    /// If it is `Some(&[...])`, each collection is enumerated in turn and the
    /// results are deduplicated (a paper may belong to multiple collections).
    /// [`MAX_ENUMERATION_PAGES`] caps the loop per collection.
    async fn enumerate_library(
        &self,
        collection_ids: Option<&[String]>,
    ) -> std::result::Result<Vec<String>, String> {
        match collection_ids {
            None => self.enumerate_collection(None).await,
            Some(ids) => {
                let mut seen = HashSet::new();
                let mut all_ids = Vec::new();
                for id in ids {
                    let ids = self.enumerate_collection(Some(id.as_str())).await?;
                    for id in ids {
                        if seen.insert(id.clone()) {
                            all_ids.push(id);
                        }
                    }
                }
                Ok(all_ids)
            }
        }
    }

    /// Enumerate one collection (or the whole library when `collection` is
    /// `None`), returning ids in most-recently-added order.
    async fn enumerate_collection(
        &self,
        collection: Option<&str>,
    ) -> std::result::Result<Vec<String>, String> {
        let mut ids = Vec::new();
        let mut offset: u32 = 0;
        let mut complete = false;
        for _ in 0..MAX_ENUMERATION_PAGES {
            if self.cancel.is_cancelled() {
                return Err("Lattice sync cancelled while enumerating library.".to_string());
            }
            let page = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    return Err("Lattice sync cancelled while enumerating library.".to_string());
                }
                result = self.client.search_collection_page("", collection, self.batch_limit, offset) => result,
            };
            let page = page.map_err(|e| format!("Failed to enumerate Lattice library: {e}"))?;
            let count = page.papers.len() as u32;
            ids.extend(page.papers.into_iter().map(|p| p.id));
            offset += count;
            if count == 0 || count < self.batch_limit {
                complete = true;
                break;
            }
        }
        if !complete {
            return Err(format!(
                "Lattice library enumeration hit the {MAX_ENUMERATION_PAGES}-page cap; \
                 aborting sync to avoid mistaking a partial listing for deletions"
            ));
        }
        Ok(ids)
    }

    /// Fetch full details for papers not yet imported. Per-paper failures
    /// are collected as non-fatal errors; cancellation stops fetching early.
    async fn fetch_new_details(&self, ids: &[String]) -> (Vec<LatticePaperDetail>, Vec<String>) {
        let mut items = Vec::with_capacity(ids.len());
        let mut errors = Vec::new();
        for id in ids {
            if self.is_cancelled() {
                break;
            }
            let detail = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => break,
                result = self.client.get_paper(id) => result,
            };
            match detail {
                Ok(detail) => items.push(detail),
                Err(e) => errors.push(format!("Failed to fetch Lattice detail for {id}: {e}")),
            }
        }
        (items, errors)
    }

    /// Remove previously imported papers whose Lattice ids were absent from
    /// this cycle's enumeration. When collection filtering is active, the
    /// enumeration only covers the selected collections, so papers outside
    /// those collections are removed — this matches Zotero collection-filter
    /// semantics and is the intended behaviour.
    async fn remove_missing_papers(
        &self,
        to_remove: &[String],
        existing_papers: &HashMap<String, Paper>,
    ) -> Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(PaperedError::Cancelled(
                "Lattice sync cancelled while removing missing papers".to_string(),
            ));
        }
        let pairs: Vec<(String, String)> = to_remove
            .iter()
            .filter_map(|id| existing_papers.get(id).map(|p| (id.clone(), p.id.clone())))
            .collect();
        Ok(crate::sync::remove_missing_papers(&self.store, &pairs, "Lattice").await)
    }

    /// Collect all papers imported from Lattice, keyed by `lattice_id`.
    async fn collect_imported_lattice_papers(&self) -> Result<HashMap<String, Paper>> {
        let mut map = HashMap::new();
        crate::sync::scan_paper_batches!(
            self.store,
            self.cancel,
            "Lattice sync cancelled while collecting imported papers",
            |paper| {
                if let Some(ref extra) = paper.extra
                    && let Some(lattice_id) = parse_lattice_id(extra)
                {
                    map.insert(lattice_id, paper.clone());
                }
            }
        );
        Ok(map)
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// Pre-fetched Lattice items fed through the generic [`sync_library`]
/// pipeline. All HTTP work happens in [`LatticeSyncer::sync`] before the
/// pipeline runs, so `fetch_items` only drains the buffers.
struct LatticeSource {
    fetched: Mutex<(Vec<LatticePaperDetail>, Vec<String>)>,
    cancel: CancellationToken,
}

impl LatticeSource {
    fn new(items: Vec<LatticePaperDetail>, errors: Vec<String>, cancel: CancellationToken) -> Self {
        Self {
            fetched: Mutex::new((items, errors)),
            cancel,
        }
    }
}

#[async_trait::async_trait]
impl LibrarySource for LatticeSource {
    type Item = LatticePaperDetail;

    async fn fetch_items(&self) -> Result<(Vec<Self::Item>, Vec<String>)> {
        let mut fetched = self.fetched.lock().unwrap();
        Ok((
            std::mem::take(&mut fetched.0),
            std::mem::take(&mut fetched.1),
        ))
    }

    fn extract_id(item: &Self::Item) -> String {
        item.id.clone()
    }

    fn to_paper(item: Self::Item) -> Result<Paper> {
        let csl_json = item.csl_item.as_ref().map(std::string::ToString::to_string);
        let extra = super::build_lattice_extra(&item, csl_json);

        let mut paper = Paper::new(item.title);
        paper.authors = item.authors;
        paper.published_date = item.year.map(|y| y.to_string());
        paper.venue = item.journal;
        paper.doi = item.doi;
        paper.keywords = extract_keywords_from_csl(item.csl_item.as_ref());
        paper.extra = Some(extra);
        paper.source = Some(PaperSource::Lattice);
        // Carry Lattice's stored abstract. For PDF-backed papers the indexer
        // extracts its own abstract into a section and clears this field; for
        // metadata-only imports it is the only abstract source.
        paper.abstract_text = item.abstract_text.clone();
        Ok(paper)
    }

    async fn find_pdf(&self, item: &Self::Item, search_paths: &[PathBuf]) -> Option<PathBuf> {
        // Prefer the exact path Lattice resolved from its security-scoped
        // bookmark — far more reliable than guessing by filename. Falls back to
        // the filesystem search when Lattice has no path or the file is
        // inaccessible (e.g. inside Lattice's sandbox container).
        if let Some(ref p) = item.pdf_path {
            let path = PathBuf::from(p);
            match tokio::fs::try_exists(&path).await {
                Ok(true) => return Some(path),
                Ok(false) => {
                    tracing::debug!(
                        "Lattice pdfPath '{}' does not exist; falling back to filesystem search",
                        p
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        "Lattice pdfPath '{}' not accessible ({e}); falling back to filesystem search",
                        p
                    );
                }
            }
        }
        crate::sync::find_pdf_with_cancel(
            &self.cancel,
            search_paths.to_vec(),
            util::pdf_search_terms(&item.title, 40, &item.citekey, item.doi.as_deref()),
        )
        .await
    }

    fn source_name() -> &'static str {
        "lattice"
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// Default Lattice data directory (sandboxed container on macOS).
fn default_lattice_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library")
            .join("Containers")
            .join("com.aurelian.Lattice")
            .join("Data")
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Lattice")
    }
}

/// Parse the Lattice paper ID from a Papered paper's extra JSON.
fn parse_lattice_id(extra: &str) -> Option<String> {
    crate::util::parse_extra_key(extra, "lattice_id")
}

fn extract_keywords_from_csl(csl_item: Option<&serde_json::Value>) -> Vec<String> {
    let Some(csl) = csl_item else {
        return Vec::new();
    };
    let raw = match csl.get("keyword") {
        Some(serde_json::Value::String(s)) => s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    crate::util::dedup_strings(raw, false)
}

#[cfg(test)]
mod tests;
