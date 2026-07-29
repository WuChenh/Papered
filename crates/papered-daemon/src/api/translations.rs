//! Translation API endpoints — translate paper content and manage translations.

use axum::extract::{Path, Query, State};
use axum::response::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use papered::StrLabel;
use papered::llm::client::LlmClient;
use papered::llm::translation::{content_hash, translate_text};
use papered::store::vector::TranslationInfo;

use super::types::{
    ApiResult, ApiStatusResult, bad_request_msg, map_err, not_found, validate_paper_id,
};
use crate::AppState;

// ------------------------------------------------------------------
// Request / response types
// ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TranslateRequest {
    pub target_language: String,
    pub content_type: String, // "title", "abstract", "section", "figure_caption"
    pub content_ref: String,  // section_type, figure_id, or "_self"
}

#[derive(Debug, Deserialize)]
pub struct BatchTranslateRequest {
    pub target_language: String,
    /// Optional explicit list of items. When empty, the server auto-collects
    /// all translatable content (title + abstract + sections + figure captions).
    #[serde(default)]
    pub items: Vec<BatchTranslateItem>,
}

#[derive(Debug, Deserialize)]
pub struct BatchTranslateItem {
    pub content_type: String,
    pub content_ref: String,
}

#[derive(Debug, Serialize)]
pub struct TranslateResponse {
    pub content_type: String,
    pub content_ref: String,
    pub translated_text: String,
    pub cached: bool,
}

#[derive(Debug, Serialize)]
pub struct BatchTranslateResponse {
    pub translations: Vec<TranslateResponse>,
}

#[derive(Debug, Serialize)]
pub struct TranslationsListResponse {
    pub translations: Vec<TranslationInfo>,
}

