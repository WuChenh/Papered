use axum::{
    extract::State,
    http::{HeaderMap, HeaderName, StatusCode, header},
    response::Json,
};
use papered::StrLabel;
use papered::util::fs::{human_readable_size, path_size};
use std::sync::Arc;

use super::types::{
    ApiResult, ERR_CONFIG_CONFLICT, ERR_INVALID_CONFIG, ERR_SSRF_BLOCKED, EmbeddingUpdateRequest,
    HealthResponse, ImportQueueItem, ResetRequest, ResetResponse, SetupStatusResponse,
    TestEmbeddingRequest, TestEmbeddingResponse, TestEndpointRequest, TestRerankerRequest,
    TestRerankerResponse, bad_request, bad_request_msg, internal_error, map_err,
};
use crate::AppState;
use papered::client::is_loopback_host;
use papered::error::ApiError;
use papered::paper::PaperStatus;
use serde::Serialize;

#[derive(Serialize)]
pub struct TestEndpointResponse {
    pub status: String,
    pub reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

fn config_etag(config: &papered::AppConfig) -> String {
    format!("\"{}\"", config.version_hash())
}

/// Replace every `Some(api_key)` in `config.providers` with the redacted
/// sentinel so API keys are never serialized into HTTP responses.
fn redact_api_keys(config: &mut papered::AppConfig) {
    for provider in config.providers.values_mut() {
        if provider.api_key.is_some() {
            provider.api_key = Some(REDACTED_API_KEY.to_string());
        }
    }
}

/// Restore any redacted sentinel values in `incoming` with the original
/// keys from `stored`. This lets the frontend round-trip the config without
/// ever seeing (or needing to re-send) the real key values.
fn restore_redacted_keys(incoming: &mut papered::AppConfig, stored: &papered::AppConfig) {
    for (name, provider) in incoming.providers.iter_mut() {
        if provider.api_key.as_deref() == Some(REDACTED_API_KEY) {
            provider.api_key = stored.providers.get(name).and_then(|p| p.api_key.clone());
        }
    }
}

/// Whether an `If-Match` header value satisfies the current ETag. Quoted (HTTP
/// standard) and bare values are both accepted; a missing header means "no
/// precondition" and always passes.
fn if_match_satisfied(if_match: Option<&str>, current_etag: &str) -> bool {
    match if_match {
        None => true,
        Some(value) => value.trim_matches('"') == current_etag.trim_matches('"'),
    }
}

/// Reject cross-origin state-changing requests from non-local origins.
///
/// This defends against CSRF on multipart/form-data endpoints that bypass
/// CORS preflight (browsers send multipart POSTs without first asking the
/// server for permission). If either `Origin` or `Referer` is present it
/// must point at localhost; requests with neither header (e.g. curl) are
/// allowed — the browser always sends at least one for cross-origin form
/// submissions.
pub(crate) fn check_local_origin(headers: &HeaderMap) -> Result<(), (StatusCode, Json<ApiError>)> {
    let check = |value: &str| -> bool {
        value.starts_with("http://localhost")
            || value.starts_with("https://localhost")
            || value.starts_with("http://127.0.0.1")
            || value.starts_with("https://127.0.0.1")
    };
    if let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        if !check(origin) {
            return Err((
                StatusCode::FORBIDDEN,
                Json(ApiError::new(
                    "CSRF_BLOCKED",
                    "Cross-origin requests are not allowed",
                )),
            ));
        }
        return Ok(());
    }
    if let Some(referer) = headers
        .get(axum::http::header::REFERER)
        .and_then(|v| v.to_str().ok())
        && !check(referer)
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(ApiError::new(
                "CSRF_BLOCKED",
                "Cross-origin requests are not allowed",
            )),
        ));
    }
    Ok(())
}

