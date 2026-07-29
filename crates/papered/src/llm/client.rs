//! Shared LLM client for Ollama and OpenAI-compatible APIs.
//!
//! Used by the unified query enhancement layer and other optional features.

use crate::error::{PaperedError, Result};
use crate::llm::Provider;
use crate::llm::metrics::{CallKind, MetricsSink, TokenUsage, truncate_error};
use crate::llm::rate_limiter::RateLimiter;
use reqwest::Client;

const JSON_OBJECT_TYPE: &str = "json_object";
const REASONING_CONTENT: &str = "reasoning_content";
const FINISH_REASON: &str = "finish_reason";
const FINISH_LENGTH: &str = "length";

/// A lightweight LLM client that routes to Ollama or OpenAI-compatible endpoints.
pub struct LlmClient {
    http: Client,
    api_base: String,
    api_key: Option<String>,
    model: String,
    provider: Provider,
    rate_limiter: Option<RateLimiter>,
    /// Extra parameters merged into every request body (provider-specific).
    /// Set via `with_extra_body`.
    extra_body: Option<serde_json::Value>,
    /// Reasoning effort level ("low", "medium", "high") for models that support it.
    /// Set via `with_reasoning_effort`.
    reasoning_effort: Option<String>,
    /// Per-provider max_tokens cap. Requests exceeding this are silently clamped.
    /// Configured per-endpoint; e.g. Qianwen Cloud models enforce 65536.
    output_token_limit: Option<usize>,
    /// Optional metrics sink injected by the daemon; records usage + latency
    /// for every provider call. `None` disables recording.
    metrics: Option<MetricsSink>,
}

impl LlmClient {
    pub fn new(
        api_base: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let api_base = api_base.into();
        let provider = Provider::from_url(&api_base);
        let http = crate::client::build_http_client(180, None)?;

        Ok(Self {
            http,
            api_base,
            api_key,
            model: model.into(),
            provider,
            rate_limiter: None,
            extra_body: None,
            reasoning_effort: None,
            output_token_limit: None,
            metrics: None,
        })
    }

