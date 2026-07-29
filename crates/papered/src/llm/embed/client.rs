use crate::error::{PaperedError, Result};
use crate::llm::Provider;
use crate::llm::metrics::{CallKind, MetricsSink, TokenUsage, truncate_error};
use crate::llm::rate_limiter::RateLimiter;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

/// Manual `Debug` for the sink-less client fields (`MetricsSink` is not `Debug`).
fn debug_option_sink(sink: &Option<MetricsSink>) -> &'static str {
    if sink.is_some() { "Some(..)" } else { "None" }
}

#[derive(Debug, Clone)]
pub struct EmbeddingResult {
    pub embedding: Vec<f32>,
}

/// A single item in a multimodal embedding input batch.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum MultimodalInput {
    /// Structured text input: `{"text": "..."}`
    Text { text: String },
    /// Structured image input: `{"image": "..."}`
    Image { image: String },
}

#[derive(Clone)]
pub struct EmbeddingClient {
    client: Client,
    api_base: String,
    api_key: Option<String>,
    model: String,
    provider: Provider,
    detected_dimension: Arc<AtomicUsize>,
    max_batch_size: usize,
    rate_limiter: Option<RateLimiter>,
    ollama_semaphore: Arc<Semaphore>,
    /// Optional multimodal params — set when the embedding config carries
    /// `truncate` / `encoding_format` (SiliconFlow Qwen-VL style).
    truncate: Option<String>,
    encoding_format: Option<String>,
    /// Whether this model supports multimodal (image+text) embedding.
    /// When `false`, `embed_image_with_mime` returns an error immediately
    /// without making any HTTP request.
    supports_multimodal: bool,
    /// Optional metrics sink injected by the daemon; records usage + latency
    /// for every provider call. `None` disables recording.
    metrics: Option<MetricsSink>,
}

impl std::fmt::Debug for EmbeddingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingClient")
            .field("api_base", &self.api_base)
            .field("model", &self.model)
            .field("provider", &self.provider)
            .field("max_batch_size", &self.max_batch_size)
            .field("supports_multimodal", &self.supports_multimodal)
            .field("truncate", &self.truncate)
            .field("encoding_format", &self.encoding_format)
            .field("metrics", &debug_option_sink(&self.metrics))
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct EmbeddingRequest {
    model: String,
    input: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
    usage: Option<EmbeddingUsage>,
}

#[derive(Deserialize)]
struct EmbeddingUsage {
    prompt_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Serialize)]
struct OllamaEmbeddingRequest {
    model: String,
    prompt: String,
}

#[derive(Deserialize)]
struct OllamaEmbeddingResponse {
    embedding: Vec<f32>,
    prompt_eval_count: Option<u32>,
}

impl EmbeddingClient {
    pub fn new(
        api_base: impl Into<String>,
        api_key: Option<impl Into<String>>,
        model: impl Into<String>,
        config: &crate::config::EmbeddingConfig,
    ) -> Result<Self> {
        if config.max_batch_size == 0 {
            return Err(PaperedError::invalid_argument("max_batch_size must be > 0"));
        }
        let client = crate::client::build_http_client(config.timeout_secs, None)?;
        let api_base = api_base.into();
        let provider = Provider::from_url(&api_base);

        Ok(Self {
            client,
            api_base,
            api_key: api_key.map(std::convert::Into::into),
            model: model.into(),
            provider,
            detected_dimension: Arc::new(AtomicUsize::new(0)),
            max_batch_size: config.max_batch_size,
            rate_limiter: None,
            ollama_semaphore: Arc::new(Semaphore::new(8)),
            truncate: config.truncate.clone(),
            encoding_format: config.encoding_format.clone(),
            supports_multimodal: config.supports_multimodal,
            metrics: None,
        })
    }

    /// Replace the HTTP client with a custom one (e.g. SSRF-hardened probe).
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub fn detected_dimension(&self) -> usize {
        self.detected_dimension.load(Ordering::Relaxed)
    }

    /// Whether this client can embed images.
    /// When `false`, `embed_image_with_mime` returns an error without
    /// making an HTTP request (no wasted bandwidth).
    pub fn supports_multimodal(&self) -> bool {
        self.supports_multimodal
    }

    fn dimension_arg(&self) -> Option<usize> {
        let d = self.detected_dimension();
        if d > 0 { Some(d) } else { None }
    }

    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Attach a metrics sink recording usage + latency for every provider call.
    #[must_use]
    pub fn with_metrics(mut self, sink: MetricsSink) -> Self {
        self.metrics = Some(sink);
        self
    }

    /// Set the concurrency limit for Ollama embedding requests.
    /// Defaults to 8 if not called.
    #[must_use]
    pub fn with_ollama_concurrency(mut self, concurrency: usize) -> Self {
        let permits = if concurrency > 0 { concurrency } else { 8 };
        self.ollama_semaphore = Arc::new(Semaphore::new(permits));
        self
    }

