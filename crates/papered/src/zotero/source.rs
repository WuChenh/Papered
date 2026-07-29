use crate::error::{PaperedError, Result};
use crate::paper::{Paper, PaperSource, PaperStatus};
use crate::sync::LibrarySource;
use crate::util;
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use super::client::ZoteroApi;
use super::types::{ZoteroItem, ZoteroItemData};
use super::{build_zotero_extra, zotero_data_dir};

/// Zotero-specific [`LibrarySource`] implementation.
///
/// Wraps the Zotero local API client and handles PDF resolution (local storage,
/// downloaded attachment, or search-path fallback), incremental fetching, and
/// metadata-only updates for already-imported papers.
pub struct ZoteroSource<'a> {
    client: &'a dyn ZoteroApi,
    batch_limit: u32,
    collection_keys: Vec<String>,
    download_pdf: bool,
    since: u64,
    recursive_collections: bool,
    cancel: CancellationToken,
    tmp_paths: Arc<Mutex<Vec<PathBuf>>>,
    new_since: AtomicU64,
}

impl<'a> ZoteroSource<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: &'a dyn ZoteroApi,
        batch_limit: u32,
        collection_keys: Vec<String>,
        download_pdf: bool,
        since: u64,
        recursive_collections: bool,
        cancel: CancellationToken,
        tmp_paths: Arc<Mutex<Vec<PathBuf>>>,
    ) -> Self {
        Self {
            client,
            batch_limit,
            collection_keys,
            download_pdf,
            since,
            recursive_collections,
            cancel,
            tmp_paths,
            new_since: AtomicU64::new(since),
        }
    }

    pub fn new_since(&self) -> u64 {
        self.new_since.load(Ordering::Relaxed)
    }

    /// Fetch the full set of current item keys for removal detection.
    /// Returns the key set plus any non-fatal per-collection errors.
    pub async fn fetch_all_keys(&self) -> Result<(HashSet<String>, Vec<String>)> {
        if self.is_cancelled() {
            return Ok((HashSet::new(), Vec::new()));
        }

        let mut keys = HashSet::new();
        let mut errors = Vec::new();

        if self.collection_keys.is_empty() {
            match self.client.list_top_items(self.batch_limit, 0).await {
                Ok(r) => {
                    for item in r.items {
                        keys.insert(item.data.key);
                    }
                }
                Err(e) => errors.push(format!(
                    "Failed to fetch full Zotero item list for cleanup: {e}"
                )),
            }
        } else {
            for ck in &self.collection_keys {
                let fetch_result = if self.recursive_collections {
                    self.client
                        .get_collection_items(ck, self.batch_limit, 0)
                        .await
                } else {
                    self.client
                        .get_collection_top_items(ck, self.batch_limit, 0)
                        .await
                };
                match fetch_result {
                    Ok(r) => {
                        for item in r.items {
                            keys.insert(item.data.key);
                        }
                    }
                    Err(e) => errors.push(format!(
                        "Failed to fetch full collection {ck} for cleanup: {e}"
                    )),
                }
            }
        }

        Ok((keys, errors))
    }

    async fn find_attachment_path(&self, item: &ZoteroItem) -> Option<String> {
        let children = self.client.get_children(&item.data.key).await.ok()?;

        let pdf_child = children.iter().find(|c| {
            c.data.contentType.as_deref() == Some("application/pdf")
                || c.data
                    .filename
                    .as_deref()
                    .is_some_and(|f| f.to_lowercase().ends_with(".pdf"))
        })?;

        if let Some(ref path_str) = pdf_child.data.path {
            if let Some(storage_path) = parse_storage_path(path_str) {
                let base = zotero_data_dir().join("storage");
                let full_path = base.join(&storage_path);
                if full_path.exists() {
                    return Some(full_path.to_string_lossy().into_owned());
                }
            } else if path_str.starts_with("attachments:") {
                let rel_path = path_str.strip_prefix("attachments:").unwrap_or(path_str);
                let full_path = zotero_data_dir().join("attachments").join(rel_path);
                if full_path.exists() {
                    return Some(full_path.to_string_lossy().into_owned());
                }
            } else {
                let path = PathBuf::from(path_str);
                if path.exists() {
                    return Some(path.to_string_lossy().into_owned());
                }
            }
        }

        if let Some(ref filename) = pdf_child.data.filename {
            let base = zotero_data_dir().join("storage").join(&pdf_child.data.key);
            let full_path = base.join(filename);
            if full_path.exists() {
                return Some(full_path.to_string_lossy().into_owned());
            }
        }

        if self.download_pdf {
            let key = pdf_child.data.key.clone();
            let bytes = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return None,
                result = self.client.download_file(&key) => match result {
                    Ok(b) if !b.is_empty() => b,
                    Ok(_) => return None,
                    Err(e) => {
                        tracing::debug!("Failed to download Zotero PDF {}: {e}", key);
                        return None;
                    }
                },
            };
            let ext = pdf_child
                .data
                .filename
                .as_deref()
                .and_then(|f| {
                    let p = PathBuf::from(f);
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(std::string::ToString::to_string)
                })
                .unwrap_or_else(|| "pdf".to_string());
            let filename = format!("zotero_dl_{}_{}.{ext}", item.data.key, pdf_child.data.key);
            let download_path = std::env::temp_dir().join(&filename);
            match tokio::fs::write(&download_path, &bytes).await {
                Ok(()) => {
                    let result = download_path.to_string_lossy().into_owned();
                    if let Ok(mut tmp) = self.tmp_paths.lock() {
                        tmp.push(download_path);
                    }
                    return Some(result);
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to write downloaded Zotero PDF {}: {}",
                        download_path.display(),
                        e
                    );
                }
            }
        }

        None
    }
}