pub async fn get_config(
    State(state): State<Arc<AppState>>,
) -> ([(HeaderName, String); 1], Json<papered::AppConfig>) {
    let mut config = state.config.read().await.clone();
    // Compute the ETag from the original config so the version hash
    // matches the stored config (used by update_config for optimistic
    // concurrency). Redaction happens after, only for the JSON body.
    let etag = config_etag(&config);
    redact_api_keys(&mut config);
    ([(header::ETAG, etag)], Json(config))
}

pub async fn update_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(mut config): Json<papered::AppConfig>,
) -> Result<([(HeaderName, String); 1], Json<papered::AppConfig>), (StatusCode, Json<ApiError>)> {
    if let Err(e) = config.validate_strict() {
        return Err(bad_request(ERR_INVALID_CONFIG, e.to_string()));
    }
    let if_match = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok());
    let _guard = state.config_write_lock.lock().await;
    // Optimistic concurrency: reject writes based on a stale read (e.g. the
    // config changed on disk or via another client after the caller's GET).
    let current_etag = config_etag(&state.config.read().await.clone());
    if !if_match_satisfied(if_match, &current_etag) {
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError::new(
                ERR_CONFIG_CONFLICT,
                "Config was modified since it was read; reload and retry.",
            )),
        ));
    }
    // Restore API keys that were redacted by get_config: the frontend never
    // sees the real values, so it sends back the redacted sentinel.
    {
        let stored = state.config.read().await;
        restore_redacted_keys(&mut config, &stored);
    }
    if let Err(e) = config.save() {
        return Err(internal_error(e.to_string()));
    }
    let old = state.config.read().await.clone();
    state.apply_config_update(&config, &old).await;
    // Compute ETag before redaction so it matches the stored config.
    let etag = config_etag(&config);
    redact_api_keys(&mut config);
    Ok(([(header::ETAG, etag)], Json(config)))
}

pub async fn update_embedding_config(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EmbeddingUpdateRequest>,
) -> ApiResult<HealthResponse> {
    if let Some(ref model_key) = req.model_key
        && !state.config.read().await.models.contains_key(model_key)
    {
        return Err(bad_request_msg(format!(
            "Model '{model_key}' not found in models registry"
        )));
    }
    {
        let _guard = state.config_write_lock.lock().await;
        let mut config = state.config.write().await;
        if let Some(ref model_key) = req.model_key {
            config.purposes.embedding.clone_from(model_key);
        }
        config.save().map_err(|e| internal_error(e.to_string()))?;
    }
    state
        .reload_clients()
        .await
        .map_err(|e| internal_error(e.to_string()))?;
    if req.reembed_all {
        let change = match state
            .handle_embedding_model_change(crate::state::EmbeddingRebuildPolicy::ForceRebuild)
            .await
        {
            Ok(change) => change,
            Err(crate::state::EmbeddingChangeError::Probe(e)) => {
                return Ok(Json(HealthResponse::degraded(&state, e.to_string()).await));
            }
            Err(crate::state::EmbeddingChangeError::Reset(e)) => return Err(map_err(e)),
        };

        if change.detected_dim == 0 {
            tracing::warn!("Embedding dimension not yet detected; re-embed skipped");
            return Ok(Json(
                HealthResponse::degraded(
                    &state,
                    "Embedding dimension not yet detected".to_string(),
                )
                .await,
            ));
        }

        if change.rebuilt {
            let pre_count = state.store.paper_count().await.map_err(map_err)?;
            tracing::info!(
                "Re-embed all papers requested: {} papers, {} vectors",
                pre_count,
                state.store.count().await.unwrap_or(0)
            );
            let total_queued = state.reembed_all_now().await;
            tracing::info!("Re-embed all queued {} papers", total_queued);
        }
    }
    let paper_count = state.store.paper_count().await.map_err(map_err)?;
    let vector_count = state.store.count().await.unwrap_or(0);
    Ok(Json(
        HealthResponse::from_state(&state, "ok", paper_count, vector_count).await,
    ))
}

