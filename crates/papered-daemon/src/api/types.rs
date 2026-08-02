use axum::http::StatusCode;
use axum::response::Json;
use papered::error::{self as papered_error, ApiError, PaperedError};
use papered::search::SearchMethod;
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------
// Daemon-specific error code constants
// ------------------------------------------------------------------

/// `invalid_argument` — the most common bad-request code. Re-exported from
/// `papered::error` so all call sites share one spelling.
pub(crate) const ERR_INVALID_ARGUMENT: &str = papered_error::ERR_INVALID_ARGUMENT;
pub(crate) const ERR_INVALID_CONFIG: &str = "invalid_config";
pub(crate) const ERR_CONFIG_CONFLICT: &str = "config_conflict";
pub(crate) const ERR_SSRF_BLOCKED: &str = "ssrf_blocked";
pub(crate) const ERR_FORBIDDEN: &str = "forbidden";
pub(crate) const ERR_QUEUE_CLOSED: &str = "queue_closed";
pub(crate) const ERR_SYNC_BUSY: &str = "sync_busy";
pub(crate) const ERR_LATTICE_ERROR: &str = "lattice_error";
pub(crate) const ERR_ZOTERO_ERROR: &str = "zotero_error";

// ------------------------------------------------------------------
// Convenience helpers: create ApiError + status → HTTP response tuple
// ------------------------------------------------------------------

/// Bad request with an explicit non-default code.
pub(crate) fn bad_request(
    code: impl Into<String>,
    message: impl Into<String>,
) -> (StatusCode, Json<ApiError>) {
    (StatusCode::BAD_REQUEST, Json(ApiError::new(code, message)))
}

/// Bad request with the default `invalid_argument` code — the common case.
pub(crate) fn bad_request_msg(message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError::new(ERR_INVALID_ARGUMENT, message)),
    )
}

/// Shared sync cancellation message — parameterised by source name so Lattice
/// and Zotero cancel endpoints use the same template. Formerly the Zotero
/// variant was missing the source-name prefix (inconsistent with Lattice).
pub(crate) fn sync_cancel_message(source: &str) -> String {
    format!(
        "{source} sync cancellation requested. The current sync cycle will stop after the current item completes."
    )
}

pub(crate) fn not_found(message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError::new(papered_error::ERR_NOT_FOUND, message)),
    )
}

pub(crate) fn internal_error(message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError::new(papered_error::ERR_INTERNAL, message)),
    )
}

pub(crate) fn conflict(message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::CONFLICT,
        Json(ApiError::new(papered_error::ERR_CONFLICT, message)),
    )
}

pub(crate) fn payload_too_large(message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(ApiError::new(papered_error::ERR_PAYLOAD_TOO_LARGE, message)),
    )
}

pub(crate) fn service_unavailable(
    code: impl Into<String>,
    message: impl Into<String>,
) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiError::new(code, message)),
    )
}

pub(crate) fn bad_gateway(
    code: impl Into<String>,
    message: impl Into<String>,
) -> (StatusCode, Json<ApiError>) {
    (StatusCode::BAD_GATEWAY, Json(ApiError::new(code, message)))
}

pub(crate) fn unprocessable_entity(message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ApiError::new(papered_error::ERR_INVALID_ARGUMENT, message)),
    )
}

// ------------------------------------------------------------------
// Error mapping
// ------------------------------------------------------------------

pub(crate) fn map_err(e: PaperedError) -> (StatusCode, Json<ApiError>) {
    let (status, api_error) = e.into_api_error();
    match status {
        StatusCode::INTERNAL_SERVER_ERROR => tracing::error!("API error: {}", api_error.message),
        StatusCode::NOT_FOUND => tracing::debug!("API not found: {}", api_error.message),
        _ => tracing::warn!("API client error ({}): {}", status, api_error.message),
    }
    (status, Json(api_error))
}

// ------------------------------------------------------------------
// Type aliases
// ------------------------------------------------------------------