#[async_trait]
impl LibrarySource for ZoteroSource<'_> {
    type Item = ZoteroItem;

    async fn fetch_items(&self) -> Result<(Vec<Self::Item>, Vec<String>)> {
        if self.is_cancelled() {
            return Ok((Vec::new(), Vec::new()));
        }

        let mut errors = Vec::new();

        if self.collection_keys.is_empty() {
            let response = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Ok((Vec::new(), errors)),
                result = self.client.list_top_items(self.batch_limit, self.since) => result?,
            };
            self.new_since
                .fetch_max(response.last_modified_version, Ordering::Relaxed);
            return Ok((response.items, errors));
        }

        let mut all = Vec::new();
        let mut seen = HashSet::new();
        for ck in &self.collection_keys {
            if self.is_cancelled() {
                errors.push("Sync cancelled by user request.".to_string());
                break;
            }
            let fetch_fut = if self.recursive_collections {
                self.client
                    .get_collection_items(ck, self.batch_limit, self.since)
            } else {
                self.client
                    .get_collection_top_items(ck, self.batch_limit, self.since)
            };
            let fetch_result = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => break,
                result = fetch_fut => result,
            };
            match fetch_result {
                Ok(r) => {
                    self.new_since
                        .fetch_max(r.last_modified_version, Ordering::Relaxed);
                    for item in r.items {
                        if seen.insert(item.data.key.clone()) {
                            all.push(item);
                        }
                    }
                }
                Err(e) => errors.push(format!("Zotero collection {ck} search failed: {e}")),
            }
        }

        Ok((all, errors))
    }

    fn extract_id(item: &Self::Item) -> String {
        item.data.key.clone()
    }

    fn to_paper(item: Self::Item) -> Result<Paper> {
        zotero_item_to_paper(item)
    }

    async fn find_pdf(&self, item: &Self::Item, search_paths: &[PathBuf]) -> Option<PathBuf> {
        if let Some(path) = self.find_attachment_path(item).await {
            return Some(PathBuf::from(path));
        }

        let paths = if search_paths.is_empty() {
            vec![zotero_data_dir().join("storage")]
        } else {
            search_paths.to_vec()
        };
        let terms = util::pdf_search_terms(
            &item.data.title,
            100,
            &item.data.key,
            item.data.DOI.as_deref(),
        );

        crate::sync::find_pdf_with_cancel(&self.cancel, paths, terms).await
    }

    fn should_skip(item: &Self::Item) -> bool {
        item.data.item_type == "attachment" || item.data.item_type == "note"
    }

    async fn update_existing(&self, paper: &Paper, item: &Self::Item) -> Result<Option<Paper>> {
        let data = &item.data;
        let mut updated = paper.clone();
        let mut changed = false;

        let new_title = data.title.trim();
        if (updated.title.is_empty() || updated.title == "Processing\u{2026}")
            && !new_title.is_empty()
        {
            updated.title = new_title.to_string();
            changed = true;
        }

        if updated.authors.is_empty() {
            let new_authors = extract_authors(data);
            if !new_authors.is_empty() {
                updated.authors = new_authors;
                changed = true;
            }
        }

        if updated.published_date.is_none()
            && let Some(new_date) = extract_published_date(data)
        {
            updated.published_date = Some(new_date);
            changed = true;
        }

        if updated.venue.is_none() {
            let new_venue = extract_venue(data);
            if new_venue.is_some() {
                updated.venue = new_venue;
                changed = true;
            }
        }

        if updated.doi.is_none() && data.DOI.is_some() {
            updated.doi.clone_from(&data.DOI);
            changed = true;
        }

        if updated.abstract_text.is_none() && data.abstract_note.is_some() {
            updated.abstract_text.clone_from(&data.abstract_note);
            changed = true;
        }

        if updated.keywords.is_empty() {
            let new_keywords = extract_tags(data);
            if !new_keywords.is_empty() {
                updated.keywords = new_keywords;
                changed = true;
            }
        }

        if updated.urls.is_empty() {
            let new_urls: Vec<String> = data.url.clone().into_iter().collect();
            if !new_urls.is_empty() {
                updated.urls = new_urls;
                changed = true;
            }
        }

        let new_extra = build_zotero_extra(
            &data.key,
            &data.item_type,
            data.DOI.as_deref(),
            data.url.as_deref(),
            zotero_user_extra(data),
        );
        if updated.extra.as_deref() != Some(&new_extra) {
            updated.extra = Some(new_extra);
            changed = true;
        }

        if !changed {
            return Ok(None);
        }

        updated.updated_at = chrono::Utc::now();
        Ok(Some(updated))
    }

    fn allow_metadata_only(_paper: &Paper) -> bool {
        false
    }

    fn on_queue_closed(paper: &mut Paper, path: &PathBuf) {
        paper.status = PaperStatus::Failed;
        paper.error_message =
            Some("Indexing worker unavailable; will retry on next sync".to_string());
        paper.file_path = Some(path.to_string_lossy().into_owned());
    }

    fn is_skip_error(e: &crate::error::PaperedError) -> bool {
        matches!(e, PaperedError::InvalidArgument(_))
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn source_name() -> &'static str {
        "zotero"
    }
}

