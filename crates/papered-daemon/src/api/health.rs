use axum::{extract::State, response::Json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use self::image_quality::{prune_stale_figures_for_paper, scan_paper_images};
use super::types::{
    ApiResult, BadImageRef, CleanupResponse, DataQualityResponse, DuplicateGroupsResponse,
    DuplicatePaperSummary, HealthResponse, ImageQualityResponse, KbHealthResponse,
    MissingFigureRef, PaperQualityIssue, PaperRefResponse, ServiceHealthResponse, map_err,
};

mod image_quality;

use crate::AppState;
use papered::PaperPager;
use papered::paper::PaperStatus;
use papered::util::paths::is_safe_paper_id;

pub async fn health() -> Json<ServiceHealthResponse> {
    Json(ServiceHealthResponse {
        status: "ok".to_string(),
        service: "papered-daemon".to_string(),
    })
}

pub async fn v1_health(State(state): State<Arc<AppState>>) -> ApiResult<HealthResponse> {
    let model_ready = state.embedding_model_ready.load(Ordering::Relaxed);
    let status = if model_ready { "ok" } else { "degraded" };
    let paper_count = state.store.paper_count().await.map_err(map_err)?;
    let vector_count = state.store.count().await.map_err(map_err)?;
    Ok(Json(
        HealthResponse::from_state(&state, status, paper_count, vector_count).await,
    ))
}

pub async fn kb_health(State(state): State<Arc<AppState>>) -> ApiResult<KbHealthResponse> {
    let data_dir = state.config.read().await.data_dir.clone();
    let to_ref = |v: Vec<papered::store::vector::PaperRef>| {
        v.into_iter()
            .map(|p| PaperRefResponse {
                id: p.id,
                title: p.title,
            })
            .collect()
    };
    let papers_without_vectors = to_ref(
        state
            .store
            .papers_without_vectors()
            .await
            .map_err(map_err)?,
    );
    let orphaned_vector_paper_ids = state
        .store
        .orphaned_vector_paper_ids()
        .await
        .map_err(map_err)?;
    let papers_with_missing_files = to_ref(
        state
            .store
            .papers_with_missing_files()
            .await
            .map_err(map_err)?,
    );
    let raw_missing = state
        .store
        .figures_with_missing_images(&data_dir)
        .await
        .map_err(map_err)?;
    let figures_with_missing_images: Vec<MissingFigureRef> = raw_missing
        .into_iter()
        .map(|f| MissingFigureRef {
            paper_id: f.paper_id,
            figure_id: f.figure_id,
            paper_title: f.paper_title,
        })
        .collect();
    let orphaned_directories = state
        .store
        .orphaned_data_directories(&data_dir)
        .await
        .map_err(map_err)?;
    Ok(Json(KbHealthResponse {
        papers_without_vectors,
        orphaned_vector_paper_ids,
        papers_with_missing_files,
        figures_with_missing_images,
        orphaned_directories,
    }))
}

pub async fn find_duplicate_groups(
    State(state): State<Arc<AppState>>,
) -> ApiResult<DuplicateGroupsResponse> {
    let papers = state.store.duplicate_scan_papers().await.map_err(map_err)?;

    let mut groups: std::collections::HashMap<String, Vec<DuplicatePaperSummary>> =
        std::collections::HashMap::new();

    for paper in papers {
        let key = if let Some(ref hash) = paper.file_hash
            && !hash.is_empty()
        {
            format!("hash:{hash}")
        } else {
            let normalized_title = paper.title.to_lowercase().trim().to_string();
            let authors_key = {
                let mut authors = paper.authors.clone();
                authors.sort();
                authors.join(",")
            };
            let date_part = paper.published_date.clone().unwrap_or_default();
            format!("meta:{normalized_title}|{authors_key}|{date_part}")
        };

        groups.entry(key).or_default().push(DuplicatePaperSummary {
            id: paper.id,
            title: paper.title,
            authors: paper.authors,
            published_date: paper.published_date,
            file_hash: paper.file_hash,
            status: paper.status,
            updated_at: paper.updated_at.to_rfc3339(),
        });
    }

    let mut result: Vec<Vec<DuplicatePaperSummary>> =
        groups.into_values().filter(|g| g.len() > 1).collect();
    result.sort_by_key(|b| std::cmp::Reverse(b.len()));

    Ok(Json(DuplicateGroupsResponse { groups: result }))
}

pub async fn cleanup_health(State(state): State<Arc<AppState>>) -> ApiResult<CleanupResponse> {
    let mut removed = Vec::new();
    let mut metadata_only: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Collect stale ids while paging, then batch-delete once. Deleting inside
    // the paging loop both shifts OFFSET pagination (rows get skipped) and
    // commits the FTS cascade once per paper — a batched DELETE IN (...) folds
    // N papers into one tantivy commit.
    let mut stale_ids: Vec<String> = Vec::new();
    let mut pager = PaperPager::new(&state.store, 1000);
    while let Some(batch) = pager.next_batch().await.map_err(map_err)? {
        for paper in &batch {
            if paper.file_path.is_none() {
                metadata_only.insert(paper.id.clone());
            }
            if paper.status != PaperStatus::Indexed {
                stale_ids.push(paper.id.clone());
            }
        }
    }
    let stale_ids: Vec<&str> = stale_ids.iter().map(String::as_str).collect();
    if let Err(e) = state.store.delete_papers(&stale_ids).await {
        tracing::warn!("Failed to delete stale papers in batch: {}", e);
    } else {
        removed.extend(stale_ids.iter().map(|s| s.to_string()));
    }
    let without_vectors = state
        .store
        .papers_without_vectors()
        .await
        .map_err(map_err)?;
    let removed_set: std::collections::HashSet<String> = removed.iter().cloned().collect();
    let stray_ids: Vec<&str> = without_vectors
        .iter()
        .filter(|pr| !removed_set.contains(&pr.id) && !metadata_only.contains(&pr.id))
        .map(|pr| pr.id.as_str())
        .collect();
    if let Err(e) = state.store.delete_papers(&stray_ids).await {
        tracing::warn!("Failed to delete papers without vectors in batch: {}", e);
    } else {
        removed.extend(stray_ids.iter().map(|s| s.to_string()));
    }
    let orphan_ids = state
        .store
        .orphaned_vector_paper_ids()
        .await
        .map_err(map_err)?;
    let mut removed_vectors = Vec::new();
    for id in &orphan_ids {
        if let Err(e) = state.store.delete_by_paper(id).await {
            tracing::warn!("Failed to delete orphan vectors for {}: {}", id, e);
        } else {
            removed_vectors.push(id.clone());
        }
    }
    let data_dir = state.config.read().await.data_dir.clone();
    let orphaned = state
        .store
        .orphaned_data_directories(&data_dir)
        .await
        .map_err(map_err)?;
    let mut removed_dirs = Vec::new();
    for id in &orphaned {
        if !is_safe_paper_id(id) {
            tracing::warn!(paper_id = %id, "Skipping orphaned directory cleanup for unsafe id");
            continue;
        }
        let dir = data_dir.join("papers").join(id);
        match tokio::task::spawn_blocking({
            let dir = dir.clone();
            move || std::fs::remove_dir_all(&dir)
        })
        .await
        {
            Ok(Ok(())) => {
                tracing::info!("Removed orphaned directory: {}", dir.display());
                removed_dirs.push(id.clone());
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "Failed to remove orphaned directory {}: {}",
                    dir.display(),
                    e
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to remove orphaned directory {}: {}",
                    dir.display(),
                    e
                );
            }
        }
        for ext in ["jpg", "png"] {
            let cover = data_dir.join("covers").join(format!("{id}.{ext}"));
            if tokio::fs::try_exists(&cover).await.unwrap_or(false)
                && let Err(e) = tokio::fs::remove_file(&cover).await
            {
                tracing::warn!("Failed to remove orphaned cover {}: {}", cover.display(), e);
            }
        }
    }
    // Clean up figures whose image files no longer exist on disk.
    let missing_figures = state
        .store
        .figures_with_missing_images(&data_dir)
        .await
        .map_err(map_err)?;
    let mut affected_papers = std::collections::HashSet::new();
    for f in missing_figures {
        affected_papers.insert(f.paper_id);
    }
    let mut removed_figures = 0usize;
    for paper_id in affected_papers {
        removed_figures += prune_stale_figures_for_paper(&state.store, &data_dir, &paper_id).await;
    }

    tracing::info!(
        "Health cleanup removed {} papers, {} orphan vectors, {} orphaned directories, {} stale figures",
        removed.len(),
        removed_vectors.len(),
        removed_dirs.len(),
        removed_figures
    );
    Ok(Json(CleanupResponse {
        removed_papers: removed,
        removed_orphan_vectors: removed_vectors,
        removed_orphan_directories: removed_dirs,
        removed_figures,
    }))
}