    pub async fn embed_single(&self, text: &str) -> Result<EmbeddingResult> {
        let embeddings = self.embed_batch_impl(&[text]).await?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| PaperedError::EmbeddingApi {
                status: 500,
                message: "Empty embedding response".to_string(),
            })
    }

    fn record_dimension(&self, dims: usize) -> Result<()> {
        let current = self.detected_dimension();
        if current == 0 {
            self.detected_dimension.store(dims, Ordering::Relaxed);
            Ok(())
        } else if dims == current {
            Ok(())
        } else {
            Err(PaperedError::EmbeddingApi {
                status: 500,
                message: format!("Dimension mismatch: expected {current}, got {dims}"),
            })
        }
    }

    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.embed_batch_impl(texts).await
    }

    async fn embed_batch_impl(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>> {
        let mut all_results = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(self.max_batch_size) {
            let chunk_results = self.embed_batch_inner(chunk).await?;
            all_results.extend(chunk_results);
        }
        Ok(all_results)
    }

    async fn embed_batch_inner(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>> {
        if self.provider == Provider::Ollama {
            // Ollama does not support batch embeddings in older versions,
            // so we parallelize single requests using concurrent tasks.
            // Limit concurrency to avoid overwhelming the local server.
            // Tasks own cloned state because callers (e.g. the batching
            // embedder) spawn the returned future, which must be 'static.
            let sem = self.ollama_semaphore.clone();

            let mut handles = Vec::with_capacity(texts.len());
            for text in texts {
                let client = self.clone();
                let text = text.to_string();
                let sem = sem.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = sem
                        .acquire()
                        .await
                        .map_err(|e| PaperedError::EmbeddingApi {
                            status: 500,
                            message: format!("Semaphore error: {e}"),
                        })?;
                    client.embed_ollama_single(&text).await
                }));
            }

            let mut results = Vec::with_capacity(texts.len());
            for handle in handles {
                results.push(handle.await.map_err(|e| PaperedError::EmbeddingApi {
                    status: 500,
                    message: format!("Embedding task panicked: {e}"),
                })??);
            }
            Ok(results)
        } else {
            self.embed_openai_batch(texts).await
        }
    }

    async fn embed_openai_batch(&self, texts: &[&str]) -> Result<Vec<EmbeddingResult>> {
        // Acquire rate limit permit
        if let Some(ref limiter) = self.rate_limiter {
            let estimated_tokens: usize =
                texts.iter().map(|t| crate::util::estimate_tokens(t)).sum();
            let _permit = limiter.acquire(estimated_tokens).await?;
        }

        let url = self
            .provider
            .build_url(&self.api_base, self.provider.embedding_path());

        let inputs: Vec<String> = texts.iter().map(std::string::ToString::to_string).collect();
        let dim_arg = self.detected_dimension();
        let body = EmbeddingRequest {
            model: self.model.clone(),
            input: inputs,
            dimensions: if dim_arg > 0 { Some(dim_arg) } else { None },
        };

        let vectors = self.post_embeddings(&url, &body, texts.len()).await?;
        Ok(vectors
            .into_iter()
            .map(|embedding| EmbeddingResult { embedding })
            .collect())
    }

    async fn embed_ollama_single(&self, text: &str) -> Result<EmbeddingResult> {
        if let Some(ref limiter) = self.rate_limiter {
            let estimated_tokens = crate::util::estimate_tokens(text);
            let _permit = limiter.acquire(estimated_tokens).await?;
        }

        let url = self
            .provider
            .build_url(&self.api_base, self.provider.embedding_path());

        let body = OllamaEmbeddingRequest {
            model: self.model.clone(),
            prompt: text.to_string(),
        };

        // Ollama requests are unauthenticated, so no API key is passed here.
        let t0 = std::time::Instant::now();
        let result: Result<OllamaEmbeddingResponse> =
            crate::client::post_json(&self.client, &url, &body, None, |status, message| {
                PaperedError::EmbeddingApi { status, message }
            })
            .await;
        let (usage, success, error) = match &result {
            Ok(resp) => (
                TokenUsage {
                    prompt_tokens: resp.prompt_eval_count,
                    completion_tokens: None,
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
            CallKind::Embedding,
            self.model.clone(),
            usage,
            t0.elapsed(),
            success,
            error,
        );
        let resp = result?;

        self.record_dimension(resp.embedding.len())?;

        Ok(EmbeddingResult {
            embedding: resp.embedding,
        })
    }

    // --- Image / multimodal embedding ---

    /// Embed an image (base64-encoded) with an explicit MIME type and optional caption.
    ///
    /// Sends the multimodal input format: a single image or `[image, caption]`.
    /// Returns an error if the provider does not support multimodal embedding;
    /// use [`embed_image_or_text`] for automatic fallback to text embedding.
    pub async fn embed_image_with_mime(
        &self,
        image_base64: &str,
        mime_type: &str,
        caption: Option<&str>,
    ) -> Result<Vec<f32>> {
        if !self.supports_multimodal {
            return Err(PaperedError::EmbeddingApi {
                status: 400,
                message: "Multimodal embedding not supported by this model".to_string(),
            });
        }
        let image = format!("data:{mime_type};base64,{image_base64}");
        let input = if let Some(cap) = caption {
            serde_json::to_value(&[
                MultimodalInput::Image { image },
                MultimodalInput::Text {
                    text: cap.to_string(),
                },
            ])
            .unwrap_or_default()
        } else {
            serde_json::to_value(&MultimodalInput::Image { image }).unwrap_or_default()
        };
        self.embed_multimodal_batch(input)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| PaperedError::EmbeddingApi {
                status: 500,
                message: "Empty multimodal embedding response".to_string(),
            })
    }

    async fn embed_multimodal_batch(&self, input: serde_json::Value) -> Result<Vec<Vec<f32>>> {
        let expected_count = match &input {
            serde_json::Value::Array(arr) => arr.len(),
            _ => 1,
        };

        #[derive(Serialize)]
        struct MultimodalRequest {
            model: String,
            input: serde_json::Value,
            #[serde(skip_serializing_if = "Option::is_none")]
            dimensions: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            truncate: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            encoding_format: Option<String>,
        }

        let req = MultimodalRequest {
            model: self.model.clone(),
            input,
            dimensions: self.dimension_arg(),
            truncate: self.truncate.clone(),
            encoding_format: self.encoding_format.clone(),
        };

        let url = self
            .provider
            .build_url(&self.api_base, self.provider.embedding_path());
        self.post_embeddings(&url, &req, expected_count).await
    }

    /// Shared inner method: POST a body to the embeddings endpoint, record
    /// metrics, parse the response, and return raw `Vec<f32>` vectors.
    async fn post_embeddings(
        &self,
        url: &str,
        body: impl Serialize,
        expected_count: usize,
    ) -> Result<Vec<Vec<f32>>> {
        let t0 = std::time::Instant::now();
        let result: Result<EmbeddingResponse> = crate::client::post_json(
            &self.client,
            url,
            &body,
            self.api_key.as_deref(),
            |status, message| PaperedError::EmbeddingApi { status, message },
        )
        .await;
        let (usage, success, error) = match &result {
            Ok(resp) => (
                TokenUsage {
                    prompt_tokens: resp.usage.as_ref().and_then(|u| u.prompt_tokens),
                    completion_tokens: None,
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
            CallKind::Embedding,
            self.model.clone(),
            usage,
            t0.elapsed(),
            success,
            error,
        );
        let resp = result?;
        tracing::debug!("Embedding request took {:.1}s", t0.elapsed().as_secs_f64());

        let mut results = vec![None; expected_count];
        for data in resp.data {
            if data.index < results.len() {
                self.record_dimension(data.embedding.len())?;
                results[data.index] = Some(data.embedding);
            }
        }

        results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.ok_or_else(|| PaperedError::EmbeddingApi {
                    status: 500,
                    message: format!("Missing embedding for index {i}"),
                })
            })
            .collect()
    }
}

