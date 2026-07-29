//! `GET /api/v1/metrics` — aggregated LLM call usage and latency stats.

use axum::{extract::State, response::Json};
use serde::Serialize;
use std::sync::Arc;

use super::types::{ApiResult, map_err};
use crate::AppState;
use papered::llm::metrics::LlmCallMetricGroup;

/// Aggregated LLM call metrics: one group per (kind, model), both all-time
/// and restricted to the last 24 hours.
#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    pub all_time: Vec<LlmCallMetricGroup>,
    pub last_24h: Vec<LlmCallMetricGroup>,
}

pub async fn metrics(State(state): State<Arc<AppState>>) -> ApiResult<MetricsResponse> {
    let (all_time, last_24h) = papered::llm::metrics::fetch_both_metrics(&*state.store)
        .await
        .map_err(map_err)?;
    Ok(Json(MetricsResponse { all_time, last_24h }))
}