    #[must_use]
    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    #[must_use]
    pub fn with_extra_body(mut self, extra: serde_json::Value) -> Self {
        self.extra_body = Some(extra);
        self
    }

    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: String) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    #[must_use]
    pub fn with_output_token_limit(mut self, cap: usize) -> Self {
        self.output_token_limit = Some(cap);
        self
    }

    /// Attach a metrics sink recording usage + latency for every provider call.
    #[must_use]
    pub fn with_metrics(mut self, sink: MetricsSink) -> Self {
        self.metrics = Some(sink);
        self
    }

    /// Attach (or replace) the metrics sink after construction.
    pub fn set_metrics(&mut self, sink: MetricsSink) {
        self.metrics = Some(sink);
    }

    pub fn from_config(
        endpoint: &crate::config::ModelEndpoint,
        rate_limiter: Option<crate::llm::rate_limiter::RateLimiter>,
    ) -> Result<Self> {
        let mut client = Self::new(
            &endpoint.api_base,
            endpoint.api_key.clone(),
            &endpoint.model,
        )?;
        if let Some(rl) = rate_limiter {
            client = client.with_rate_limiter(rl);
        }
        if let Some(ref extra) = endpoint.extra_body {
            client = client.with_extra_body(extra.clone());
        }
        if let Some(ref effort) = endpoint.reasoning_effort {
            client = client.with_reasoning_effort(effort.clone());
        }
        if let Some(cap) = endpoint.max_output_tokens {
            client = client.with_output_token_limit(cap);
        }
        Ok(client)
    }

    /// Return the model identifier this client was configured with.
    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Generate text from a single-turn prompt (system + user).
    pub async fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_tokens: usize,
        temperature: f32,
    ) -> Result<String> {
        self.generate_with_images(system_prompt, user_prompt, &[], max_tokens, temperature)
            .await
    }

    /// Generate with JSON output format (`response_format: {"type": "json_object"}`).
    pub async fn generate_json(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        max_tokens: usize,
        temperature: f32,
    ) -> Result<String> {
        let response_format = Some(serde_json::json!({"type": JSON_OBJECT_TYPE}));
        self.generate_dispatch(
            system_prompt,
            user_prompt,
            &[],
            max_tokens,
            temperature,
            response_format,
        )
        .await
    }

    /// Generate text from a single-turn prompt with optional base64-encoded images.
    /// Images are passed via OpenAI-compatible vision API (type: image_url with data URI).
    pub async fn generate_with_images(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        images_base64: &[String],
        max_tokens: usize,
        temperature: f32,
    ) -> Result<String> {
        self.generate_dispatch(
            system_prompt,
            user_prompt,
            images_base64,
            max_tokens,
            temperature,
            None,
        )
        .await
    }

    /// Shared dispatch for all generation entry points: acquires the rate
    /// limiter exactly once, then routes to the provider-specific transport.
    async fn generate_dispatch(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        images_base64: &[String],
        max_tokens: usize,
        temperature: f32,
        response_format: Option<serde_json::Value>,
    ) -> Result<String> {
        // Apply rate limiting before the request
        if let Some(ref limiter) = self.rate_limiter {
            let estimated_tokens = crate::util::estimate_tokens(system_prompt)
                + crate::util::estimate_tokens(user_prompt)
                + max_tokens;
            let _permit = limiter.acquire(estimated_tokens).await?;
        }

        match self.provider {
            Provider::Ollama => {
                let mut prompt = user_prompt.to_string();
                for (i, _img) in images_base64.iter().enumerate() {
                    use std::fmt::Write;
                    let _ = write!(prompt, "\n[IMAGE {}]", i + 1);
                }
                self.generate_ollama(
                    system_prompt,
                    &prompt,
                    images_base64,
                    max_tokens,
                    temperature,
                    response_format,
                )
                .await
            }
            Provider::OpenAiCompatible | Provider::Qianwen => {
                self.generate_openai_chat(
                    system_prompt,
                    user_prompt,
                    images_base64,
                    max_tokens,
                    temperature,
                    response_format,
                )
                .await
            }
        }
    }

    async fn generate_ollama(
        &self,
        system_prompt: &str,
        prompt: &str,
        images_base64: &[String],
        max_tokens: usize,
        temperature: f32,
        response_format: Option<serde_json::Value>,
    ) -> Result<String> {
        let path = self.provider.generate_path().ok_or_else(|| {
            PaperedError::LlmGeneration(
                "generate_path is only valid for Provider::Ollama".to_string(),
            )
        })?;
        let url = self.provider.build_url(&self.api_base, path);

        let mut body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
            "system": system_prompt,
            "options": {
                "temperature": temperature,
                "num_predict": max_tokens,
            }
        });
        if !images_base64.is_empty() {
            body["images"] = serde_json::json!(images_base64);
        }
        if response_format
            .as_ref()
            .is_some_and(|f| f.get("type").and_then(|v| v.as_str()) == Some(JSON_OBJECT_TYPE))
        {
            body["format"] = "json".into();
        }

        #[derive(serde::Deserialize)]
        struct GenerateResponse {
            response: String,
            prompt_eval_count: Option<u32>,
            eval_count: Option<u32>,
        }

        let kind = if images_base64.is_empty() {
            CallKind::Chat
        } else {
            CallKind::Vision
        };
        let t0 = std::time::Instant::now();
        let result: Result<GenerateResponse> =
            crate::client::post_json(&self.http, &url, &body, None, |status, body_text| {
                PaperedError::LlmGeneration(format!(
                    "Ollama API error {}: {body_text}",
                    crate::client::status_text(status)
                ))
            })
            .await;
        let (usage, success, error) = match &result {
            Ok(resp) => (
                TokenUsage {
                    prompt_tokens: resp.prompt_eval_count,
                    completion_tokens: resp.eval_count,
                },
                true,
                None,
            ),
            Err(e) => (
                TokenUsage::default(),
                false,
                Some(truncate_error(&e.to_string())),
            ),
        };
        crate::llm::metrics::record_metric(
            self.metrics.as_ref(),
            kind,
            self.model.clone(),
            usage,
            t0.elapsed(),
            success,
            error,
        );
        let gen_resp = result?;
        Ok(gen_resp.response.trim().to_string())
    }

    async fn generate_openai_chat(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        images_base64: &[String],
        max_tokens: usize,
        temperature: f32,
        response_format: Option<serde_json::Value>,
    ) -> Result<String> {
        let url = self
            .provider
            .build_url(&self.api_base, self.provider.chat_path());

        let user_content = build_user_content(user_prompt, images_base64);
        let messages = build_messages(system_prompt, user_content);
        let effective_max_tokens = clamp_max_tokens(
            max_tokens,
            self.output_token_limit,
            &self.model,
            &self.api_base,
        );

        let is_json_mode = response_format
            .as_ref()
            .and_then(|f| f.get("type"))
            .and_then(|t| t.as_str())
            == Some(JSON_OBJECT_TYPE);

        let body = build_request_body(
            &self.model,
            messages,
            temperature,
            effective_max_tokens,
            response_format.as_ref(),
            self.extra_body.as_ref(),
            self.reasoning_effort.as_deref(),
        );

        let kind = if images_base64.is_empty() {
            CallKind::Chat
        } else {
            CallKind::Vision
        };
        let t0 = std::time::Instant::now();
        let result: Result<serde_json::Value> = crate::client::post_json(
            &self.http,
            &url,
            &body,
            self.api_key.as_deref(),
            |status, body_text| {
                PaperedError::LlmGeneration(format!(
                    "OpenAI API error {}: {body_text}",
                    crate::client::status_text(status)
                ))
            },
        )
        .await;
        let (usage, success, error) = match &result {
            Ok(json) => (openai_usage(json), true, None),
            Err(e) => (
                TokenUsage::default(),
                false,
                Some(truncate_error(&e.to_string())),
            ),
        };
        crate::llm::metrics::record_metric(
            self.metrics.as_ref(),
            kind,
            self.model.clone(),
            usage,
            t0.elapsed(),
            success,
            error,
        );
        let json = result?;
        Ok(parse_chat_response(&json, max_tokens, is_json_mode))
    }
}

