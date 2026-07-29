use axum::{extract::State, http::StatusCode, response::Json};
use papered::error::ApiError;
use papered::index::export::{ExportFormat, ExportRequest, ExportTarget, perform_export};
use std::sync::Arc;

use super::types::{ApiResult, ERR_FORBIDDEN, ExportRequestBody, bad_request_msg, map_err};
use crate::AppState;

async fn canonicalize_async(
    path: std::path::PathBuf,
) -> Result<std::path::PathBuf, std::io::Error> {
    tokio::task::spawn_blocking(move || std::fs::canonicalize(&path))
        .await
        .map_err(std::io::Error::other)?
}

pub async fn export_data(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExportRequestBody>,
) -> ApiResult<papered::index::export::ExportResult> {
    let target = match req.target.as_str() {
        "database" => ExportTarget::Database,
        "sections" => ExportTarget::Sections,
        "full_papers" => ExportTarget::FullPapers,
        _ => {
            return Err(bad_request_msg(format!(
                "Unknown export target: {}",
                req.target
            )));
        }
    };
    let format = match req.format.as_str() {
        "json" => ExportFormat::Json,
        "csv" => ExportFormat::Csv,
        "markdown" => ExportFormat::Markdown,
        "sqlite" => ExportFormat::Sqlite,
        _ => {
            return Err(bad_request_msg(format!(
                "Unknown export format: {}",
                req.format
            )));
        }
    };

    let dest = std::path::Path::new(&req.destination).to_path_buf();
    let canonical_dest = match canonicalize_async(dest.clone()).await {
        Ok(c) => c,
        Err(_) => {
            let parent = if let Some(p) = dest.parent() {
                canonicalize_async(p.to_path_buf()).await.ok()
            } else {
                None
            };
            let file_name = dest.file_name().and_then(|n| n.to_str());
            match (parent, file_name) {
                (Some(p), Some(n))
                    if !n.contains("..") && !n.contains('/') && !n.contains('\\') =>
                {
                    p.join(n)
                }
                _ => {
                    return Err(bad_request_msg("Invalid destination path".to_string()));
                }
            }
        }
    };
    let data_dir = state.config.read().await.data_dir.clone();
    let allowed_roots = [
        dirs::download_dir(),
        dirs::document_dir(),
        dirs::desktop_dir(),
        Some(std::env::temp_dir()),
        Some(data_dir),
    ];
    let mut within_allowed = false;
    for root in allowed_roots {
        if let Some(r) = root
            && let Ok(cr) = canonicalize_async(r).await
            && canonical_dest.starts_with(&cr)
        {
            within_allowed = true;
            break;
        }
    }
    if !within_allowed {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                ERR_FORBIDDEN,
                "Destination must be within Downloads, Documents, Desktop, temp, or the Papered data directory.",
            )),
        ));
    }
    let request = ExportRequest {
        target,
        format,
        destination: req.destination,
        paper_ids: req.paper_ids,
    };
    let result = perform_export(
        &*state.store,
        &state.config.read().await.db_path(),
        &request,
    )
    .await
    .map_err(map_err)?;
    Ok(Json(result))
}