pub async fn import_queue(State(state): State<Arc<AppState>>) -> ApiResult<Vec<ImportQueueItem>> {
    let mut items = Vec::new();
    let processing = state
        .store
        .get_papers_by_status(PaperStatus::Processing.as_str())
        .await
        .map_err(map_err)?;
    let failed = state
        .store
        .get_papers_by_status(PaperStatus::Failed.as_str())
        .await
        .map_err(map_err)?;
    for p in processing.into_iter().chain(failed) {
        items.push(ImportQueueItem {
            paper_id: p.id,
            file_path: p.file_path.unwrap_or_default(),
            status: p.status.to_string(),
        });
    }
    Ok(Json(items))
}

/// Whether first-run setup is incomplete: no provider carries an API key and
/// no provider points at a loopback (key-less, e.g. Ollama) endpoint.
pub(crate) fn compute_setup_status(config: &papered::AppConfig) -> SetupStatusResponse {
    let has_keyed_provider = config
        .providers
        .values()
        .any(|p| p.api_key.as_ref().is_some_and(|k| !k.trim().is_empty()));
    let has_local_provider = config.providers.values().any(|p| {
        p.api_base
            .parse::<axum::http::Uri>()
            .ok()
            .and_then(|u| u.host().map(|h| h.to_string()))
            .as_deref()
            .is_some_and(papered::client::is_loopback_host)
    });
    let needs_setup = !(has_keyed_provider || has_local_provider);
    SetupStatusResponse {
        needs_setup,
        reasons: if needs_setup {
            vec!["no_provider_credentials".to_string()]
        } else {
            Vec::new()
        },
    }
}

pub async fn setup_status(State(state): State<Arc<AppState>>) -> Json<SetupStatusResponse> {
    let config = state.config.read().await;
    Json(compute_setup_status(&config))
}

/// Addresses that must never be probed: unspecified binds and the link-local
/// cloud metadata endpoint. (Loopback/RFC1918 are allowed for local LLMs, but
/// redirects into blocked/reserved ranges are stopped by disabling redirects.)
fn is_blocked_host(host: &str) -> bool {
    let h = host.to_lowercase();
    let h = h.trim_matches(['[', ']']);
    matches!(
        h,
        "0.0.0.0" | "::" | "169.254.169.254" | "::ffff:169.254.169.254"
    )
}

/// Shown when a probe succeeded without forwarding the user's API key.
const KEY_SKIPPED_OK_WARNING: &str =
    "Connection OK. (Key not sent during test \u{2014} actual API calls use your key normally.)";
/// Shown when a probe failed without forwarding the user's API key.
const KEY_SKIPPED_ERR_WARNING: &str =
    "(API key was not sent during this test \u{2014} actual API calls use your key normally.)";

/// Sentinel value substituted for API keys in GET/PUT config responses.
/// Frontend uses `!!p.api_key` for boolean checks, so any non-empty string
/// is sufficient. The `update_config` handler detects this sentinel and
/// restores the original stored key before writing to disk.
const REDACTED_API_KEY: &str = "****redacted****";