pub(crate) type ApiResult<T> = std::result::Result<Json<T>, (StatusCode, Json<ApiError>)>;
pub(crate) type ApiStatusResult = std::result::Result<StatusCode, (StatusCode, Json<ApiError>)>;

// ------------------------------------------------------------------
// Constants
// ------------------------------------------------------------------

pub(crate) const MAX_LIMIT: usize = papered::search::MAX_RESULT_LIMIT;
pub(crate) const MAX_BATCH_SIZE: usize = 1000;

const DEFAULT_LIST_LIMIT: usize = 20;
const DEFAULT_SIMILAR_LIMIT: usize = 5;
const DEFAULT_LATTICE_LIMIT: u32 = 20;
/// Default number of papers included in the relatedness graph.
const DEFAULT_GRAPH_LIMIT: usize = 200;
/// Default per-node edge cap for the relatedness graph.
const DEFAULT_GRAPH_DEGREE: usize = 5;

// ------------------------------------------------------------------
// Validation helpers
// ------------------------------------------------------------------

pub(crate) fn validate_paper_id(id: &str) -> Result<(), (StatusCode, Json<ApiError>)> {
    papered::util::paths::validate_paper_id(id).map_err(|e| {
        let (status, api_error) = e.into_api_error();
        (status, Json(api_error))
    })
}

pub(crate) fn check_batch_size(n: usize) -> Result<(), (StatusCode, Json<ApiError>)> {
    if n > MAX_BATCH_SIZE {
        return Err(bad_request(
            papered_error::ERR_INVALID_ARGUMENT,
            format!("Batch size {n} exceeds maximum {MAX_BATCH_SIZE}"),
        ));
    }
    Ok(())
}

/// Fetch a paper by ID, validating the ID and returning a `not_found` error if absent.
pub(crate) async fn require_paper(
    state: &crate::AppState,
    paper_id: &str,
) -> Result<papered::paper::Paper, (StatusCode, Json<ApiError>)> {
    validate_paper_id(paper_id)?;
    state
        .store
        .get_paper(paper_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| not_found(format!("Paper not found: {paper_id}")))
}

// ------------------------------------------------------------------
// Request / response types
// ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListPapersQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    pub status: Option<String>,
    pub paper_type: Option<String>,
    /// Exact keyword filter — matches papers whose `keywords` array contains
    /// this value (case-insensitive). Used by the clickable keyword chips.
    pub keyword: Option<String>,
    /// Exact-match bio-entity filters (case-sensitive); combine with AND.
    pub species: Option<String>,
    pub gene: Option<String>,
    pub technique: Option<String>,
    pub pathway: Option<String>,
    #[serde(default)]
    pub sort_by: Option<String>,
    #[serde(default)]
    pub sort_order: Option<String>,
}

fn default_limit() -> usize {
    DEFAULT_LIST_LIMIT
}

/// One entry of the daemon's paper-list response: the paper itself plus the
/// user's annotation summary (fetched in one batch query per list call, not
/// per row). Extra fields serialize inline next to the paper's own fields.
#[derive(Debug, Serialize)]
pub struct PaperListItem {
    #[serde(flatten)]
    pub paper: papered::paper::Paper,
    /// The user's star rating (1–5); absent when unrated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<i64>,
    /// Number of user comments on the paper.
    pub comment_count: i64,
}

/// Daemon-side paginated paper list. Distinct from `papered::ListPapersResponse`
/// (shared with the CLI) because entries carry annotation summaries.
#[derive(Debug, Serialize)]
pub struct ListPapersResponse {
    pub papers: Vec<PaperListItem>,
    pub total: usize,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub section_type: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_min_score")]
    pub min_score: f32,
    #[serde(default)]
    pub search_method: Option<SearchMethod>,
}

fn default_min_score() -> f32 {
    papered::search::DEFAULT_MIN_SCORE
}

#[derive(Debug, Deserialize)]
pub struct SimilarRequest {
    pub paper_id: String,
    pub section_type: Option<String>,
    #[serde(default = "default_similar_limit")]
    pub limit: usize,
}

fn default_similar_limit() -> usize {
    DEFAULT_SIMILAR_LIMIT
}