pub(super) fn zotero_item_to_paper(item: ZoteroItem) -> Result<Paper> {
    let data = item.data;
    let title = data.title.trim();
    if title.is_empty() {
        return Err(PaperedError::invalid_argument(
            "Zotero item has empty title",
        ));
    }

    let extra = build_zotero_extra(
        &data.key,
        &data.item_type,
        data.DOI.as_deref(),
        data.url.as_deref(),
        zotero_user_extra(&data),
    );

    let mut paper = Paper::new(title);
    paper.authors = extract_authors(&data);
    paper.published_date = extract_published_date(&data);
    paper.venue = extract_venue(&data);
    paper.doi.clone_from(&data.DOI);
    paper.abstract_text.clone_from(&data.abstract_note);
    paper.keywords = extract_tags(&data);
    paper.urls = data.url.into_iter().collect();
    paper.extra = Some(extra);
    paper.source = Some(PaperSource::Zotero);
    Ok(paper)
}

pub(super) fn parse_zotero_key(extra: &str) -> Option<String> {
    crate::util::parse_extra_key(extra, "zotero_key")
}

fn parse_storage_path(path_str: &str) -> Option<PathBuf> {
    path_str.strip_prefix("storage:").map(PathBuf::from)
}

fn extract_venue(data: &ZoteroItemData) -> Option<String> {
    ["publicationTitle", "proceedingsTitle", "bookTitle"]
        .iter()
        .find_map(|&k| data.extra.get(k).and_then(|v| v.as_str()).map(String::from))
}

fn zotero_user_extra(data: &ZoteroItemData) -> Option<&str> {
    data.extra.get("extra").and_then(|v| v.as_str())
}

fn extract_authors(data: &ZoteroItemData) -> Vec<String> {
    data.creators
        .iter()
        .filter(|c| c.creator_type == "author")
        .map(|c| {
            if let Some(ref name) = c.name {
                name.clone()
            } else {
                let first = c.first_name.as_deref().unwrap_or("");
                let last = c.last_name.as_deref().unwrap_or("");
                if first.is_empty() && last.is_empty() {
                    String::new()
                } else if first.is_empty() {
                    last.to_string()
                } else {
                    format!("{last}, {first}")
                }
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn extract_published_date(data: &ZoteroItemData) -> Option<String> {
    data.date.as_ref().map(|d| {
        let trimmed = d.trim();
        if trimmed.len() > 4
            && trimmed.chars().take(4).all(|c| c.is_ascii_digit())
            && trimmed.chars().nth(4) == Some('-')
        {
            trimmed.to_string()
        } else if trimmed.len() >= 4 && trimmed.chars().take(4).all(|c| c.is_ascii_digit()) {
            trimmed.chars().take(4).collect()
        } else {
            trimmed.to_string()
        }
    })
}

fn extract_tags(data: &ZoteroItemData) -> Vec<String> {
    data.tags.iter().map(|t| t.tag.clone()).collect()
}