pub async fn test_endpoint(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<TestEndpointRequest>,
) -> ApiResult<TestEndpointResponse> {
    let base = req.api_base.trim_end_matches('/');
    let parsed = base
        .parse::<axum::http::Uri>()
        .map_err(|e| bad_request_msg(format!("Invalid api_base URL: {e}")))?;
    let scheme = parsed.scheme_str().unwrap_or("");
    if scheme != "http" && scheme != "https" {
        return Err(bad_request_msg(
            "api_base must use http or https scheme".to_string(),
        ));
    }
    let host = parsed.host().unwrap_or("");
    if host.is_empty() {
        return Err(bad_request_msg("api_base must include a host".to_string()));
    }
    if is_blocked_host(host) {
        return Err(bad_request(
            ERR_SSRF_BLOCKED,
            "refusing to probe a reserved or metadata address".to_string(),
        ));
    }

    // Forward the user-supplied key only to a loopback LLM — never to a LAN or
    // public host, where it would leak. Private/LAN hosts are still probed,
    // just without auth.
    let forward_key = is_loopback_host(host) && req.api_key.is_some();
    if forward_key {
        tracing::warn!("test_endpoint forwarding API key to loopback address");
    } else if req.api_key.is_some() {
        tracing::info!(
            "test_endpoint not forwarding API key to non-loopback endpoint; testing without auth"
        );
    }
    let effective_key: Option<String> = if forward_key {
        req.api_key.clone()
    } else {
        None
    };
    let key_skipped_for_remote = req.api_key.is_some() && !forward_key;

    // Dedicated no-redirect, short-timeout client: a remote must not bounce us
    // into internal/metadata addresses via a 302 (redirect-based SSRF). The
    // shared client keeps redirects for normal LLM calls.
    let probe_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| internal_error(format!("http client: {e}")))?;

    let url = format!("{base}/models");
    let mut request = probe_client.get(&url);
    if let Some(ref key) = effective_key {
        request = request.header("Authorization", format!("Bearer {key}"));
    }
    match request.send().await {
        Ok(resp) if resp.status().is_success() => Ok(Json(TestEndpointResponse {
            status: "ok".to_string(),
            reachable: true,
            http_status: None,
            error: None,
            warning: if key_skipped_for_remote {
                Some(KEY_SKIPPED_OK_WARNING.to_string())
            } else {
                None
            },
        })),
        Ok(resp) => Ok(Json(TestEndpointResponse {
            status: "error".to_string(),
            reachable: true,
            http_status: Some(resp.status().as_u16()),
            error: None,
            warning: if key_skipped_for_remote {
                Some(KEY_SKIPPED_OK_WARNING.to_string())
            } else {
                None
            },
        })),
        Err(e) => Ok(Json(TestEndpointResponse {
            status: "error".to_string(),
            reachable: false,
            http_status: None,
            error: Some(e.to_string()),
            warning: if key_skipped_for_remote {
                Some(KEY_SKIPPED_ERR_WARNING.to_string())
            } else {
                None
            },
        })),
    }
}

pub async fn test_embedding(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<TestEmbeddingRequest>,
) -> ApiResult<TestEmbeddingResponse> {
    // SSRF: validate the host before constructing an HTTP client.
    let base = req.api_base.trim_end_matches('/');
    let parsed = base
        .parse::<axum::http::Uri>()
        .map_err(|e| bad_request_msg(format!("Invalid api_base URL: {e}")))?;
    let scheme = parsed.scheme_str().unwrap_or("");
    if scheme != "http" && scheme != "https" {
        return Err(bad_request_msg(
            "api_base must use http or https scheme".to_string(),
        ));
    }
    let host = parsed.host().unwrap_or("");
    if host.is_empty() {
        return Err(bad_request_msg("api_base must include a host".to_string()));
    }
    if is_blocked_host(host) {
        return Err(bad_request(
            ERR_SSRF_BLOCKED,
            "refusing to connect to a reserved or metadata address".to_string(),
        ));
    }
    // Dedicated no-redirect, short-timeout probe client.
    let probe_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| internal_error(format!("http client: {e}")))?;

    let client = papered::llm::embed::EmbeddingClient::new(
        &req.api_base,
        req.api_key,
        &req.model,
        &papered::config::EmbeddingConfig::default(),
    )
    .map_err(|e| bad_request_msg(e.to_string()))?
    .with_client(probe_client);

    match client.embed_single("test").await {
        Ok(result) => Ok(Json(TestEmbeddingResponse {
            status: "ok".to_string(),
            reachable: true,
            dimension: result.embedding.len(),
            error: None,
        })),
        Err(e) => Ok(Json(TestEmbeddingResponse {
            status: "error".to_string(),
            reachable: false,
            dimension: 0,
            error: Some(e.to_string()),
        })),
    }
}