pub async fn data_quality(State(state): State<Arc<AppState>>) -> ApiResult<DataQualityResponse> {
    let config = state.config.read().await;
    let pdf_config = &config.pdf_extraction;

    let mut papers_with_issues = Vec::new();
    let mut issue_summary = std::collections::HashMap::new();
    let mut total_checked = 0usize;

    let mut pager = PaperPager::new(&state.store, 500);
    while let Some((batch, sections_batch)) =
        pager.next_batch_with_sections().await.map_err(map_err)?
    {
        for (paper, sections) in batch.iter().zip(sections_batch) {
            total_checked += 1;
            let mut issues = Vec::new();

            // === Metadata integrity ===
            let title_trimmed = paper.title.trim();
            if title_trimmed.is_empty() || title_trimmed == "Untitled" {
                issues.push("empty_title".to_string());
            } else {
                let title_chars = title_trimmed.chars().count();
                if title_chars < pdf_config.min_title_chars {
                    issues.push("short_title".to_string());
                }
                if title_chars > 300 {
                    issues.push("oversized_title".to_string());
                }
                if title_trimmed
                    .chars()
                    .all(|c| c.is_numeric() || c.is_whitespace() || c == '.' || c == '-')
                {
                    issues.push("numeric_title".to_string());
                }
            }

            if paper.authors.is_empty() {
                issues.push("empty_authors".to_string());
            } else {
                for author in &paper.authors {
                    if author.contains('@') && author.contains('.') {
                        issues.push("email_in_authors".to_string());
                        break;
                    }
                }
            }

            if paper.status == PaperStatus::Failed {
                issues.push("indexing_failed".to_string());
            }

            if let Some(ref err) = paper.error_message
                && !err.is_empty()
            {
                issues.push("has_error".to_string());
            }

            // === Section content quality ===
            let section_list = sections.sections;
            if section_list.is_empty() {
                issues.push("no_sections".to_string());
            } else {
                let total_chars: usize = section_list.iter().map(|s| s.content.len()).sum();
                if total_chars < 500 {
                    issues.push("low_content".to_string());
                }

                let empty_count = section_list
                    .iter()
                    .filter(|s| s.content.trim().is_empty())
                    .count();
                if empty_count > 0 {
                    issues.push("empty_sections".to_string());
                }

                let has_abstract = section_list.iter().any(|s| {
                    s.section_type == papered::paper::section::SectionType::Abstract
                        && !s.content.trim().is_empty()
                });
                if !has_abstract {
                    issues.push("missing_abstract".to_string());
                }

                if let Some(abstract_section) = section_list
                    .iter()
                    .find(|s| s.section_type == papered::paper::section::SectionType::Abstract)
                    && abstract_section.content.trim().len() < 200
                {
                    issues.push("short_abstract".to_string());
                }
            }

            if !issues.is_empty() {
                for issue in &issues {
                    *issue_summary.entry(issue.clone()).or_insert(0) += 1;
                }
                papers_with_issues.push(PaperQualityIssue {
                    paper_id: paper.id.clone(),
                    title: paper.title.clone(),
                    issues,
                });
            }
        }
    }

    Ok(Json(DataQualityResponse {
        total_checked,
        papers_with_issues,
        issue_summary,
    }))
}