/// Extract token usage from an OpenAI-compatible response body.
fn openai_usage(json: &serde_json::Value) -> TokenUsage {
    let usage = &json["usage"];
    TokenUsage {
        prompt_tokens: usage["prompt_tokens"].as_u64().map(|n| n as u32),
        completion_tokens: usage["completion_tokens"].as_u64().map(|n| n as u32),
    }
}

fn build_user_content(user_prompt: &str, images_base64: &[String]) -> serde_json::Value {
    if images_base64.is_empty() {
        serde_json::json!(user_prompt)
    } else {
        let mut content = vec![serde_json::json!({
            "type": "text",
            "text": user_prompt
        })];
        for b64 in images_base64 {
            content.push(serde_json::json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/png;base64,{}", b64)
                }
            }));
        }
        serde_json::json!(content)
    }
}

fn build_messages(system_prompt: &str, user_content: serde_json::Value) -> serde_json::Value {
    if system_prompt.is_empty() {
        serde_json::json!([
            { "role": "user", "content": user_content }
        ])
    } else {
        serde_json::json!([
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_content }
        ])
    }
}

fn clamp_max_tokens(max_tokens: usize, cap: Option<usize>, model: &str, api_base: &str) -> usize {
    if let Some(cap) = cap {
        if max_tokens > cap {
            tracing::info!(
                "Clamping max_tokens {} to provider cap {} ({} via {})",
                max_tokens,
                cap,
                model,
                api_base
            );
            cap
        } else {
            max_tokens
        }
    } else {
        max_tokens
    }
}

fn build_request_body(
    model: &str,
    messages: serde_json::Value,
    temperature: f32,
    max_tokens: usize,
    response_format: Option<&serde_json::Value>,
    extra_body: Option<&serde_json::Value>,
    reasoning_effort: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "temperature": temperature,
        "max_tokens": max_tokens,
    });

    if let Some(fmt) = response_format {
        body["response_format"] = fmt.clone();
    }

    if let Some(extra) = extra_body
        && let Some(obj) = extra.as_object()
    {
        for (key, value) in obj {
            body[key] = value.clone();
        }
    }

    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = serde_json::Value::String(effort.to_string());
    }

    body
}

fn parse_chat_response(json: &serde_json::Value, max_tokens: usize, is_json_mode: bool) -> String {
    let choice = json["choices"].get(0);
    let message = choice.and_then(|c| c.get("message"));

    if let Some(finish_reason) = choice
        .and_then(|c| c.get(FINISH_REASON))
        .and_then(|v| v.as_str())
        && finish_reason == FINISH_LENGTH
    {
        tracing::warn!(
            "LLM response truncated by max_tokens limit ({} tokens). Consider increasing max_tokens for this task.",
            max_tokens
        );
    }

    let mut content = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    let reasoning_content = message
        .and_then(|m| m.get(REASONING_CONTENT))
        .and_then(|c| c.as_str());

    if let Some(reasoning) = reasoning_content {
        tracing::debug!("LLM reasoning length: {} chars", reasoning.len());
    }

    if content.trim().is_empty() {
        if let Some(reasoning) = reasoning_content {
            if is_json_mode {
                tracing::warn!(
                    "LLM returned empty content with reasoning in JSON mode; returning empty"
                );
            } else {
                content = reasoning.to_string();
                tracing::info!("LLM content empty; using reasoning_content instead");
            }
        } else {
            tracing::warn!("LLM returned empty content and no reasoning_content");
        }
    }

    let trimmed = content.trim();
    if trimmed.len() == content.len() {
        content
    } else {
        trimmed.to_owned()
    }
}
