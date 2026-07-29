//! Neural reranker client for cross-encoder reranking.
//!
//! Supports any OpenAI-compatible `/v1/rerank` endpoint, including:
//! - SiliconFlow: `https://api.siliconflow.cn/v1/rerank`
//! - Qianwen Cloud: `https://dashscope.aliyuncs.com/compatible-api/v1/reranks`
//! - Local self-hosted endpoints (vLLM, TEI, etc.)

use crate::config::ModelEndpoint;
use crate::error::{PaperedError, Result};
use crate::llm::Provider;
use crate::llm::metrics::{CallKind, MetricsSink, TokenUsage, truncate_error};
use crate::llm::rate_limiter::RateLimiter;
use serde::{Deserialize, Serialize};

/// Configuration for the neural reranker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RerankerConfig {
    /// Maximum number of results to return from reranker.
    pub top_n: usize,
    /// Request timeout in seconds.
    pub timeout_secs: u64,
    /// Optional instruction for instruction-aware rerankers (Qwen3-VL-Reranker).
    pub instruction: Option<String>,
    /// Optional timeout: if the reranker does not respond within this many
    /// seconds, reranking is skipped and the caller falls back to the raw ANN
    /// ordering. None = no skip timeout (only `timeout_secs` applies).
    #[serde(default)]
    pub skip_timeout_secs: Option<u64>,
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            top_n: 20,
            timeout_secs: 30,
            instruction: None,
            skip_timeout_secs: None,
        }
    }
}

/// A single reranked result from the API.
#[derive(Debug, Clone, Deserialize)]
pub struct RerankResult {
    /// Index of the document in the original input list.
    pub index: usize,
    /// Relevance score (0.0–1.0, higher is better).
    pub relevance_score: f32,
}

/// Full API response.
#[derive(Debug, Clone, Deserialize)]
struct RerankResponse {
    results: Vec<RerankResult>,
}

/// Neural reranker HTTP client.
#[derive(Clone)]
pub struct RerankerClient {
    client: reqwest::Client,
    api_base: String,
    api_key: Option<String>,
    model: String,
    top_n: usize,
    instruction: Option<String>,
    /// Skip timeout: abort the rerank request after this long so the caller
    /// can fall back to raw ANN results. `None` waits indefinitely.
    skip_timeout: Option<std::time::Duration>,
    rate_limiter: Option<RateLimiter>,
    /// Optional metrics sink injected by the daemon; records latency for
    /// every rerank call. `None` disables recording.
    metrics: Option<MetricsSink>,
}

impl std::fmt::Debug for RerankerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RerankerClient")
            .field("api_base", &self.api_base)
            .field("model", &self.model)
            .field("top_n", &self.top_n)
            .finish_non_exhaustive()
    }
}

impl RerankerClient {
    /// Create a new reranker client from configuration.
    pub fn new(config: &RerankerConfig, endpoint: &ModelEndpoint) -> Result<Self> {
        let client = crate::client::build_http_client(config.timeout_secs, None)?;

        Ok(Self {
            client,
            api_base: endpoint.api_base.clone(),
            api_key: endpoint.api_key.clone(),
            model: endpoint.model.clone(),
            top_n: config.top_n,
            instruction: config.instruction.clone(),
            skip_timeout: config.skip_timeout_secs.map(std::time::Duration::from_secs),
            rate_limiter: None,
            metrics: None,
        })
    }

    /// Replace the HTTP client with a custom one (e.g. SSRF-hardened probe).
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Attach a metrics sink recording latency for every rerank call.
    #[must_use]
    pub fn with_metrics(mut self, sink: MetricsSink) -> Self {
        self.metrics = Some(sink);
        self
    }

    /// Rerank a list of text documents against a text query.
    pub async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<RerankResult>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }

        if let Some(ref limiter) = self.rate_limiter {
            let estimated_tokens: usize = crate::util::estimate_tokens(query)
                + documents
                    .iter()
                    .map(|d| crate::util::estimate_tokens(d))
                    .sum::<usize>();
            let _permit = limiter.acquire(estimated_tokens).await?;
        }

        let provider = Provider::from_url(&self.api_base);
        let url = provider.build_url(&self.api_base, provider.rerank_path());

        let mut body = serde_json::json!({
            "model": self.model,
            "query": query,
            "documents": documents,
            "top_n": self.top_n.min(documents.len()),
        });

        if let Some(ref instruction) = self.instruction {
            body["instruction"] = serde_json::Value::String(instruction.clone());
        }

        let t0 = std::time::Instant::now();
        let request = crate::client::post_json(
            &self.client,
            &url,
            &body,
            self.api_key.as_deref(),
            |status, msg| {
                PaperedError::Reranker(format!(
                    "Reranker HTTP {}: {msg}",
                    crate::client::status_text(status)
                ))
            },
        );
        let result: Result<RerankResponse> = match self.skip_timeout {
            Some(d) => match tokio::time::timeout(d, request).await {
                Ok(r) => r,
                Err(_) => Err(PaperedError::Reranker(format!(
                    "Reranker timed out after {}s (skip_timeout_secs)",
                    d.as_secs()
                ))),
            },
            None => request.await,
        };
        let (success, error) = match &result {
            Ok(_) => (true, None),
            Err(e) => (false, Some(truncate_error(&e.to_string()))),
        };
        crate::llm::metrics::record_metric(
            self.metrics.as_ref(),
            CallKind::Rerank,
            self.model.clone(),
            TokenUsage::default(),
            t0.elapsed(),
            success,
            error,
        );
        let json = result?;
        Ok(json.results)
    }
}
