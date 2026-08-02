use axum::{
    extract::{Query, State},
    response::Json,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::types::{
    ApiResult, ERR_LATTICE_ERROR, LatticeCollectionItem, LatticeCollectionListResponse,
    LatticeImportRequest, LatticeImportResponse, LatticeSearchQuery, LatticeStatusResponse,
    LatticeSyncCollectionsRequest, LatticeSyncCollectionsResponse, SyncCancelResponse, bad_gateway,
    bad_request_msg, internal_error, map_err, sync_cancel_message, unprocessable_entity,
};
use crate::AppState;
use crate::sync_runner::{auto_sync_paused, sync_run_failure_reason, update_sync_circuit_breaker};
use std::sync::atomic::Ordering;

pub async fn lattice_status(
    State(state): State<Arc<AppState>>,
) -> ApiResult<LatticeStatusResponse> {
    let client = state.lattice_client.as_ref().ok_or_else(|| {
        bad_gateway(
            ERR_LATTICE_ERROR,
            "Lattice client not available".to_string(),
        )
    })?;
    let consecutive_failures = state.lattice_sync_failures.load(Ordering::Relaxed);
    let auto_sync_paused = auto_sync_paused(&state.lattice_sync_failures);
    let collections = state.config.read().await.lattice_sync.collections.clone();
    match client.status().await {
        Ok(s) => Ok(Json(LatticeStatusResponse {
            available: true,
            api_version: Some(s.api_version),
            app_version: Some(s.app_version),
            capabilities: Some(s.capabilities),
            base_url: Some(client.base_url().to_string()),
            auto_sync_paused,
            consecutive_failures,
            collections,
        })),
        Err(e) => {
            tracing::debug!("Lattice status check failed: {}", e);
            Ok(Json(LatticeStatusResponse {
                available: false,
                api_version: None,
                app_version: None,
                capabilities: None,
                base_url: Some(client.base_url().to_string()),
                auto_sync_paused,
                consecutive_failures,
                collections,
            }))
        }
    }
}

pub async fn lattice_collections(
    State(state): State<Arc<AppState>>,
) -> ApiResult<LatticeCollectionListResponse> {
    let client = state.lattice_client.as_ref().ok_or_else(|| {
        bad_gateway(
            ERR_LATTICE_ERROR,
            "Lattice client not available".to_string(),
        )
    })?;
    let collections = client.list_collections().await.map_err(|e| {
        bad_gateway(
            ERR_LATTICE_ERROR,
            format!("Lattice collections failed: {e}"),
        )
    })?;
    Ok(Json(LatticeCollectionListResponse {
        collections: collections
            .into_iter()
            .map(|c| LatticeCollectionItem {
                id: c.id,
                name: c.name,
                path: c.path,
                depth: c.depth,
            })
            .collect(),
    }))
}

pub async fn lattice_search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<LatticeSearchQuery>,
) -> ApiResult<papered::lattice::LatticeSearchResponse> {
    let client = state.lattice_client.as_ref().ok_or_else(|| {
        bad_gateway(
            ERR_LATTICE_ERROR,
            "Lattice client not available".to_string(),
        )
    })?;
    let results = client
        .search(&params.q, params.limit)
        .await
        .map_err(|e| bad_gateway(ERR_LATTICE_ERROR, format!("Lattice search failed: {e}")))?;
    Ok(Json(results))
}

pub async fn lattice_sync(
    State(state): State<Arc<AppState>>,
) -> ApiResult<papered::sync::SyncReport> {
    let config = state.config.read().await;
    // Reset-on-start so `lattice_sync_cancel` can stop this run, mirroring the
    // Zotero sync path.
    let cancel = {
        let mut guard = state.lattice_cancel.lock().await;
        *guard = CancellationToken::new();
        guard.clone()
    };
    let syncer = papered::lattice::syncer::LatticeSyncer::with_collections(
        state.store.clone(),
        state.import_tx.clone(),
        config.lattice_sync.base.batch_limit,
        config.lattice_sync.base.pdf_search_paths.clone(),
        config.lattice_sync.collections.clone(),
        cancel.clone(),
    )
    .map_err(|e| {
        update_sync_circuit_breaker(
            &state.lattice_sync_failures,
            "Lattice",
            Some(&e.to_string()),
        );
        bad_gateway(
            ERR_LATTICE_ERROR,
            format!("Failed to create Lattice syncer: {e}"),
        )
    })?;
    drop(config);
    let report = syncer.sync().await;
    // A cancelled cycle is neither success nor failure — leave the counter.
    if !cancel.is_cancelled() {
        update_sync_circuit_breaker(
            &state.lattice_sync_failures,
            "Lattice",
            sync_run_failure_reason(&report).as_deref(),
        );
    }
    Ok(Json(report))
}

