use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Json, Response},
};
use papered::StrLabel;
use papered::error::ApiError;
use papered::paper::{Paper, PaperSource, PaperStatus};
use papered::store::vector::VectorStore;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio_stream::StreamExt;

use super::types::{
    AddPaperRequest, ApiResult, ApiStatusResult, BatchAddPaperResult, BatchAddPapersRequest,
    BatchAddPapersResponse, BatchError, BatchPaperIdsRequest, BatchResult, BatchStatusRequest,
    ChunkDetailResponse, ERR_QUEUE_CLOSED, ImportResponse, ListPapersQuery, ListPapersResponse,
    MAX_LIMIT, RegenerateCoverResponse, StatsResponse, UpdatePaperRequest, bad_request_msg,
    check_batch_size, conflict, internal_error, map_err, not_found, payload_too_large,
    require_paper, service_unavailable, validate_paper_id,
};
use crate::AppState;
use papered::util::file_limits::{PdfBackend, check_size_bytes};
use papered::util::fs::{dir_size_mb, path_size};
use papered::util::image::image_content_type;
use papered::util::paths::{
    is_safe_paper_id, normalize_input_path, resolve_figure_paths, safe_join,
};

const SUPPORTED_FILE_TYPES_MSG: &str = "pdf, md, txt, tex, png, jpg, jpeg, webp, gif, bmp, docx";

fn unsupported_file_type_error(path: &std::path::Path) -> String {
    format!(
        "Unsupported file type: {}. Supported: {}",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("unknown"),
        SUPPORTED_FILE_TYPES_MSG
    )
}

fn batch_add_paper_error(file_path: &str, error: impl Into<String>) -> BatchAddPaperResult {
    BatchAddPaperResult {
        id: String::new(),
        title: String::new(),
        file_path: file_path.to_string(),
        status: "error".to_string(),
        error: Some(error.into()),
    }
}

/// Validate that `path` points to an existing, supported file within the
/// extraction backend's size limit. Returns the detected document source.
pub(crate) async fn validate_addable_file(
    path: &std::path::Path,
    backend: PdfBackend,
) -> Result<papered::paper::source::DocumentSource, (StatusCode, Json<ApiError>)> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(bad_request_msg(format!(
                "File does not exist: {}",
                path.display()
            )));
        }
        Err(e) => {
            return Err(bad_request_msg(format!(
                "Failed to read file metadata: {e}"
            )));
        }
    };
    if !metadata.is_file() {
        return Err(bad_request_msg(format!(
            "Path is not a file: {}",
            path.display()
        )));
    }
    let Some(source) = papered::paper::source::DocumentSource::from_path(path) else {
        return Err(bad_request_msg(unsupported_file_type_error(path)));
    };
    check_size_bytes(metadata.len(), backend).map_err(map_err)?;
    Ok(source)
}

pub async fn list_papers(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListPapersQuery>,
) -> ApiResult<ListPapersResponse> {
    let limit = params.limit.min(MAX_LIMIT);
    let offset = params.offset;
    let sort_desc = params.sort_order.as_deref() != Some("asc");
    let sort_by = params.sort_by.as_deref();
    let entity_filter = papered::paper::EntityFilter {
        species: params.species,
        gene: params.gene,
        technique: params.technique,
        pathway: params.pathway,
    };
    let (papers, total) = state
        .store
        .list_papers_filtered(
            params.status.as_deref(),
            params.paper_type.as_deref(),
            params.keyword.as_deref(),
            &entity_filter,
            sort_by,
            sort_desc,
            limit,
            offset,
        )
        .await
        .map_err(map_err)?;
    // One batch query for the whole page — the list UI shows a rating and a
    // comment count per row, so per-row lookups would be an N+1.
    let ids: Vec<&str> = papers.iter().map(|p| p.id.as_str()).collect();
    let summaries = state
        .store
        .annotation_summaries(&ids)
        .await
        .map_err(map_err)?;
    let papers = papers
        .into_iter()
        .map(|paper| {
            let summary = summaries.get(&paper.id).copied().unwrap_or_default();
            super::types::PaperListItem {
                paper,
                rating: summary.rating,
                comment_count: summary.comment_count,
            }
        })
        .collect();
    Ok(Json(ListPapersResponse {
        papers,
        total,
        has_more: (offset + limit) < total,
    }))
}

