use axum::{extract::State, response::Json};
use std::sync::Arc;

use super::types::{ApiResult, RagRequest, RagResponse, map_err};
use crate::AppState;
use papered::llm::rag::RagSourceView;

pub async fn ask_rag(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RagRequest>,
) -> ApiResult<RagResponse> {
    let answer = state
        .rag_engine
        .read()
        .await
        .ask(
            &req.question,
            req.search_method,
            req.prompt_id.as_deref(),
            req.use_enhancement,
            req.paper_id.as_deref(),
        )
        .await
        .map_err(map_err)?;
    let sources = answer
        .sources
        .into_iter()
        .map(RagSourceView::from)
        .collect();
    Ok(Json(RagResponse {
        answer: answer.answer,
        sources,
        search_method: answer.search_method_used,
    }))
}