pub async fn test_reranker(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<TestRerankerRequest>,
) -> ApiResult<TestRerankerResponse> {
    // SSRF: validate the host before constructing an HTTP client.
    let base = req.api_base.trim_end_matches('/');
    let parsed = base
        .parse::<axum::http::Uri>()
        .map_err(|e| bad_request_msg(format!("Invalid api_base URL: {e}")))?;
    let scheme = parsed.scheme_str().unwrap_or("");
    if scheme != "http" && scheme != "https" {
        return Err(bad_request_msg(
            "api_base must use http or https scheme".to_string(),
        ));
    }
    let host = parsed.host().unwrap_or("");
    if host.is_empty() {
        return Err(bad_request_msg("api_base must include a host".to_string()));
    }
    if is_blocked_host(host) {
        return Err(bad_request(
            ERR_SSRF_BLOCKED,
            "refusing to connect to a reserved or metadata address".to_string(),
        ));
    }
    // Dedicated no-redirect, short-timeout probe client.
    let probe_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| internal_error(format!("http client: {e}")))?;

    let endpoint = papered::config::ModelEndpoint {
        api_base: req.api_base,
        api_key: req.api_key,
        model: req.model,
        ..papered::config::ModelEndpoint::placeholder()
    };
    let client = papered::llm::reranker::RerankerClient::new(
        &papered::llm::reranker::RerankerConfig::default(),
        &endpoint,
    )
    .map_err(|e| bad_request_msg(e.to_string()))?
    .with_client(probe_client);

    match client.rerank("test", &["test document".to_string()]).await {
        Ok(_) => Ok(Json(TestRerankerResponse {
            status: "ok".to_string(),
            error: None,
        })),
        Err(e) => Ok(Json(TestRerankerResponse {
            status: "error".to_string(),
            error: Some(e.to_string()),
        })),
    }
}