pub async fn get_paper(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiResult<papered::paper::Paper> {
    let mut paper = require_paper(&state, &paper_id).await?;
    paper.entities = state
        .store
        .paper_entities(&paper_id)
        .await
        .map_err(map_err)?;
    Ok(Json(paper))
}

pub async fn add_paper(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddPaperRequest>,
) -> ApiResult<papered::paper::Paper> {
    let expanded = normalize_input_path(&req.file_path);
    let path = expanded.as_path();
    let backend = {
        let config = state.config.read().await;
        PdfBackend::from_config(&config)
    };
    validate_addable_file(path, backend).await?;
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled");
    let mut paper = Paper::new(title);
    paper.source = Some(PaperSource::Manual);
    crate::state::queue_paper_for_indexing(
        &*state.store,
        &state.import_tx,
        &mut paper,
        Some(expanded.to_string_lossy().into_owned()),
        false,
        false,
    )
    .await?;
    Ok(Json(paper))
}

pub async fn import_paper(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<ImportResponse> {
    // CSRF guard: multipart/form-data bypasses CORS preflight, so we must
    // validate the origin explicitly for state-changing requests.
    super::config::check_local_origin(&headers)?;
    let field = multipart
        .next_field()
        .await
        .map_err(|e| bad_request_msg(format!("Failed to read multipart data: {e}")))?
        .ok_or_else(|| bad_request_msg("No file field found in request".to_string()))?;
    let file_name = field
        .file_name()
        .map(std::string::ToString::to_string)
        .ok_or_else(|| bad_request_msg("Uploaded file has no filename".to_string()))?;
    let path = std::path::Path::new(&file_name);
    let source = papered::paper::source::DocumentSource::from_path(path)
        .ok_or_else(|| bad_request_msg(unsupported_file_type_error(path)))?;
    let imports_dir = state.config.read().await.data_dir.join("imports");
    tokio::fs::create_dir_all(&imports_dir)
        .await
        .map_err(|e| internal_error(format!("Failed to create imports directory: {e}")))?;
    let title = std::path::Path::new(&file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled");
    let mut paper = papered::paper::Paper::new(title);
    paper.source = Some(PaperSource::Manual);
    let dest_filename = format!("{}_{}", paper.id, file_name);
    let dest_path = imports_dir.join(&dest_filename);
    let tmp_path = imports_dir.join(format!(".{}.tmp", paper.id));

    let (limit, mode_label) = {
        let config = state.config.read().await;
        let backend = PdfBackend::from_config(&config);
        (backend.size_limit_bytes(), backend.label())
    };

    let mut tmp_file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(|e| internal_error(format!("Failed to create temp file: {e}")))?;
    let mut total_size: u64 = 0;
    let mut stream = field;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| bad_request_msg(format!("Failed to read upload chunk: {e}")))?;
        total_size += chunk.len() as u64;
        if total_size > limit {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(payload_too_large(format!(
                "File size {total_size} bytes exceeds {mode_label} limit of {limit} bytes"
            )));
        }
        tmp_file
            .write_all(&chunk)
            .await
            .map_err(|e| internal_error(format!("Failed to write temp file: {e}")))?;
    }
    tmp_file
        .flush()
        .await
        .map_err(|e| internal_error(format!("Failed to flush temp file: {e}")))?;

    tokio::fs::rename(&tmp_path, &dest_path)
        .await
        .map_err(|e| internal_error(format!("Failed to save file: {e}")))?;

    let file_path = dest_path.to_string_lossy().into_owned();
    crate::state::queue_paper_for_indexing(
        &*state.store,
        &state.import_tx,
        &mut paper,
        Some(file_path),
        false,
        false,
    )
    .await?;

    let source_str = source.as_str();

    Ok(Json(ImportResponse {
        paper_id: paper.id,
        source: source_str.to_string(),
        title: paper.title,
        status: paper.status.to_string(),
        file_path: paper.file_path,
    }))
}

/// Opens a native file picker dialog on the daemon host and returns the
/// selected file paths.  This lets the web UI reference files by their real
/// path (no copy into the imports directory).
///
/// A user cancel is **not** an error: it returns `200` with an empty `paths`
/// array. A dialog that fails to open returns `5xx` with a readable message
/// so the UI can tell the two apart.
pub async fn pick_file() -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let paths = pick_file_paths().await?;
    Ok(Json(serde_json::json!({ "paths": paths })))
}

