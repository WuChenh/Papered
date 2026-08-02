use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::api::types::HealthResponse;
use crate::state::AppState;
use papered::StrLabel;
use papered::paper::PaperStatus;

impl HealthResponse {
    /// Build a [`HealthResponse`] from the current daemon state.
    ///
    /// `paper_count` and `vector_count` are passed explicitly so callers can
    /// decide whether store-count errors should propagate or fall back to 0.
    pub async fn from_state(
        state: &Arc<AppState>,
        status: &str,
        paper_count: usize,
        vector_count: usize,
    ) -> Self {
        let config = state.config.read().await;
        let embedding_dimension = state.store.store_dimension().await;
        let embedding_model = config.embedding_fingerprint();
        let model_ready = state.embedding_model_ready.load(Ordering::Relaxed);
        let (embedding_model_status, embedding_model_error) = if model_ready {
            ("ready".to_string(), None)
        } else if config.purposes.embedding.is_empty() {
            (
                "unavailable".to_string(),
                Some(papered::config::unconfigured_model_message("embedding").to_string()),
            )
        } else {
            (
                "unavailable".to_string(),
                Some(
                    "Embedding model not available — waiting for config change or API recovery"
                        .to_string(),
                ),
            )
        };
        let reembed_total = state.reembed_total.load(Ordering::Relaxed);
        let reembed_completed = state.reembed_completed.load(Ordering::Relaxed);
        let reembed_pending = if reembed_total > 0 {
            reembed_total.saturating_sub(reembed_completed)
        } else {
            0
        };
        let processing_count = state
            .store
            .count_papers_by_status(PaperStatus::Processing.as_str())
            .await
            .unwrap_or(0);
        let failed_count = state
            .store
            .count_papers_by_status(PaperStatus::Failed.as_str())
            .await
            .unwrap_or(0);
        Self {
            status: status.to_string(),
            service: "papered-daemon".to_string(),
            paper_count,
            vector_count,
            embedding_dimension,
            embedding_model,
            embedding_model_status,
            embedding_model_error,
            config_needs_restart: state.config_needs_restart.load(Ordering::Relaxed),
            reembed_pending,
            reembed_completed,
            reembed_total,
            processing_count,
            failed_count,
            indexing_paused: state.indexing_paused.load(Ordering::Relaxed),
        }
    }

    /// Degraded response for embedding-model failures: counts are
    /// best-effort, the restart flag is cleared, re-embed counters are zeroed,
    /// and the error is surfaced explicitly.
    pub async fn degraded(state: &Arc<AppState>, error: String) -> Self {
        let mut response = Self::from_state(
            state,
            "degraded",
            state.store.paper_count().await.unwrap_or(0),
            state.store.count().await.unwrap_or(0),
        )
        .await;
        response.embedding_model_error = Some(error);
        response.config_needs_restart = false;
        response.reembed_completed = 0;
        response.reembed_pending = 0;
        response.reembed_total = 0;
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn from_state_returns_expected_defaults() {
        let (state, _tmp) = crate::state::test_app_state().await;
        let response = HealthResponse::from_state(&state, "ok", 0, 0).await;

        assert_eq!(response.status, "ok");
        assert_eq!(response.service, "papered-daemon");
        assert_eq!(response.paper_count, 0);
        assert_eq!(response.vector_count, 0);
        assert_eq!(response.embedding_dimension, None);
        assert_eq!(response.embedding_model_status, "unavailable");
        assert!(response.embedding_model_error.is_some());
        assert!(!response.config_needs_restart);
        assert_eq!(response.reembed_pending, 0);
        assert_eq!(response.reembed_completed, 0);
        assert_eq!(response.reembed_total, 0);
        assert_eq!(response.processing_count, 0);
        assert_eq!(response.failed_count, 0);
        assert!(!response.indexing_paused);
    }

    #[tokio::test]
    async fn from_state_reflects_indexing_paused() {
        let (state, _tmp) = crate::state::test_app_state().await;
        state.indexing_paused.store(true, Ordering::Relaxed);
        let response = HealthResponse::from_state(&state, "ok", 0, 0).await;
        assert!(response.indexing_paused);
    }

    #[tokio::test]
    async fn from_state_reflects_model_ready() {
        let (state, _tmp) = crate::state::test_app_state().await;
        state.embedding_model_ready.store(true, Ordering::Relaxed);
        let response = HealthResponse::from_state(&state, "ok", 0, 0).await;
        assert_eq!(response.status, "ok");
        assert_eq!(response.embedding_model_status, "ready");
        assert!(response.embedding_model_error.is_none());
    }

    #[tokio::test]
    async fn from_state_reports_unconfigured_model() {
        let (state, _tmp) = crate::state::test_app_state().await;
        state.config.write().await.purposes.embedding = String::new();
        let response = HealthResponse::from_state(&state, "ok", 0, 0).await;
        assert_eq!(response.embedding_model_status, "unavailable");
        let expected = papered::config::unconfigured_model_message("embedding");
        assert_eq!(
            response.embedding_model_error.as_deref(),
            Some(expected.as_str())
        );
    }
}