#[derive(Debug, Deserialize)]
pub struct TranslationQuery {
    pub lang: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TranslationSearchQuery {
    pub q: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    20
}

#[derive(Debug, Serialize)]
pub struct TranslationSearchResponse {
    pub results: Vec<TranslationInfo>,
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

/// Build an LlmClient for translation using the config's translation model.
async fn build_translation_client(
    state: &AppState,
) -> Result<LlmClient, (axum::http::StatusCode, Json<papered::error::ApiError>)> {
    let config = state.config.read().await;
    let endpoint = config.resolve_translation_model().map_err(map_err)?;
    let rate_limiter = papered::llm::rate_limiter::RateLimiter::for_endpoint(&endpoint);
    let mut client = LlmClient::from_config(&endpoint, rate_limiter).map_err(map_err)?;
    client.set_metrics(papered::llm::metrics::store_metrics_sink(&state.store));
    Ok(client)
}

/// Resolve the original text for a given (content_type, content_ref) from a paper.
async fn resolve_source_text(
    state: &AppState,
    paper: &papered::paper::Paper,
    content_type: &str,
    content_ref: &str,
) -> Result<String, (axum::http::StatusCode, Json<papered::error::ApiError>)> {
    match content_type {
        "title" => Ok(paper.title.clone()),
        "abstract" => {
            // Try sections first (abstract stored as a section after full indexing).
            let sections = state.store.get_sections(&paper.id).await.map_err(map_err)?;
            let abstract_section = sections
                .sections
                .iter()
                .find(|s| s.section_type.as_str() == "abstract");
            if let Some(sec) = abstract_section {
                Ok(sec.content.clone())
            } else {
                // Fall back to paper metadata abstract.
                Ok(paper.abstract_text.clone().unwrap_or_default())
            }
        }
        "section" => {
            let sections = state.store.get_sections(&paper.id).await.map_err(map_err)?;
            let sec = sections
                .sections
                .iter()
                .find(|s| s.section_type.as_str() == content_ref);
            match sec {
                Some(s) => Ok(s.content.clone()),
                None => Err(not_found(format!("Section not found: {content_ref}"))),
            }
        }
        "figure_caption" => {
            let figures = state.store.get_figures(&paper.id).await.map_err(map_err)?;
            let fig = figures.iter().find(|f| f.id == content_ref);
            match fig {
                Some(f) => Ok(f.caption.clone().unwrap_or_default()),
                None => Err(not_found(format!("Figure not found: {content_ref}"))),
            }
        }
        _ => Err(bad_request_msg(format!(
            "Unknown content_type: {content_type}"
        ))),
    }
}

// ------------------------------------------------------------------
// Handlers
// ------------------------------------------------------------------

/// POST /papers/:id/translate — translate a single content piece.
pub async fn translate_paper(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<TranslateRequest>,
) -> ApiResult<TranslateResponse> {
    let paper = super::types::require_paper(&state, &id).await?;

    if req.target_language.is_empty() {
        return Err(bad_request_msg("target_language is required"));
    }
    if req.content_type.is_empty() {
        return Err(bad_request_msg("content_type is required"));
    }

    let source_text =
        resolve_source_text(&state, &paper, &req.content_type, &req.content_ref).await?;
    if source_text.trim().is_empty() {
        return Err(bad_request_msg(
            "Source text is empty — nothing to translate",
        ));
    }

    let src_hash = content_hash(&source_text);

    // Check cache.
    if let Some(existing) = state
        .store
        .get_translation(
            &paper.id,
            &req.content_type,
            &req.content_ref,
            &req.target_language,
        )
        .await
        .map_err(map_err)?
        && existing.source_hash == src_hash
    {
        return Ok(Json(TranslateResponse {
            content_type: req.content_type,
            content_ref: req.content_ref,
            translated_text: existing.translated_text,
            cached: true,
        }));
    }

    // Translate via LLM.
    let client = build_translation_client(&state).await?;
    let translated = translate_text(
        &client,
        &source_text,
        &req.content_type,
        &req.target_language,
    )
    .await
    .map_err(map_err)?;

    // Store the translation.
    let info = TranslationInfo {
        id: 0,
        paper_id: paper.id.clone(),
        content_type: req.content_type.clone(),
        content_ref: req.content_ref.clone(),
        source_hash: src_hash,
        target_language: req.target_language.clone(),
        translated_text: translated.clone(),
        model: Some(client.model_name().to_string()),
        created_at: String::new(),
        updated_at: String::new(),
    };
    state
        .store
        .upsert_translation(&info)
        .await
        .map_err(map_err)?;

    Ok(Json(TranslateResponse {
        content_type: req.content_type,
        content_ref: req.content_ref,
        translated_text: translated,
        cached: false,
    }))
}

/// POST /papers/:id/translate/batch — batch translate all content of a paper.
pub async fn batch_translate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<BatchTranslateRequest>,
) -> ApiResult<BatchTranslateResponse> {
    let paper = super::types::require_paper(&state, &id).await?;

    if req.target_language.is_empty() {
        return Err(bad_request_msg("target_language is required"));
    }

    let client = build_translation_client(&state).await?;
    let mut responses = Vec::new();

    // Collect items to translate, grouped by content_type.
    let items_to_translate: Vec<(String, String, String)> = if req.items.is_empty() {
        // Auto-collect all translatable content.
        let mut items = Vec::new();

        // Title.
        if !paper.title.is_empty() {
            items.push((
                "title".to_string(),
                "_self".to_string(),
                paper.title.clone(),
            ));
        }

        // Abstract.
        let sections = state.store.get_sections(&paper.id).await.map_err(map_err)?;
        let abstract_section = sections
            .sections
            .iter()
            .find(|s| s.section_type.as_str() == "abstract");
        if let Some(sec) = abstract_section {
            if !sec.content.is_empty() {
                items.push((
                    "abstract".to_string(),
                    "_self".to_string(),
                    sec.content.clone(),
                ));
            }
        } else if let Some(ref abs) = paper.abstract_text
            && !abs.is_empty()
        {
            items.push(("abstract".to_string(), "_self".to_string(), abs.clone()));
        }

        // Sections (skip abstract since we handled it above).
        for sec in &sections.sections {
            if sec.section_type.as_str() == "abstract" {
                continue;
            }
            if !sec.content.is_empty() {
                items.push((
                    "section".to_string(),
                    sec.section_type.to_string(),
                    sec.content.clone(),
                ));
            }
        }

        // Figure captions.
        let figures = state.store.get_figures(&paper.id).await.map_err(map_err)?;
        for fig in &figures {
            if let Some(ref caption) = fig.caption
                && !caption.is_empty()
            {
                items.push((
                    "figure_caption".to_string(),
                    fig.id.clone(),
                    caption.clone(),
                ));
            }
        }

        items
    } else {
        // Use explicit items list.
        let mut items = Vec::new();
        for item in &req.items {
            let text =
                resolve_source_text(&state, &paper, &item.content_type, &item.content_ref).await?;
            if !text.is_empty() {
                items.push((item.content_type.clone(), item.content_ref.clone(), text));
            }
        }
        items
    };

    // Process each item, checking cache first.
    for (content_type, content_ref, source_text) in &items_to_translate {
        let src_hash = content_hash(source_text);

        // Check cache.
        if let Some(existing) = state
            .store
            .get_translation(&paper.id, content_type, content_ref, &req.target_language)
            .await
            .map_err(map_err)?
            && existing.source_hash == src_hash
        {
            responses.push(TranslateResponse {
                content_type: content_type.clone(),
                content_ref: content_ref.clone(),
                translated_text: existing.translated_text,
                cached: true,
            });
            continue;
        }

        // Translate.
        let translated = translate_text(&client, source_text, content_type, &req.target_language)
            .await
            .map_err(map_err)?;

        let info = TranslationInfo {
            id: 0,
            paper_id: paper.id.clone(),
            content_type: content_type.clone(),
            content_ref: content_ref.clone(),
            source_hash: src_hash,
            target_language: req.target_language.clone(),
            translated_text: translated.clone(),
            model: Some(client.model_name().to_string()),
            created_at: String::new(),
            updated_at: String::new(),
        };
        state
            .store
            .upsert_translation(&info)
            .await
            .map_err(map_err)?;

        responses.push(TranslateResponse {
            content_type: content_type.clone(),
            content_ref: content_ref.clone(),
            translated_text: translated,
            cached: false,
        });
    }

    Ok(Json(BatchTranslateResponse {
        translations: responses,
    }))
}

/// GET /papers/:id/translations — list translations for a paper.
pub async fn get_translations(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<TranslationQuery>,
) -> ApiResult<TranslationsListResponse> {
    validate_paper_id(&id)?;
    let lang = query.lang.unwrap_or_else(|| {
        // Will be overridden below; we need the config default.
        String::new()
    });

    let target_lang = if lang.is_empty() {
        let config = state.config.read().await;
        config.translation.target_language.clone()
    } else {
        lang
    };

    let translations = state
        .store
        .get_translations(&id, &target_lang)
        .await
        .map_err(map_err)?;

    Ok(Json(TranslationsListResponse { translations }))
}

/// GET /translations/search — full-text search across translations.
pub async fn search_translations(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TranslationSearchQuery>,
) -> ApiResult<TranslationSearchResponse> {
    if query.q.trim().is_empty() {
        return Err(bad_request_msg("Query parameter 'q' is required"));
    }

    let limit = query.limit.min(100);
    let results = state
        .store
        .search_translations(&query.q, limit)
        .await
        .map_err(map_err)?;

    Ok(Json(TranslationSearchResponse { results }))
}

/// DELETE /papers/:id/translations — delete all translations for a paper.
pub async fn delete_translations(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiStatusResult {
    validate_paper_id(&id)?;
    state
        .store
        .delete_translations(&id)
        .await
        .map_err(map_err)?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