/// macOS: rfd's AppKit backend panics when there is no `NSApplication` event
/// loop on the main thread — exactly the daemon's situation — and the panic
/// surfaces as a failed join. Drive the system dialog through `osascript`
/// instead; it works without an event loop and reports cancel vs. failure
/// via its exit status.
#[cfg(target_os = "macos")]
async fn pick_file_paths() -> Result<Vec<String>, (StatusCode, Json<ApiError>)> {
    const SCRIPT_LINES: [&str; 6] = [
        "set sel to choose file with multiple selections allowed of type {\"pdf\", \"md\", \"txt\", \"tex\", \"docx\", \"png\", \"jpg\", \"jpeg\", \"webp\", \"gif\", \"bmp\"}",
        "set out to \"\"",
        "repeat with f in sel",
        "set out to out & POSIX path of f & linefeed",
        "end repeat",
        "return out",
    ];
    let mut cmd = std::process::Command::new("osascript");
    for line in SCRIPT_LINES {
        cmd.arg("-e").arg(line);
    }
    // osascript blocks until the user dismisses the dialog — keep it off the
    // async runtime threads.
    let output = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .map_err(|e| internal_error(format!("File picker task failed: {e}")))?
        .map_err(|e| internal_error(format!("Failed to launch the file picker: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "User canceled. (-128)" is the normal dismiss path, not a failure.
        if stderr.contains("(-128)") || stderr.contains("User canceled") {
            return Ok(Vec::new());
        }
        return Err(internal_error(format!(
            "File picker failed: {}",
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(std::string::ToString::to_string)
        .collect())
}

/// Other platforms: rfd works fine headful here. A panicking/failed dialog
/// task is mapped to an error response instead of a silent empty selection.
#[cfg(not(target_os = "macos"))]
async fn pick_file_paths() -> Result<Vec<String>, (StatusCode, Json<ApiError>)> {
    tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .add_filter(
                "Paper files",
                &[
                    "pdf", "md", "txt", "tex", "docx", "png", "jpg", "jpeg", "webp", "gif", "bmp",
                ],
            )
            .add_filter("All files", &[""])
            .pick_files()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| internal_error(format!("File picker failed to open: {e}")))
}

pub async fn delete_paper(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiStatusResult {
    validate_paper_id(&paper_id)?;
    state
        .indexer
        .read()
        .await
        .delete_paper(&paper_id)
        .await
        .map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn update_paper(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
    Json(req): Json<UpdatePaperRequest>,
) -> ApiResult<papered::paper::Paper> {
    let mut paper = require_paper(&state, &paper_id).await?;
    if let Some(title) = req.title {
        paper.title = title;
    }
    if let Some(authors) = req.authors {
        paper.authors = authors;
    }
    if let Some(affiliations) = req.affiliations {
        paper.affiliations = affiliations;
    }
    if let Some(venue) = req.venue {
        paper.venue = Some(venue);
    }
    if let Some(doi) = req.doi {
        paper.doi = Some(doi);
    }
    if let Some(abstract_text) = req.abstract_text {
        paper.abstract_text = Some(abstract_text);
    }
    if let Some(keywords) = req.keywords {
        paper.keywords = keywords;
    }
    if let Some(urls) = req.urls {
        paper.urls = urls;
    }
    if let Some(emails) = req.emails {
        paper.emails = emails;
    }
    if let Some(extra) = req.extra {
        paper.extra = Some(extra);
    }
    if let Some(paper_type) = req.paper_type {
        paper.paper_type = Some(paper_type);
    }
    if let Some(published_date) = req.published_date {
        paper.published_date = Some(published_date);
    }
    if let Some(corresponding_author) = req.corresponding_author {
        paper.corresponding_author = corresponding_author;
    }
    if let Some(data_availability) = req.data_availability {
        paper.data_availability = Some(data_availability);
    }
    state.store.update_paper(&paper).await.map_err(map_err)?;
    Ok(Json(paper))
}

pub async fn reindex_paper(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiResult<Paper> {
    validate_paper_id(&paper_id)?;
    do_reindex(&state, &paper_id, false).await
}

pub async fn reindex_paper_sections(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiResult<Paper> {
    validate_paper_id(&paper_id)?;
    do_reindex(&state, &paper_id, true).await
}

async fn do_reindex(
    state: &Arc<AppState>,
    paper_id: &str,
    sections_only: bool,
) -> ApiResult<Paper> {
    let mut paper = state
        .store
        .get_paper(paper_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| not_found(format!("Paper {paper_id} not found")))?;
    if paper.status == PaperStatus::Processing {
        return Err(conflict("Paper is already being processed"));
    }
    let has_sections = state
        .store
        .get_sections(paper_id)
        .await
        .is_ok_and(|s| !s.sections.is_empty());
    let lacks_file = paper.file_path.is_none();
    if !sections_only && lacks_file && !has_sections {
        return Err(bad_request_msg(format!(
            "Paper {paper_id} has no associated file and no sections to re-embed"
        )));
    }
    let previous_status = paper.status;
    paper.status = PaperStatus::Processing;
    paper.retry_count = 0;
    paper.error_message = None;
    state.store.update_paper(&paper).await.map_err(map_err)?;

    let reembed_only = !sections_only && has_sections && lacks_file;

    let job = papered::util::IndexJob {
        paper_id: paper_id.to_string(),
        file_path: paper.file_path.clone().unwrap_or_default(),
        is_reindex: !reembed_only,
        retry_count: paper.retry_count,
        sections_only: sections_only && !reembed_only,
        reembed_only,
    };
    if state.import_tx.try_send(job).is_err() {
        paper.status = previous_status;
        state.store.update_paper(&paper).await.map_err(map_err)?;
        return Err(service_unavailable(
            ERR_QUEUE_CLOSED,
            "Import channel full, try again later",
        ));
    }
    Ok(Json(paper))
}

pub async fn get_paper_sections(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiResult<Vec<papered::paper::section::SectionView>> {
    validate_paper_id(&paper_id)?;
    let sections = state.store.get_sections(&paper_id).await.map_err(map_err)?;
    Ok(Json(sections.to_views()))
}

pub async fn get_paper_figures(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiResult<Vec<papered::index::multimodal::FigureInfo>> {
    validate_paper_id(&paper_id)?;
    let data_dir = state.config.read().await.data_dir.clone();
    let mut figures = state.store.get_figures(&paper_id).await.map_err(map_err)?;
    resolve_figure_paths(&data_dir, &paper_id, &mut figures).await;
    Ok(Json(figures))
}

/// Read an image file and build the response, mapping a missing file to 404.
/// `cache_control` lets each caller pick its freshness policy: figure
/// images are replaced in place on reindex (same URL, new content), so
/// they must not be served stale from the browser cache.
async fn image_response(
    path: std::path::PathBuf,
    cache_control: &'static str,
) -> Result<Response<Body>, (StatusCode, Json<ApiError>)> {
    let bytes = tokio::fs::read(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            not_found(format!("Image file not found: {}", path.display()))
        } else {
            internal_error(format!("Failed to read image {}: {e}", path.display()))
        }
    })?;
    let mut resp = Response::new(Body::from(bytes));
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(image_content_type(&path)),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    Ok(resp)
}

/// `GET /api/v1/papers/{id}/figures/{figure_id}/image` — serve a figure's
/// image bytes so the web UI can display extracted figures (embedded UI
/// assets and `default-src 'self'` rule out direct file access).
pub async fn get_figure_image(
    State(state): State<Arc<AppState>>,
    Path((paper_id, figure_id)): Path<(String, String)>,
) -> Result<Response<Body>, (StatusCode, Json<ApiError>)> {
    validate_paper_id(&paper_id)?;
    if !is_safe_paper_id(&figure_id) {
        return Err(bad_request_msg(format!("Invalid figure ID: {figure_id:?}")));
    }
    let figures = state.store.get_figures(&paper_id).await.map_err(map_err)?;
    let rel = figures
        .iter()
        .find(|f| f.id == figure_id)
        .and_then(|f| f.image_path.clone())
        .ok_or_else(|| not_found(format!("Figure image not found: {figure_id}")))?;
    let data_dir = state.config.read().await.data_dir.clone();
    // safe_join enforces containment under data_dir/papers/{paper_id}.
    let path = safe_join(&data_dir, &paper_id, &rel)
        .await
        .map_err(map_err)?;
    // Reindex rewrites the figure image under the same URL — the browser
    // must revalidate instead of serving a stale copy for an hour.
    image_response(path, "private, no-cache").await
}

/// Resolve a relative path under `data_dir`, enforcing containment via
/// canonicalization so symlinked components cannot escape the data directory.
fn resolve_safe_path(
    data_dir: &std::path::Path,
    rel: &str,
) -> Result<std::path::PathBuf, (StatusCode, Json<ApiError>)> {
    if rel.contains("..") || rel.starts_with('/') || rel.starts_with('\\') || rel.contains('\\') {
        return Err(bad_request_msg(format!(
            "Unsafe path component in stored value: {rel}"
        )));
    }
    let candidate = data_dir.join(rel);
    let canonical_data_dir =
        std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    match std::fs::canonicalize(&candidate) {
        Ok(c) if c.starts_with(&canonical_data_dir) => Ok(c),
        Ok(_) => Err(bad_request_msg(format!(
            "Path resolves outside data directory: {rel}"
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(not_found(format!("File not found: {rel}")))
        }
        Err(e) => Err(internal_error(format!("Failed to resolve path {rel}: {e}"))),
    }
}

/// `GET /api/v1/papers/{id}/cover` — serve the paper's full-size cover image.
pub async fn get_paper_cover(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> Result<Response<Body>, (StatusCode, Json<ApiError>)> {
    let paper = require_paper(&state, &paper_id).await?;
    let rel = paper
        .cover_path
        .ok_or_else(|| not_found(format!("Paper has no cover: {paper_id}")))?;
    let data_dir = state.config.read().await.data_dir.clone();
    let path = resolve_safe_path(&data_dir, &rel)?;
    image_response(path, "private, max-age=3600").await
}

/// `GET /api/v1/papers/{id}/cover/thumb` — serve the paper's cover thumbnail (small).
/// A missing thumbnail is a plain 404; rebuild with `POST /health/regenerate-covers`.
pub async fn get_paper_cover_thumb(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> Result<Response<Body>, (StatusCode, Json<ApiError>)> {
    let paper = require_paper(&state, &paper_id).await?;
    paper
        .cover_path
        .as_ref()
        .ok_or_else(|| not_found(format!("Paper has no cover: {paper_id}")))?;
    let data_dir = state.config.read().await.data_dir.clone();
    let thumb_rel = format!("covers/{paper_id}_thumb.jpg");
    let path = resolve_safe_path(&data_dir, &thumb_rel)?;
    image_response(path, "private, max-age=3600").await
}

/// `POST /api/v1/papers/{id}/cover/regenerate` — regenerate the paper's cover thumbnail.
pub async fn regenerate_paper_cover(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> Result<Json<RegenerateCoverResponse>, (StatusCode, Json<ApiError>)> {
    let paper = require_paper(&state, &paper_id).await?;

    let file_path = paper
        .file_path
        .ok_or_else(|| bad_request_msg(format!("Paper has no file path: {paper_id}")))?;
    let pdf_path = std::path::Path::new(&file_path);
    if !pdf_path.exists() {
        return Err(not_found(format!("Paper file not found: {file_path}")));
    }

    let data_dir = state.config.read().await.data_dir.clone();
    let result = papered::cover::generate_cover(pdf_path, &paper_id, &data_dir)
        .map_err(|e| internal_error(format!("Cover generation failed: {e}")))?;

    match result {
        Some(cover_rel) => {
            if let Err(e) = state.store.update_paper_cover(&paper_id, &cover_rel).await {
                tracing::warn!(paper_id = %paper_id, "Failed to persist cover_path: {e}");
            }
            Ok(Json(RegenerateCoverResponse {
                cover_path: cover_rel,
                regenerated: true,
            }))
        }
        None => Ok(Json(RegenerateCoverResponse {
            cover_path: String::new(),
            regenerated: false,
        })),
    }
}

/// Resolve a single chunk by id — the endpoint RAG citations point at.
/// Returns the chunk content plus its heading path for navigation.
pub async fn get_paper_chunk(
    State(state): State<Arc<AppState>>,
    Path((paper_id, chunk_id)): Path<(String, String)>,
) -> ApiResult<ChunkDetailResponse> {
    validate_paper_id(&paper_id)?;
    let (chunk, heading_path) =
        papered::retrieval::chunk_with_heading_path(state.store.as_ref(), &paper_id, &chunk_id)
            .await
            .map_err(map_err)?
            .ok_or_else(|| not_found(format!("Chunk not found: {chunk_id}")))?;
    Ok(Json(ChunkDetailResponse {
        chunk,
        heading_path,
    }))
}

pub async fn batch_add_papers(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchAddPapersRequest>,
) -> ApiResult<BatchAddPapersResponse> {
    if req.file_paths.is_empty() {
        return Err(bad_request_msg("file_paths must not be empty".to_string()));
    }
    check_batch_size(req.file_paths.len())?;
    let backend = {
        let config = state.config.read().await;
        PdfBackend::from_config(&config)
    };
    let mut results: Vec<BatchAddPaperResult> = Vec::with_capacity(req.file_paths.len());
    let mut queued = 0usize;
    let mut errors = 0usize;
    for file_path in &req.file_paths {
        let normalized = normalize_input_path(file_path);
        let path = normalized.as_path();
        if let Err((_, Json(err))) = validate_addable_file(path, backend).await {
            results.push(batch_add_paper_error(file_path, err.message));
            errors += 1;
            continue;
        }
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled");
        let mut paper = Paper::new(title);
        paper.source = Some(PaperSource::Manual);
        if let Err((_, Json(err))) = crate::state::queue_paper_for_indexing(
            &*state.store,
            &state.import_tx,
            &mut paper,
            Some(normalized.to_string_lossy().into_owned()),
            false,
            false,
        )
        .await
        {
            results.push(batch_add_paper_error(file_path, err.message));
            errors += 1;
            continue;
        }
        results.push(BatchAddPaperResult {
            id: paper.id.clone(),
            title: paper.title.clone(),
            file_path: file_path.clone(),
            status: paper.status.to_string(),
            error: None,
        });
        queued += 1;
    }
    Ok(Json(BatchAddPapersResponse {
        results,
        queued,
        errors,
    }))
}

pub async fn batch_delete_papers(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchPaperIdsRequest>,
) -> ApiResult<BatchResult> {
    check_batch_size(req.paper_ids.len())?;
    let mut result = BatchResult {
        success: Vec::with_capacity(req.paper_ids.len()),
        errors: Vec::new(),
        request_id: req.request_id.clone(),
    };
    for pid in &req.paper_ids {
        match state.indexer.read().await.delete_paper(pid).await {
            Ok(()) => result.success.push(pid.clone()),
            Err(e) => result.errors.push(BatchError {
                paper_id: pid.clone(),
                error: e.to_string(),
            }),
        }
    }
    Ok(Json(result))
}

pub async fn batch_reindex_papers_sections(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchPaperIdsRequest>,
) -> ApiResult<BatchResult> {
    check_batch_size(req.paper_ids.len())?;
    let mut result = BatchResult {
        success: Vec::with_capacity(req.paper_ids.len()),
        errors: Vec::new(),
        request_id: req.request_id.clone(),
    };
    for pid in &req.paper_ids {
        let paper = match state.store.get_paper(pid).await.map_err(map_err)? {
            Some(p) => p,
            None => {
                result.errors.push(BatchError {
                    paper_id: pid.clone(),
                    error: "Paper not found".to_string(),
                });
                continue;
            }
        };
        if paper.status == PaperStatus::Processing {
            result.errors.push(BatchError {
                paper_id: pid.clone(),
                error: "Paper is already being reindexed".to_string(),
            });
            continue;
        }
        let previous_status = paper.status;
        if let Err(e) = state
            .store
            .update_paper_status(pid, PaperStatus::Processing.as_str(), None, None)
            .await
        {
            result.errors.push(BatchError {
                paper_id: pid.clone(),
                error: e.to_string(),
            });
            continue;
        }
        let job = papered::util::IndexJob {
            paper_id: pid.clone(),
            file_path: paper.file_path.clone().unwrap_or_default(),
            is_reindex: true,
            retry_count: 0,
            sections_only: true,
            reembed_only: false,
        };
        if state.import_tx.send(job).await.is_err() {
            if let Err(e) = state
                .store
                .update_paper_status(pid, previous_status.as_str(), None, None)
                .await
            {
                tracing::error!(paper_id = %pid, error = %e, previous_status = %previous_status, "Failed to restore paper status after worker send error (batch)");
            }
            result.errors.push(BatchError {
                paper_id: pid.clone(),
                error: "Indexing worker is not running".to_string(),
            });
            continue;
        }
        result.success.push(pid.clone());
    }
    Ok(Json(result))
}

async fn get_papers_batch(
    store: &dyn VectorStore,
    ids: &[String],
) -> Result<Vec<Paper>, (StatusCode, Json<ApiError>)> {
    if ids.is_empty() {
        return Err(bad_request_msg("paper_ids must not be empty".to_string()));
    }
    check_batch_size(ids.len())?;
    let id_refs: Vec<&str> = ids.iter().map(std::string::String::as_str).collect();
    store.get_papers_by_ids(&id_refs).await.map_err(map_err)
}

pub async fn batch_paper_status(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BatchStatusRequest>,
) -> ApiResult<Vec<Paper>> {
    let papers = get_papers_batch(state.store.as_ref(), &req.ids).await?;
    Ok(Json(papers))
}

pub async fn stats(State(state): State<Arc<AppState>>) -> ApiResult<StatsResponse> {
    let papers = state.store.paper_count().await.map_err(map_err)?;
    let vectors = state.store.count().await.map_err(map_err)?;
    let figures = state.store.figure_count().await.map_err(map_err)?;
    let data_dir = state.config.read().await.data_dir.clone();
    let db_path = state.config.read().await.db_path();
    let data_dir_clone = data_dir.clone();
    let (data_dir_size_mb, storage) = tokio::task::spawn_blocking(move || {
        let total = dir_size_mb(&data_dir_clone);
        // SQLite keeps live data in the -wal/-shm sidecars between
        // checkpoints; the main .db file alone under-reports the footprint.
        let mut db_bytes = path_size(&db_path);
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = db_path.clone().into_os_string();
            sidecar.push(suffix);
            db_bytes += path_size(std::path::Path::new(&sidecar));
        }
        let images_bytes = papered::util::fs::dir_size(data_dir_clone.join("papers")).unwrap_or(0);
        let covers_bytes = papered::util::fs::dir_size(data_dir_clone.join("covers")).unwrap_or(0);
        (
            total,
            super::types::StorageBreakdown {
                db_bytes,
                images_bytes,
                covers_bytes,
            },
        )
    })
    .await
    .map_err(|e| internal_error(format!("Dir size task failed: {e}")))?;
    Ok(Json(StatsResponse {
        papers,
        vectors,
        figures,
        data_dir_size_mb,
        data_dir: data_dir.display().to_string(),
        config_path: papered::AppConfig::find_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        storage,
    }))
}
