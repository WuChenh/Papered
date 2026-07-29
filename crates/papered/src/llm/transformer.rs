//! Generic text transformer using an LLM client.
//!
//! Shared infrastructure for the unified query enhancement layer.

use crate::config::ModelEndpoint;
use crate::error::Result;
use crate::llm::client::LlmClient;
use crate::llm::rate_limiter::RateLimiter;

/// A generic text transformer that wraps an LLM client.
pub struct TextTransformer {
    client: LlmClient,
    temperature: f32,
    max_tokens: usize,
}

impl TextTransformer {
    pub fn with_rate_limiter(
        endpoint: &ModelEndpoint,
        temperature: f32,
        max_tokens: usize,
        rate_limiter: Option<RateLimiter>,
    ) -> Result<Self> {
        let client = LlmClient::from_config(endpoint, rate_limiter)?;
        Ok(Self {
            client,
            temperature,
            max_tokens,
        })
    }

    /// Attach a metrics sink to the underlying LLM client.
    pub fn set_metrics(&mut self, sink: crate::llm::metrics::MetricsSink) {
        self.client.set_metrics(sink);
    }

    /// Transform the prompt using the given system prompt.
    pub async fn transform(&self, system: &str, prompt: &str) -> Result<String> {
        if prompt.trim().is_empty() {
            return Ok(prompt.trim().to_string());
        }
        self.client
            .generate(system, prompt, self.max_tokens, self.temperature)
            .await
    }
}
