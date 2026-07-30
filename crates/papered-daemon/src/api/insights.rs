//! Insight generation endpoint — a one-shot LLM "take" (insight / critique /
//! inspiration) on a paper, returned to the caller without persistence. The
//! web UI drops the draft into the note textarea; saving it as a comment is
//! the user's decision.

use axum::{
    extract::{Path, State},
    response::Json,
};
use serde::Serialize;
use std::sync::Arc;

use papered::StrLabel;
use papered::llm::client::LlmClient;
use papered::llm::insight::insight_prompts;

use super::types::{ApiResult, map_err, require_paper};
use crate::AppState;

/// Generation budget: three short paragraphs, well under this cap.
const INSIGHT_MAX_TOKENS: usize = 640;
/// Slightly creative, still grounded.
const INSIGHT_TEMPERATURE: f32 = 0.7;

#[derive(Debug, Serialize)]
pub struct InsightResponse {
    pub insight: String,
}

/// `POST /api/v1/papers/{id}/insights` — generate an insight/critique/
/// inspiration draft for the paper. Uses the `rag` purpose model; nothing is
/// stored server-side.
pub async fn generate_insight(
    State(state): State<Arc<AppState>>,
    Path(paper_id): Path<String>,
) -> ApiResult<InsightResponse> {
    let paper = require_paper(&state, &paper_id).await?;

    // After full indexing the abstract lives in a section; fall back to the
    // metadata-only abstract.
    let abstract_text = match state.store.get_sections(&paper.id).await {
        Ok(sections) => sections
            .sections
            .iter()
            .find(|s| s.section_type.as_str() == "abstract")
            .map(|s| s.content.clone())
            .or_else(|| paper.abstract_text.clone()),
        Err(_) => paper.abstract_text.clone(),
    };

    let (system, user) = insight_prompts(
        &paper.title,
        &paper.authors,
        paper.venue.as_deref(),
        paper.published_date.as_deref(),
        abstract_text.as_deref(),
    );

    let config = state.config.read().await;
    let endpoint = config
        .resolve_model(&config.purposes.rag)
        .map_err(map_err)?;
    let rate_limiter = papered::llm::rate_limiter::RateLimiter::for_endpoint(&endpoint);
    let mut client = LlmClient::from_config(&endpoint, rate_limiter).map_err(map_err)?;
    client.set_metrics(papered::llm::metrics::store_metrics_sink(&state.store));
    drop(config);

    let insight = client
        .generate(&system, &user, INSIGHT_MAX_TOKENS, INSIGHT_TEMPERATURE)
        .await
        .map_err(map_err)?;
    Ok(Json(InsightResponse {
        insight: insight.trim().to_string(),
    }))
}