// --- Free functions for image embedding ---

/// Encode an image file to a base64 string (async).
pub async fn image_to_base64(path: &std::path::Path) -> Result<String> {
    use base64::{Engine as _, engine::general_purpose};
    let data = tokio::fs::read(path)
        .await
        .map_err(|e| PaperedError::io_other(format!("Failed to read image: {e}")))?;
    Ok(general_purpose::STANDARD.encode(&data))
}

/// Embed an image via multimodal embedding, falling back to text-only if the
/// model does not support multimodal or the multimodal call fails.
pub async fn embed_image_or_text(
    client: &EmbeddingClient,
    image_path: &std::path::Path,
    fallback_text: &str,
) -> crate::error::Result<Vec<f32>> {
    if !client.supports_multimodal() {
        return client
            .embed_single(fallback_text)
            .await
            .map(|r| r.embedding);
    }
    let image_data = tokio::fs::read(image_path).await.map_err(|e| {
        crate::error::PaperedError::io_other(format!(
            "Failed to read image {}: {e}",
            image_path.display()
        ))
    })?;
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&image_data);
    let mime = image_path
        .extension()
        .and_then(|e| e.to_str())
        .map_or("image/png", crate::util::mime_from_ext);
    match client
        .embed_image_with_mime(&b64, mime, Some(fallback_text))
        .await
    {
        Ok(v) => Ok(v),
        Err(e) => {
            tracing::warn!(
                "Multimodal embed failed for {}: {e}, falling back to text",
                image_path.display()
            );
            client
                .embed_single(fallback_text)
                .await
                .map(|r| r.embedding)
                .map_err(|e2| {
                    tracing::debug!(
                        "Text fallback also failed for {}: {e2}",
                        image_path.display()
                    );
                    e2
                })
        }
    }
}
