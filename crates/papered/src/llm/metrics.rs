//! Usage and latency metrics for LLM/embedding/rerank provider calls.
//!
//! Every provider call routed through [`crate::llm::client::LlmClient`],
//! [`crate::llm::embed::EmbeddingClient`], or [`crate::llm::reranker::RerankerClient`]
//! can emit one
//! [`LlmCallMetric`] to an injected [`MetricsSink`]. The daemon wires the sink to
//! [`store_metrics_sink`], which persists metrics into the `llm_call_metrics`
//! table. Recording is best-effort: failures are logged at warn level and never
//! affect the call path.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Maximum length of the stored error text (long provider bodies are truncated).
const MAX_ERROR_CHARS: usize = 200;

/// Category of an instrumented provider call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    /// Text-only chat completion.
    Chat,
    /// Text embedding request.
    Embedding,
    /// Rerank request.
    Rerank,
    /// Chat completion carrying image inputs.
    Vision,
}

impl CallKind {
    /// Stable string form stored in the `kind` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embedding => "embedding",
            Self::Rerank => "rerank",
            Self::Vision => "vision",
        }
    }
}

/// Token usage reported by the provider, when present in the response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

/// One completed provider call (success or failure).
#[derive(Debug, Clone)]
pub struct LlmCallMetric {
    pub kind: CallKind,
    pub model: String,
    pub usage: TokenUsage,
    pub latency_ms: u64,
    pub success: bool,
    pub error: Option<String>,
}

/// Aggregated metrics for one (kind, model) group, as returned by
/// [`crate::store::vector::VectorStore::llm_call_metrics_summary`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmCallMetricGroup {
    pub kind: String,
    pub model: String,
    pub calls: u64,
    pub failures: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub avg_latency_ms: f64,
}

/// Non-blocking sink injected into provider clients. The callback must be
/// cheap and must never panic — it runs on the provider call path.
pub type MetricsSink = Arc<dyn Fn(LlmCallMetric) + Send + Sync>;

/// Record one provider call metric through the sink when one is attached.
/// Never fails the call path.
pub fn record_metric(
    metrics: Option<&MetricsSink>,
    kind: CallKind,
    model: String,
    usage: TokenUsage,
    latency: std::time::Duration,
    success: bool,
    error: Option<String>,
) {
    let Some(sink) = metrics else {
        return;
    };
    sink(LlmCallMetric {
        kind,
        model,
        usage,
        latency_ms: latency.as_millis() as u64,
        success,
        error,
    });
}

/// Build a sink that persists metrics into the store. Each record is written
/// from a spawned task so the provider call path never blocks on the database;
/// write failures are logged at warn level and swallowed.
pub fn store_metrics_sink(store: &Arc<dyn crate::store::vector::VectorStore>) -> MetricsSink {
    let store = store.clone();
    Arc::new(move |metric| {
        let store = store.clone();
        tokio::spawn(async move {
            if let Err(e) = store.insert_llm_call_metric(&metric).await {
                tracing::warn!("Failed to record LLM call metric: {e}");
            }
        });
    })
}

/// Truncate an error message to a short, single-line form fit for the
/// `error` column.
pub fn truncate_error(message: &str) -> String {
    let single_line = message.replace('\n', " ");
    if single_line.chars().count() <= MAX_ERROR_CHARS {
        return single_line;
    }
    let truncated: String = single_line.chars().take(MAX_ERROR_CHARS).collect();
    format!("{truncated}…")
}

/// Fetch both all-time and last-24-hour metrics summaries from the store.
pub async fn fetch_both_metrics(
    store: &dyn crate::store::vector::VectorStore,
) -> crate::error::Result<(Vec<LlmCallMetricGroup>, Vec<LlmCallMetricGroup>)> {
    let all_time = store.llm_call_metrics_summary(None).await?;
    let since = chrono::Utc::now() - chrono::Duration::hours(24);
    let last_24h = store.llm_call_metrics_summary(Some(since)).await?;
    Ok((all_time, last_24h))
}
