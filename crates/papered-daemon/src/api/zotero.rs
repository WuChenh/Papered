use axum::{
    extract::{Path, State},
    response::Json,
};
use std::sync::Arc;

use super::types::{
    ApiResult, ERR_SYNC_BUSY, ERR_ZOTERO_ERROR, SyncCancelResponse, ZoteroCollectionItem,
    ZoteroCollectionListResponse, ZoteroStatusResponse, ZoteroSyncCollectionsRequest,
    ZoteroSyncCollectionsResponse, ZoteroSyncResponse, ZoteroSyncStatusResponse, bad_gateway,
    internal_error, not_found, service_unavailable, sync_cancel_message,
};
use crate::AppState;
use crate::state::{SyncJobStatus, ZoteroSyncRequest};
use crate::sync_runner::auto_sync_paused;
use papered::zotero::ZoteroApi;
use std::sync::atomic::Ordering;

pub async fn zotero_status(State(state): State<Arc<AppState>>) -> Json<ZoteroStatusResponse> {
    let client = state
        .zotero_client
        .as_ref()
        .expect("zotero client always initialized");
    let consecutive_failures = state.zotero_sync_failures.load(Ordering::Relaxed);
    let auto_sync_paused = auto_sync_paused(&state.zotero_sync_failures);
    let zotero_config = state.config.read().await.zotero_sync.clone();
    let collection_keys = zotero_config.collection_keys;
    let recursive_collections = zotero_config.recursive_collections;
    match client.status().await {
        Ok(s) => Json(ZoteroStatusResponse {
            available: true,
            base_url: Some(client.base_url().to_string()),
            user_id: Some(s.userID),
            username: Some(s.username),
            auto_sync_paused,
            consecutive_failures,
            collection_keys,
            recursive_collections,
        }),
        Err(e) => {
            tracing::debug!("Zotero /keys/current check failed: {}", e);
            // Fallback: /keys/current may return 403 on some Zotero configs,
            // but /users/0/collections works via the local API wildcard.
            // If collections are reachable, Zotero is running.
            match client.list_collections().await {
                Ok(_) => Json(ZoteroStatusResponse {
                    available: true,
                    base_url: Some(client.base_url().to_string()),
                    user_id: None,
                    username: None,
                    auto_sync_paused,
                    consecutive_failures,
                    collection_keys,
                    recursive_collections,
                }),
                Err(e2) => {
                    tracing::debug!("Zotero fallback collections check also failed: {}", e2);
                    Json(ZoteroStatusResponse {
                        available: false,
                        base_url: Some(client.base_url().to_string()),
                        user_id: None,
                        username: None,
                        auto_sync_paused,
                        consecutive_failures,
                        collection_keys,
                        recursive_collections,
                    })
                }
            }
        }
    }
}

pub async fn zotero_sync(State(state): State<Arc<AppState>>) -> ApiResult<ZoteroSyncResponse> {
    // `zotero_sync.enabled` gates only the automatic background scheduler
    // (`spawn_zotero_sync`). A manual sync is an explicit user request and
    // runs regardless — mirroring the Lattice manual sync endpoint.
    let sync_id = format!("sync_{}", uuid::Uuid::new_v4());
    {
        let mut jobs = state.zotero_sync_jobs.write().await;
        jobs.insert(sync_id.clone(), crate::state::SyncJob::new(sync_id.clone()));
    }

    // Queue the sync request through the unified channel so it is serialized
    // with automatic syncs and never runs concurrently.
    let (tx, rx) = tokio::sync::oneshot::channel();
    if let Err(e) = state
        .zotero_sync_tx
        .send(ZoteroSyncRequest::Manual {
            job_id: sync_id.clone(),
            response_tx: tx,
        })
        .await
    {
        tracing::warn!("Failed to queue Zotero sync request: {e}");
        {
            let mut jobs = state.zotero_sync_jobs.write().await;
            jobs.remove(&sync_id);
        }
        return Err(service_unavailable(
            ERR_SYNC_BUSY,
            "Zotero sync worker is not available. Please try again.".to_string(),
        ));
    }

    // Spawn a task that waits for completion so we can update the job map even
    // if the client never polls for status.
    let state_clone = state.clone();
    let sync_id_clone = sync_id.clone();
    tokio::spawn(async move {
        if rx.await.is_err() {
            tracing::warn!("Zotero sync worker dropped response for {sync_id_clone}");
            if let Some(job) = state_clone
                .zotero_sync_jobs
                .write()
                .await
                .get_mut(&sync_id_clone)
            {
                job.set_status(SyncJobStatus::Cancelled);
            }
        }
    });

    Ok(Json(ZoteroSyncResponse {
        sync_id: sync_id.clone(),
        status: "pending".to_string(),
    }))
}

pub async fn zotero_sync_status(
    State(state): State<Arc<AppState>>,
    Path(sync_id): Path<String>,
) -> ApiResult<ZoteroSyncStatusResponse> {
    let jobs = state.zotero_sync_jobs.read().await;
    let Some(job) = jobs.get(&sync_id) else {
        return Err(not_found(format!("Sync job '{sync_id}' not found")));
    };

    let status = job.status.to_string();

    Ok(Json(ZoteroSyncStatusResponse {
        sync_id: job.id.clone(),
        status: status.to_string(),
        report: job.report.clone(),
    }))
}

pub async fn zotero_sync_cancel(State(state): State<Arc<AppState>>) -> Json<SyncCancelResponse> {
    state.zotero_cancel.lock().await.cancel();
    Json(SyncCancelResponse {
        cancelled: true,
        message: sync_cancel_message("Zotero"),
    })
}

pub async fn zotero_collections(
    State(state): State<Arc<AppState>>,
) -> ApiResult<ZoteroCollectionListResponse> {
    let client = state
        .zotero_client
        .as_ref()
        .expect("zotero client always initialized");
    let collections = client.list_collections().await.map_err(|e| {
        bad_gateway(
            ERR_ZOTERO_ERROR,
            format!("Failed to list Zotero collections: {e}"),
        )
    })?;
    let items: Vec<ZoteroCollectionItem> = collections
        .into_iter()
        .map(|c| ZoteroCollectionItem {
            key: c.key,
            name: c.data.name,
            parent_collection: c.data.parentCollection,
        })
        .collect();
    Ok(Json(ZoteroCollectionListResponse { collections: items }))
}

pub async fn zotero_sync_collections(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ZoteroSyncCollectionsRequest>,
) -> ApiResult<ZoteroSyncCollectionsResponse> {
    let _guard = state.config_write_lock.lock().await;
    let old_config = state.config.read().await.clone();
    let mut new_config = old_config.clone();
    new_config.zotero_sync.collection_keys = req.collection_keys.clone();
    new_config.zotero_sync.recursive_collections = req.recursive_collections;
    new_config
        .save()
        .map_err(|e| internal_error(e.to_string()))?;
    state.apply_config_update(&new_config, &old_config).await;
    Ok(Json(ZoteroSyncCollectionsResponse {
        collection_keys: req.collection_keys,
        recursive_collections: req.recursive_collections,
    }))
}