pub async fn image_quality(State(state): State<Arc<AppState>>) -> ApiResult<ImageQualityResponse> {
    let config = state.config.read().await.clone();
    let data_dir = config.data_dir.clone();

    let mut bad_images = Vec::new();
    let mut total_scanned = 0usize;

    let mut pager = PaperPager::new(&state.store, 500);
    while let Some(batch) = pager.next_batch().await.map_err(map_err)? {
        for paper in &batch {
            if !is_safe_paper_id(&paper.id) {
                tracing::warn!(paper_id = %paper.id, "Skipping paper with unsafe id");
                continue;
            }
            for img in scan_paper_images(&data_dir, &paper.id, &config).await {
                total_scanned += 1;
                if let Some(reason) = img.reason {
                    bad_images.push(BadImageRef {
                        paper_id: paper.id.clone(),
                        paper_title: paper.title.clone(),
                        filename: img.filename,
                        reason,
                    });
                }
            }
        }
    }

    Ok(Json(ImageQualityResponse {
        total_scanned,
        bad_images,
    }))
}

pub async fn cleanup_images(
    State(state): State<Arc<AppState>>,
) -> ApiResult<HashMap<String, usize>> {
    let config = state.config.read().await.clone();
    let data_dir = config.data_dir.clone();

    let mut removed = 0usize;

    let mut pager = PaperPager::new(&state.store, 500);
    while let Some(batch) = pager.next_batch().await.map_err(map_err)? {
        for paper in &batch {
            if !is_safe_paper_id(&paper.id) {
                continue;
            }
            let mut any_removed = false;
            for img in scan_paper_images(&data_dir, &paper.id, &config).await {
                if img.reason.is_some() {
                    if let Err(e) = tokio::fs::remove_file(&img.path).await {
                        tracing::warn!("Failed to remove bad image {}: {}", img.path.display(), e);
                    } else {
                        removed += 1;
                        any_removed = true;
                    }
                }
            }

            // Sync figure records: remove database entries for deleted images.
            if any_removed {
                prune_stale_figures_for_paper(&state.store, &data_dir, &paper.id).await;
            }
        }
    }

    tracing::info!("cleanup_images removed {} bad images", removed);
    let mut resp = HashMap::new();
    resp.insert("removed".to_string(), removed);
    Ok(Json(resp))
}

