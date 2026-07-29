use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use papered::error::ApiError;
use std::sync::Arc;

use super::types::{ApiResult, CreatePromptRequest, map_err, not_found};
use crate::AppState;

pub async fn list_prompts(State(state): State<Arc<AppState>>) -> ApiResult<Vec<papered::Prompt>> {
    let prompts = state.store.list_prompts().await.map_err(map_err)?;
    Ok(Json(prompts))
}

pub async fn create_prompt(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreatePromptRequest>,
) -> ApiResult<papered::Prompt> {
    let mut prompt = papered::Prompt::new(req.name, req.system_prompt);
    prompt.description = req.description;
    prompt.temperature = req.temperature;
    state.store.insert_prompt(&prompt).await.map_err(map_err)?;
    state
        .rag_engine
        .read()
        .await
        .invalidate_prompt_cache()
        .await;
    Ok(Json(prompt))
}

pub async fn get_prompt(
    State(state): State<Arc<AppState>>,
    Path(prompt_id): Path<String>,
) -> ApiResult<papered::Prompt> {
    let prompt = state
        .store
        .get_prompt(&prompt_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| not_found(format!("Prompt not found: {prompt_id}")))?;
    Ok(Json(prompt))
}

pub async fn update_prompt(
    State(state): State<Arc<AppState>>,
    Path(prompt_id): Path<String>,
    Json(req): Json<CreatePromptRequest>,
) -> ApiResult<papered::Prompt> {
    let mut prompt = state
        .store
        .get_prompt(&prompt_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| not_found(format!("Prompt not found: {prompt_id}")))?;
    prompt.name = req.name;
    prompt.description = req.description;
    prompt.system_prompt = req.system_prompt;
    prompt.temperature = req.temperature;
    state.store.update_prompt(&prompt).await.map_err(map_err)?;
    state
        .rag_engine
        .read()
        .await
        .invalidate_prompt_cache()
        .await;
    Ok(Json(prompt))
}

pub async fn delete_prompt(
    State(state): State<Arc<AppState>>,
    Path(prompt_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state
        .store
        .delete_prompt(&prompt_id)
        .await
        .map_err(map_err)?;
    state
        .rag_engine
        .read()
        .await
        .invalidate_prompt_cache()
        .await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn set_default_prompt(
    State(state): State<Arc<AppState>>,
    Path(prompt_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state
        .store
        .set_default_prompt(&prompt_id)
        .await
        .map_err(map_err)?;
    state
        .rag_engine
        .read()
        .await
        .invalidate_prompt_cache()
        .await;
    Ok(StatusCode::NO_CONTENT)
}
