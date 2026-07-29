use crate::error::Result;
use crate::paper::{Paper, PaperSource};
use crate::store::vector::VectorStore;
use crate::sync::sync_library;

use crate::sync::SyncReport;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio_util::sync::CancellationToken;

use super::client::ZoteroApi;
use super::source::{ZoteroSource, parse_zotero_key};

pub struct ZoteroSyncer {
    client: Box<dyn ZoteroApi>,
    store: Arc<dyn VectorStore>,
    import_tx: tokio::sync::mpsc::Sender<crate::util::IndexJob>,
    batch_limit: u32,
    pdf_search_paths: Vec<PathBuf>,
    collection_keys: Vec<String>,
    download_pdf: bool,
    last_sync_version: u64,
    recursive_collections: bool,
    cancel: CancellationToken,
    downloaded_tmp_paths: Arc<Mutex<Vec<PathBuf>>>,
}

impl Drop for ZoteroSyncer {
    fn drop(&mut self) {
        if let Ok(paths) = self.downloaded_tmp_paths.lock() {
            for path in paths.iter() {
                if let Err(e) = std::fs::remove_file(path) {
                    tracing::debug!(
                        "Failed to clean up downloaded tmp file {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }
    }
}

impl ZoteroSyncer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<dyn VectorStore>,
        import_tx: tokio::sync::mpsc::Sender<crate::util::IndexJob>,
        batch_limit: u32,
        pdf_search_paths: Vec<PathBuf>,
        collection_keys: Vec<String>,
        download_pdf: bool,
        last_sync_version: u64,
        recursive_collections: bool,
        cancel: CancellationToken,
    ) -> Self {
        Self::with_client(
            Box::new(super::client::ZoteroClient::new()),
            store,
            import_tx,
            batch_limit,
            pdf_search_paths,
            collection_keys,
            download_pdf,
            last_sync_version,
            recursive_collections,
            cancel,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_client(
        client: Box<dyn ZoteroApi>,
        store: Arc<dyn VectorStore>,
        import_tx: tokio::sync::mpsc::Sender<crate::util::IndexJob>,
        batch_limit: u32,
        pdf_search_paths: Vec<PathBuf>,
        collection_keys: Vec<String>,
        download_pdf: bool,
        last_sync_version: u64,
        recursive_collections: bool,
        cancel: CancellationToken,
    ) -> Self {
        Self::cleanup_stale_tmp_files();
        Self {
            client,
            store,
            import_tx,
            batch_limit,
            pdf_search_paths,
            collection_keys,
            download_pdf,
            last_sync_version,
            recursive_collections,
            cancel,
            downloaded_tmp_paths: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Remove temp files left behind by crashed sync cycles.
    /// Only deletes files older than 1 hour to avoid racing with active workers.
    fn cleanup_stale_tmp_files() {
        let prefix = "zotero_dl_";
        let tmp_dir = std::env::temp_dir();
        let cutoff = SystemTime::now()
            .checked_sub(std::time::Duration::from_hours(1))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if let Ok(entries) = std::fs::read_dir(&tmp_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && name.starts_with(prefix)
                {
                    let is_stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .map_or(true, |t| t < cutoff);
                    if is_stale && let Err(e) = std::fs::remove_file(&path) {
                        tracing::debug!("Failed to clean stale tmp file {}: {}", path.display(), e);
                    }
                }
            }
        }
    }

    pub async fn sync(&mut self) -> SyncReport {
        let mut report = SyncReport::new();
        report.new_since = self.last_sync_version;

        if self.is_cancelled() {
            report
                .errors
                .push("Zotero sync cancelled before start.".to_string());
            return report;
        }

        let (existing_papers, existing_by_path) = match self.collect_existing_papers().await {
            Ok(maps) => maps,
            Err(e) => {
                report
                    .errors
                    .push(format!("Failed to read imported papers: {e}"));
                return report;
            }
        };

        let effective_collection_keys =
            if self.recursive_collections && !self.collection_keys.is_empty() {
                match self.expand_collection_keys().await {
                    Ok(keys) => keys,
                    Err(e) => {
                        report
                            .errors
                            .push(format!("Failed to expand Zotero collections: {e}"));
                        return report;
                    }
                }
            } else {
                self.collection_keys.clone()
            };

        let source = ZoteroSource::new(
            self.client.as_ref(),
            self.batch_limit,
            effective_collection_keys.clone(),
            self.download_pdf,
            self.last_sync_version,
            self.recursive_collections,
            self.cancel.clone(),
            self.downloaded_tmp_paths.clone(),
        );

        let stats = sync_library(
            &source,
            &self.store,
            &self.import_tx,
            &existing_papers,
            &existing_by_path,
            &self.pdf_search_paths,
        )
        .await;

        report.imported = stats.imported;
        report.skipped = stats.skipped;
        report.pdf_found = stats.pdf_found;
        report.metadata_only = stats.metadata_only;
        report.imported_ids = stats.imported_ids;
        report.errors.extend(stats.errors);
        report.new_since = source.new_since();

        let (fetched_keys, full_errors) = match source.fetch_all_keys().await {
            Ok(v) => v,
            Err(e) => {
                report
                    .errors
                    .push(format!("Failed to fetch full Zotero item list: {e}"));
                return report;
            }
        };
        report.errors.extend(full_errors.clone());

        let all_collections_ok = effective_collection_keys.is_empty() || full_errors.is_empty();
        if effective_collection_keys.is_empty() || all_collections_ok {
            match self
                .remove_missing_papers(&fetched_keys, &existing_papers)
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

    /// Expand the configured collection keys to include all nested sub-collections.
    /// Only called when `recursive_collections` is enabled.
    async fn expand_collection_keys(&self) -> Result<Vec<String>> {
        let all_collections = self.client.list_collections().await?;
        let mut by_parent: HashMap<String, Vec<String>> = HashMap::new();
        for c in &all_collections {
            if let Some(ref parent) = c.data.parentCollection {
                by_parent
                    .entry(parent.clone())
                    .or_default()
                    .push(c.data.key.clone());
            }
        }

        let mut result = Vec::new();
        let mut stack: Vec<String> = self.collection_keys.clone();
        let mut visited = HashSet::new();

        while let Some(key) = stack.pop() {
            if visited.insert(key.clone()) {
                result.push(key.clone());
                if let Some(children) = by_parent.get(&key) {
                    for child in children {
                        stack.push(child.clone());
                    }
                }
            }
        }

        Ok(result)
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Collect existing papers in a single full-table scan, building both
    /// lookup maps at once: Zotero-imported papers keyed by `zotero_key`, and
    /// all papers (any source) keyed by `file_path`.
    async fn collect_existing_papers(
        &self,
    ) -> Result<(HashMap<String, Paper>, HashMap<String, Paper>)> {
        let mut by_key = HashMap::new();
        let mut by_path = HashMap::new();
        crate::sync::scan_paper_batches!(
            self.store,
            self.cancel,
            "Zotero sync cancelled while collecting existing papers",
            |paper| {
                if paper.source == Some(PaperSource::Zotero)
                    && let Some(key) = paper.extra.as_deref().and_then(parse_zotero_key)
                {
                    by_key.insert(key, paper.clone());
                }
                if let Some(ref path) = paper.file_path {
                    by_path.insert(path.clone(), paper.clone());
                }
            }
        );
        Ok((by_key, by_path))
    }

    /// Remove Zotero-imported papers whose keys are no longer present in
    /// Zotero. Iterates the already-collected map instead of rescanning.
    async fn remove_missing_papers(
        &self,
        fetched_keys: &HashSet<String>,
        existing_papers: &HashMap<String, Paper>,
    ) -> Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(crate::error::PaperedError::Cancelled(
                "Zotero sync cancelled while removing missing papers".to_string(),
            ));
        }
        let to_remove: Vec<(String, String)> = existing_papers
            .iter()
            .filter(|(k, _)| !fetched_keys.contains(*k))
            .map(|(k, p)| (k.clone(), p.id.clone()))
            .collect();
        Ok(crate::sync::remove_missing_papers(&self.store, &to_remove, "Zotero").await)
    }
}

#[cfg(test)]
mod tests;