pub async fn lattice_sync_cancel(State(state): State<Arc<AppState>>) -> Json<SyncCancelResponse> {
    state.lattice_cancel.lock().await.cancel();
    Json(SyncCancelResponse {
        cancelled: true,
        message: sync_cancel_message("Lattice"),
    })
}

/// Save the selected Lattice collection names to the daemon config, mirroring
/// the Zotero sync-scope endpoint. An empty list means "sync all collections".
pub async fn lattice_sync_collections(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LatticeSyncCollectionsRequest>,
) -> ApiResult<LatticeSyncCollectionsResponse> {
    state
        .update_config_saved(|config| {
            config.lattice_sync.collections = req.collection_names.clone();
        })
        .await
        .map_err(|e| internal_error(e.to_string()))?;
    Ok(Json(LatticeSyncCollectionsResponse {
        collection_names: req.collection_names,
    }))
}

pub async fn import_from_lattice(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LatticeImportRequest>,
) -> ApiResult<LatticeImportResponse> {
    let client = state.lattice_client.as_ref().ok_or_else(|| {
        bad_gateway(
            ERR_LATTICE_ERROR,
            "Lattice client not available".to_string(),
        )
    })?;
    let lattice_paper = client.get_paper(&req.paper_id).await.map_err(|e| {
        bad_gateway(
            ERR_LATTICE_ERROR,
            format!("Failed to fetch paper from Lattice: {e}"),
        )
    })?;
    if lattice_paper.title.trim().is_empty() {
        return Err(unprocessable_entity(
            "Lattice paper has empty title".to_string(),
        ));
    }
    let csl_json = lattice_paper
        .csl_item
        .as_ref()
        .map(std::string::ToString::to_string);
    let extra = papered::lattice::build_lattice_extra(&lattice_paper, csl_json);
    let mut paper = papered::paper::Paper::new(lattice_paper.title.clone());
    paper.source = Some(papered::paper::PaperSource::Lattice);
    paper.authors = lattice_paper.authors.clone();
    paper.published_date = lattice_paper.year.map(|y| y.to_string());
    paper.venue = lattice_paper.journal.clone();
    paper.doi = lattice_paper.doi.clone();
    paper.keywords = vec![lattice_paper.citekey.clone()];
    paper.extra = Some(extra);
    paper.abstract_text = lattice_paper.abstract_text.clone();
    let mut indexing_queued = false;
    if let Some(pdf_path) = req.file_path.as_ref().filter(|p| !p.trim().is_empty()) {
        let normalized = papered::util::paths::normalize_input_path(pdf_path);
        let path = normalized.as_path();
        if !tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Err(bad_request_msg(format!(
                "File does not exist: {}",
                path.display()
            )));
        }
        let is_pdf = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
        if !is_pdf {
            return Err(bad_request_msg(format!(
                "File must be a PDF: {}",
                path.display()
            )));
        }
        crate::state::queue_paper_for_indexing(
            &*state.store,
            &state.import_tx,
            &mut paper,
            Some(normalized.to_string_lossy().into_owned()),
            false,
            false,
        )
        .await?;
        indexing_queued = true;
    } else {
        state.store.insert_paper(&paper).await.map_err(map_err)?;
    }
    let source = format!(
        "Lattice (id={}, citekey={})",
        lattice_paper.id, lattice_paper.citekey
    );
    Ok(Json(LatticeImportResponse {
        paper,
        source,
        indexing_queued,
    }))
}