#[derive(Debug, Deserialize)]
pub struct ContentSearchRequest {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_min_score")]
    pub min_score: f32,
}

/// Query parameters for the paper relatedness-graph endpoint.
#[derive(Debug, Deserialize)]
pub struct GraphQuery {
    /// How many papers to include (most recent first).
    #[serde(default = "default_graph_limit")]
    pub limit: usize,
    /// Maximum strongest edges kept per node, to keep the network readable.
    #[serde(default = "default_graph_degree")]
    pub max_edges_per_node: usize,
    /// Paper the user explicitly located: force-include it together with its
    /// strongest library-wide neighbors when it would otherwise be absent or
    /// isolated in the most-recent slice.
    pub focus: Option<String>,
}

fn default_graph_limit() -> usize {
    DEFAULT_GRAPH_LIMIT
}

fn default_graph_degree() -> usize {
    DEFAULT_GRAPH_DEGREE
}

#[derive(Debug, Deserialize)]
pub struct AddPaperRequest {
    pub file_path: String,
}

#[derive(Debug, Deserialize)]
pub struct LatticeImportRequest {
    pub paper_id: String,
    pub file_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LatticeImportResponse {
    pub paper: papered::paper::Paper,
    pub source: String,
    pub indexing_queued: bool,
}

#[derive(Debug, Serialize)]
pub struct ImportResponse {
    pub paper_id: String,
    pub source: String,
    pub title: String,
    pub status: String,
    pub file_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LatticeStatusResponse {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// True when the auto-sync circuit breaker has tripped (too many
    /// consecutive sync failures) and automatic sync is paused.
    pub auto_sync_paused: bool,
    /// Consecutive run-level sync failures since the last successful sync.
    pub consecutive_failures: u32,
    /// Currently configured collection names to sync. Empty = sync all.
    pub collections: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ZoteroStatusResponse {
    pub available: bool,
    pub base_url: Option<String>,
    pub user_id: Option<u32>,
    pub username: Option<String>,
    /// True when the auto-sync circuit breaker has tripped (too many
    /// consecutive sync failures) and automatic sync is paused.
    pub auto_sync_paused: bool,
    /// Consecutive run-level sync failures since the last successful sync.
    pub consecutive_failures: u32,
    /// Currently configured collection keys to sync. Empty = sync all.
    pub collection_keys: Vec<String>,
    /// Whether sub-collections of selected collections are included.
    pub recursive_collections: bool,
}

#[derive(Debug, Serialize)]
pub struct ZoteroCollectionListResponse {
    pub collections: Vec<ZoteroCollectionItem>,
}

#[derive(Debug, Serialize)]
pub struct ZoteroCollectionItem {
    pub key: String,
    pub name: String,
    pub parent_collection: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ZoteroSyncCollectionsRequest {
    /// Collection keys to sync. Empty = sync all collections.
    #[serde(default)]
    pub collection_keys: Vec<String>,
    /// Whether to recursively include sub-collections.
    #[serde(default = "default_recursive_collections")]
    pub recursive_collections: bool,
}

fn default_recursive_collections() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct ZoteroSyncCollectionsResponse {
    pub collection_keys: Vec<String>,
    pub recursive_collections: bool,
}

#[derive(Debug, Serialize)]
pub struct SyncCancelResponse {
    pub cancelled: bool,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct LatticeCollectionListResponse {
    pub collections: Vec<LatticeCollectionItem>,
}

#[derive(Debug, Serialize)]
pub struct LatticeCollectionItem {
    pub id: String,
    pub name: String,
    pub path: String,
    pub depth: u32,
}

#[derive(Debug, Deserialize)]
pub struct LatticeSyncCollectionsRequest {
    /// Collection names to sync. Empty = sync all collections.
    #[serde(default)]
    pub collection_names: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LatticeSyncCollectionsResponse {
    pub collection_names: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ZoteroSyncResponse {
    pub sync_id: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct ZoteroSyncStatusResponse {
    pub sync_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<papered::sync::SyncReport>,
}

#[derive(Debug, Serialize)]
pub struct ImportQueueItem {
    pub paper_id: String,
    pub file_path: String,
    pub status: String,
    /// Paper title; absent when extraction has not produced one yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Indexing failure reason; present only on failed entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportQueueResponse {
    /// True while the indexing worker pool is paused via the pause endpoint.
    pub paused: bool,
    pub items: Vec<ImportQueueItem>,
}

#[derive(Debug, Deserialize)]
pub struct SetIndexingPausedRequest {
    pub paused: bool,
}

#[derive(Debug, Serialize)]
pub struct IndexingPausedResponse {
    pub paused: bool,
}

#[derive(Debug, Serialize)]
pub struct MissingFigureRef {
    pub paper_id: String,
    pub figure_id: String,
    pub paper_title: String,
}

#[derive(Debug, Serialize)]
pub struct PaperRefResponse {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct CleanupResponse {
    pub removed_papers: Vec<String>,
    pub removed_orphan_vectors: Vec<String>,
    pub removed_orphan_directories: Vec<String>,
    pub removed_figures: usize,
}

#[derive(Debug, Deserialize)]
pub struct ResetRequest {
    /// Actually delete data. When false, returns a preview of what would be removed.
    #[serde(default)]
    pub force: bool,
    /// Also remove logs/ and cache/ directories.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Serialize)]
pub struct ResetResponse {
    pub status: String,
    pub preview: bool,
    /// Paths that were (or would be) removed.
    pub removed_paths: Vec<String>,
    /// Total bytes freed (or that would be freed). 0 in preview mode if size calculation is skipped.
    pub bytes_freed: u64,
    pub message: String,
}

/// Response for the bare `/health` liveness check.
#[derive(Debug, Serialize)]
pub struct ServiceHealthResponse {
    pub status: String,
    pub service: String,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub paper_count: usize,
    pub vector_count: usize,
    pub embedding_dimension: Option<usize>,
    pub embedding_model: Option<String>,
    pub embedding_model_status: String,
    pub embedding_model_error: Option<String>,
    pub config_needs_restart: bool,
    pub reembed_pending: usize,
    pub reembed_completed: usize,
    pub reembed_total: usize,
    pub processing_count: usize,
    pub failed_count: usize,
    /// True when the indexing worker pool is paused (see `POST /api/v1/index-queue/pause`).
    pub indexing_paused: bool,
}

#[derive(Debug, Serialize)]
pub struct KbHealthResponse {
    pub papers_without_vectors: Vec<PaperRefResponse>,
    pub orphaned_vector_paper_ids: Vec<String>,
    pub papers_with_missing_files: Vec<PaperRefResponse>,
    pub figures_with_missing_images: Vec<MissingFigureRef>,
    pub orphaned_directories: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DataQualityResponse {
    pub total_checked: usize,
    pub papers_with_issues: Vec<PaperQualityIssue>,
    pub issue_summary: std::collections::HashMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct PaperQualityIssue {
    pub paper_id: String,
    pub title: String,
    pub issues: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ImageQualityResponse {
    pub total_scanned: usize,
    pub bad_images: Vec<BadImageRef>,
}

#[derive(Debug, Serialize)]
pub struct BadImageRef {
    pub paper_id: String,
    pub paper_title: String,
    pub filename: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct ExportRequestBody {
    pub target: String,
    pub format: String,
    pub destination: String,
    pub paper_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct EmbeddingUpdateRequest {
    pub model_key: Option<String>,
    #[serde(default)]
    pub reembed_all: bool,
}

#[derive(Debug, Deserialize)]
pub struct TestEndpointRequest {
    pub api_base: String,
    pub api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TestEmbeddingRequest {
    pub api_base: String,
    pub api_key: Option<String>,
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct TestEmbeddingResponse {
    pub status: String,
    pub reachable: bool,
    pub dimension: usize,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TestRerankerRequest {
    pub api_base: String,
    pub api_key: Option<String>,
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct TestRerankerResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RagRequest {
    pub question: String,
    pub search_method: Option<SearchMethod>,
    pub prompt_id: Option<String>,
    pub paper_id: Option<String>,
    /// Force query enhancement (rewriting + HyDE) on/off; omit to let adaptive
    /// retrieval decide.
    #[serde(default)]
    pub use_enhancement: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct RagResponse {
    pub answer: String,
    pub sources: Vec<papered::llm::rag::RagSourceView>,
    pub search_method: String,
}

#[derive(Debug, Serialize)]
pub struct StorageBreakdown {
    /// Database footprint in bytes: main .db plus live -wal/-shm sidecars.
    pub db_bytes: u64,
    /// Figure images (papers/ directory) size in bytes.
    pub images_bytes: u64,
    /// Cover images (covers/ directory) size in bytes.
    pub covers_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub papers: usize,
    pub vectors: usize,
    pub figures: usize,
    pub data_dir_size_mb: u64,
    pub data_dir: String,
    pub config_path: String,
    pub storage: StorageBreakdown,
}

#[derive(Debug, Deserialize)]
pub struct BatchPaperIdsRequest {
    pub paper_ids: Vec<String>,
    pub request_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BatchResult {
    pub success: Vec<String>,
    pub errors: Vec<BatchError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BatchError {
    pub paper_id: String,
    pub error: String,
}

#[derive(Debug, Deserialize)]
pub struct BatchAddPapersRequest {
    pub file_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct BatchStatusRequest {
    pub ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct BatchAddPaperResult {
    pub id: String,
    pub title: String,
    /// The path as requested — echoed back so clients can attribute failures
    /// (error rows carry no id/title).
    pub file_path: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BatchAddPapersResponse {
    pub results: Vec<BatchAddPaperResult>,
    pub queued: usize,
    pub errors: usize,
}

#[derive(Debug, Serialize)]
pub struct RegenerateCoverResponse {
    pub cover_path: String,
    pub regenerated: bool,
}

#[derive(Debug, Deserialize)]
pub struct LatticeSearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_lattice_limit")]
    pub limit: u32,
}

fn default_lattice_limit() -> u32 {
    DEFAULT_LATTICE_LIMIT
}

#[derive(Debug, Deserialize)]
pub struct UpdatePaperRequest {
    pub title: Option<String>,
    pub authors: Option<Vec<String>>,
    pub affiliations: Option<Vec<String>>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub abstract_text: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub urls: Option<Vec<String>>,
    pub emails: Option<Vec<String>>,
    pub extra: Option<String>,
    pub paper_type: Option<String>,
    pub published_date: Option<String>,
    pub corresponding_author: Option<Vec<String>>,
    pub data_availability: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePromptRequest {
    pub name: String,
    pub description: Option<String>,
    pub system_prompt: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_temperature() -> f32 {
    0.2
}

#[derive(Debug, Serialize)]
pub struct DuplicatePaperSummary {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub published_date: Option<String>,
    pub file_hash: Option<String>,
    pub status: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct DuplicateGroupsResponse {
    pub groups: Vec<Vec<DuplicatePaperSummary>>,
}

#[derive(Debug, Serialize)]
pub struct ChunkDetailResponse {
    pub chunk: papered::chunker::Chunk,
    /// Root-first heading chain (e.g. "Intro > Methods"), matching the
    /// `section_path` carried by RAG citations. Absent when the chunk has no
    /// heading ancestors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SetupStatusResponse {
    pub needs_setup: bool,
    pub reasons: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rag_request_defaults_enhancement_flag_to_none() {
        let req: RagRequest = serde_json::from_str(r#"{"question": "q"}"#).unwrap();
        // None hands the decision to adaptive retrieval.
        assert_eq!(req.use_enhancement, None);
    }

    #[test]
    fn rag_request_explicit_enhancement_flag_is_respected() {
        let req: RagRequest =
            serde_json::from_str(r#"{"question": "q", "use_enhancement": false}"#).unwrap();
        assert_eq!(req.use_enhancement, Some(false));
    }
}