pub async fn reset_data(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ResetRequest>,
) -> ApiResult<ResetResponse> {
    let data_dir = state.config.read().await.data_dir.clone();

    let mut paths_to_remove: Vec<std::path::PathBuf> =
        vec![data_dir.join("papers"), data_dir.join("covers")];
    if req.all {
        paths_to_remove.push(data_dir.join("logs"));
        paths_to_remove.push(data_dir.join("cache"));
    }

    if !req.force {
        // Preview mode: report what would be removed without deleting anything.
        let paths = paths_to_remove.clone();
        let data_dir_for_preview = data_dir.clone();
        let (total_bytes, preview_paths) = tokio::task::spawn_blocking(move || {
            let mut total = 0u64;
            let mut preview = Vec::new();
            for path in &paths {
                if !path.exists() {
                    continue;
                }
                let size = papered::util::fs::path_size(path);
                total += size;
                // Strip the data_dir prefix so the response doesn't leak
                // the user's home directory path.
                let rel = path
                    .strip_prefix(&data_dir_for_preview)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| path.display().to_string());
                preview.push(rel);
            }
            (total, preview)
        })
        .await
        .map_err(|e| internal_error(e.to_string()))?;

        tracing::info!(
            "Reset preview — would remove {} paths ({} bytes)",
            preview_paths.len(),
            total_bytes
        );
        let item_count = preview_paths.len();
        return Ok(Json(ResetResponse {
            status: "ok".to_string(),
            preview: true,
            removed_paths: preview_paths,
            bytes_freed: total_bytes,
            message: format!(
                "Preview: {} items would be removed ({}). Run with force=true to apply.",
                item_count,
                human_readable_size(total_bytes)
            ),
        }));
    }

    tracing::warn!(
        "Full data reset requested — clearing all papers and vectors (all={})",
        req.all
    );

    // Clear database tables first.
    state.store.clear_all_data().await.map_err(map_err)?;

    // Remove extracted data directories and optionally logs/cache.
    let paths = paths_to_remove.clone();
    let data_dir_for_force = data_dir.clone();
    let (removed_paths, bytes_freed) = tokio::task::spawn_blocking(move || {
        let mut removed = Vec::new();
        let mut total = 0u64;
        for path in &paths {
            if !path.exists() {
                continue;
            }
            let size = path_size(path);
            let result = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            match result {
                Ok(()) => {
                    total += size;
                    let rel = path
                        .strip_prefix(&data_dir_for_force)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| path.display().to_string());
                    removed.push(rel);
                }
                Err(e) => {
                    tracing::warn!("Failed to remove {} during reset: {}", path.display(), e);
                }
            }
        }

        // Recreate empty data subdirectories so the daemon can start cleanly.
        for sub in &["papers", "covers", "logs", "cache"] {
            let _ = std::fs::create_dir_all(data_dir.join(sub));
        }

        (removed, total)
    })
    .await
    .map_err(|e| internal_error(e.to_string()))?;

    tracing::info!(
        "Reset complete — removed {} paths ({} bytes)",
        removed_paths.len(),
        bytes_freed
    );
    let removed_count = removed_paths.len();
    Ok(Json(ResetResponse {
        status: "ok".to_string(),
        preview: false,
        removed_paths,
        bytes_freed,
        message: format!(
            "All data cleared. Removed {} items ({}).",
            removed_count,
            human_readable_size(bytes_freed)
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_match_precondition() {
        // Missing header = no precondition.
        assert!(if_match_satisfied(None, "abc"));
        // Quoted (HTTP standard) and bare values both accepted.
        assert!(if_match_satisfied(Some("\"abc\""), "abc"));
        assert!(if_match_satisfied(Some("abc"), "abc"));
        // Stale tag rejected.
        assert!(!if_match_satisfied(Some("\"stale\""), "abc"));
    }

    #[test]
    fn loopback_host_detection() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("::1"));
        // LAN/private are NOT loopback (key must not be forwarded there).
        assert!(!is_loopback_host("10.0.0.1"));
        assert!(!is_loopback_host("192.168.1.5"));
        assert!(!is_loopback_host("8.8.8.8"));
    }

    #[test]
    fn blocked_host_rejects_unspecified_and_metadata() {
        assert!(is_blocked_host("0.0.0.0"));
        assert!(is_blocked_host("[::]"));
        assert!(is_blocked_host("169.254.169.254"));
        assert!(is_blocked_host("[::ffff:169.254.169.254]"));
        // Legitimate targets are not blocked.
        assert!(!is_blocked_host("127.0.0.1"));
        assert!(!is_blocked_host("api.openai.com"));
        assert!(!is_blocked_host("192.168.1.5"));
    }

    #[test]
    fn test_reranker_request_deserializes_snake_case() {
        let json = serde_json::json!({
            "api_base": "http://127.0.0.1:11434/v1",
            "api_key": null,
            "model": "test-model"
        });
        let req: TestRerankerRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.api_base, "http://127.0.0.1:11434/v1");
        assert!(req.api_key.is_none());
        assert_eq!(req.model, "test-model");
    }
}

#[cfg(test)]
mod setup_status_tests {
    use super::*;
    use papered::config::ProviderConfig;

    fn config_with_provider(api_base: &str, api_key: Option<&str>) -> papered::AppConfig {
        let mut config = papered::AppConfig::default();
        config.providers.insert(
            "test".to_string(),
            ProviderConfig {
                api_base: api_base.to_string(),
                api_key: api_key.map(str::to_string),
            },
        );
        config
    }

    #[test]
    fn default_config_needs_setup() {
        let status = compute_setup_status(&papered::AppConfig::default());
        assert!(status.needs_setup);
        assert_eq!(status.reasons, vec!["no_provider_credentials".to_string()]);
    }

    #[test]
    fn provider_with_key_is_configured() {
        let config = config_with_provider("https://api.example.com/v1", Some("sk-test"));
        assert!(!compute_setup_status(&config).needs_setup);
    }

    #[test]
    fn loopback_provider_without_key_is_configured() {
        let config = config_with_provider("http://127.0.0.1:11434/v1", None);
        assert!(!compute_setup_status(&config).needs_setup);
    }

    #[test]
    fn remote_provider_without_key_needs_setup() {
        let config = config_with_provider("https://api.example.com/v1", None);
        assert!(compute_setup_status(&config).needs_setup);
    }
}