pub async fn optimize_store(
    State(state): State<Arc<AppState>>,
) -> ApiResult<HashMap<String, String>> {
    state.store.optimize().await.map_err(map_err)?;
    let mut resp = HashMap::new();
    resp.insert("status".to_string(), "ok".to_string());
    resp.insert("message".to_string(), "Store optimized".to_string());
    Ok(Json(resp))
}

/// `POST /api/v1/health/regenerate-covers` — regenerate covers and thumbnails
/// for all papers that have a PDF file. Skips papers without a file or whose
/// file is missing from disk.
pub async fn regenerate_covers(
    State(state): State<Arc<AppState>>,
) -> ApiResult<HashMap<String, String>> {
    let mut total = 0usize;
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let data_dir = state.config.read().await.data_dir.clone();

    let mut pager = PaperPager::new(&state.store, 100);
    while let Some(batch) = pager.next_batch().await.map_err(map_err)? {
        for paper in &batch {
            let file_path = match &paper.file_path {
                Some(p) => p.clone(),
                None => continue,
            };
            let pdf_path = std::path::PathBuf::from(&file_path);
            if !pdf_path.exists() {
                continue;
            }
            total += 1;
            let paper_id = paper.id.clone();
            let data_dir = data_dir.clone();
            let result = tokio::task::spawn_blocking(move || {
                papered::cover::generate_cover(&pdf_path, &paper_id, &data_dir)
            })
            .await;

            match result {
                Ok(Ok(Some(cover_rel))) => {
                    if let Err(e) = state.store.update_paper_cover(&paper.id, &cover_rel).await {
                        tracing::warn!(
                            paper_id = %paper.id,
                            "Failed to persist cover_path: {e}"
                        );
                    }
                    succeeded += 1;
                }
                Ok(Ok(None)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(
                        paper_id = %paper.id,
                        "Cover generation failed: {e}"
                    );
                    failed += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        paper_id = %paper.id,
                        "Cover generation panicked: {e}"
                    );
                    failed += 1;
                }
            }
        }
    }

    let mut resp = HashMap::new();
    resp.insert("total".to_string(), total.to_string());
    resp.insert("succeeded".to_string(), succeeded.to_string());
    resp.insert("failed".to_string(), failed.to_string());
    if succeeded > 0 || total > 0 {
        tracing::info!("Cover regeneration: {total} total, {succeeded} succeeded, {failed} failed");
    }
    Ok(Json(resp))
}

/// Request body for `POST /api/v1/health/optimize-images`.
#[derive(Debug, serde::Deserialize)]
pub struct OptimizeImagesRequest {
    /// Estimate sizes without touching files or the database. The UI uses
    /// this to show a preview before the destructive re-encode.
    #[serde(default)]
    pub dry_run: bool,
}

/// `POST /api/v1/health/optimize-images` — re-optimize all figure images
/// using the current `pdf_extraction` config (format, quality, max_long_side).
/// Pass `{"dry_run": true}` to preview the savings without modifying anything.
/// Returns storage savings statistics.
pub async fn optimize_images(
    State(state): State<Arc<AppState>>,
    body: Option<Json<OptimizeImagesRequest>>,
) -> ApiResult<serde_json::Value> {
    let dry_run = body.map(|Json(b)| b.dry_run).unwrap_or(false);
    let config = state.config.read().await;
    let data_dir = config.data_dir.clone();
    let pdf_config = config.pdf_extraction.clone();
    drop(config);

    // The `extract_images = false` guard lives in `optimize_existing_images`
    // (single checkpoint); it surfaces here as a 400 config error via map_err.
    let store = state.store.clone();
    let stats =
        papered::util::image::optimize_existing_images(&data_dir, store, &pdf_config, dry_run)
            .await
            .map_err(map_err)?;

    let saved_bytes = stats.bytes_before.saturating_sub(stats.bytes_after);
    let resp = serde_json::json!({
        "status": "ok",
        "dry_run": dry_run,
        "papers_scanned": stats.papers_scanned,
        "images_processed": stats.images_processed,
        "images_skipped": stats.images_skipped,
        "images_failed": stats.images_failed,
        "bytes_before": stats.bytes_before,
        "bytes_after": stats.bytes_after,
        "bytes_saved": saved_bytes,
        "size_before": papered::util::fs::human_readable_size(stats.bytes_before),
        "size_after": papered::util::fs::human_readable_size(stats.bytes_after),
        "saved": papered::util::fs::human_readable_size(saved_bytes),
    });
    tracing::info!(
        "Image optimization {}: {} processed, {} saved",
        if dry_run { "dry-run" } else { "complete" },
        stats.images_processed,
        papered::util::fs::human_readable_size(saved_bytes)
    );
    Ok(Json(resp))
}
