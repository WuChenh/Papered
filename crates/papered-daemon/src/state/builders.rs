//! Client builders, store initialization, and indexing queue helpers.

use crate::api::types::{ERR_QUEUE_CLOSED, map_err, service_unavailable};
use axum::http::StatusCode;
use axum::response::Json;
use papered::config::ModelEndpoint;
use papered::error::ApiError;
use papered::llm::embed::EmbeddingClient;
use papered::llm::reranker::RerankerClient;
use papered::paper::{Paper, PaperStatus};
use papered::store::vector::VectorStore;
use papered::{AppConfig, StrLabel};
use std::sync::Arc;

pub fn build_embedding_client(config: &AppConfig) -> papered::error::Result<EmbeddingClient> {
    let emb_endpoint = config.resolve_model(&config.purposes.embedding)?;
    let mut embedding = EmbeddingClient::new(
        &emb_endpoint.api_base,
        emb_endpoint.api_key.clone(),
        &emb_endpoint.model,
        &config.embedding,
    )?;
    if let Some(limiter) = papered::llm::rate_limiter::RateLimiter::for_endpoint(&emb_endpoint) {
        embedding = embedding.with_rate_limiter(limiter);
    }
    if emb_endpoint.concurrency > 0 {
        embedding = embedding.with_ollama_concurrency(emb_endpoint.concurrency);
    }
    Ok(embedding)
}

pub fn build_reranker_client(config: &AppConfig) -> papered::error::Result<RerankerClient> {
    let rerank_endpoint = config.resolve_model(&config.purposes.reranker)?;
    let mut reranker = RerankerClient::new(&config.reranker, &rerank_endpoint)?;
    if let Some(limiter) = papered::llm::rate_limiter::RateLimiter::for_endpoint(&rerank_endpoint) {
        reranker = reranker.with_rate_limiter(limiter);
    }
    Ok(reranker)
}

pub fn build_embedding_or_placeholder(
    config: &AppConfig,
    metrics: papered::llm::metrics::MetricsSink,
    degraded_msg: &str,
) -> EmbeddingClient {
    build_embedding_client(config)
        .map(|c| c.with_metrics(metrics))
        .unwrap_or_else(|e| {
            if config.purposes.embedding.is_empty() {
                tracing::warn!(
                    "{}",
                    papered::config::unconfigured_model_message("embedding")
                );
            } else {
                tracing::warn!(
                    "Embedding model not configured or unavailable ({e}); {degraded_msg}"
                );
            }
            placeholder_embedding_client()
        })
}

pub fn build_reranker_or_placeholder(
    config: &AppConfig,
    metrics: papered::llm::metrics::MetricsSink,
    degraded_msg: &str,
) -> RerankerClient {
    build_reranker_client(config)
        .map(|c| c.with_metrics(metrics))
        .unwrap_or_else(|e| {
            if config.purposes.reranker.is_empty() {
                tracing::warn!(
                    "{}",
                    papered::config::unconfigured_model_message("reranker")
                );
            } else {
                tracing::warn!(
                    "Reranker model not configured or unavailable ({e}); {degraded_msg}"
                );
            }
            placeholder_reranker_client(config)
        })
}

/// Placeholder used when no model is configured yet (fresh install before the
/// setup wizard runs). Any real call fails fast against the discard port, and
/// the recovery watcher / config-update path swaps in the real client once the
/// config is completed.
pub fn placeholder_embedding_client() -> EmbeddingClient {
    EmbeddingClient::new(
        "http://127.0.0.1:9",
        None::<String>,
        "",
        &papered::config::EmbeddingConfig::default(),
    )
    .expect("placeholder embedding client uses fixed valid input")
}

/// Reranker counterpart of [`placeholder_embedding_client`].
pub fn placeholder_reranker_client(config: &AppConfig) -> RerankerClient {
    let endpoint = ModelEndpoint::placeholder();
    RerankerClient::new(&config.reranker, &endpoint)
        .expect("placeholder reranker client uses fixed valid input")
}

/// Build the search, RAG, and indexing engines from the shared store, clients,
/// and configuration. This is the single construction path used at startup,
/// during client reload, and in tests so the three engines stay consistent.
pub async fn build_engines(
    store: Arc<dyn VectorStore>,
    embedding: EmbeddingClient,
    reranker: RerankerClient,
    config: &AppConfig,
) -> papered::error::Result<(
    papered::search::SearchEngine,
    papered::llm::rag::RagEngine,
    papered::Indexer,
)> {
    let search_engine =
        papered::search::SearchEngine::new(store.clone(), embedding.clone(), reranker.clone());
    let rag_engine =
        papered::llm::rag::RagEngine::new(store.clone(), search_engine.clone(), config.clone())
            .await?;
    let indexer = papered::Indexer::new(store, embedding, config.clone())?;
    Ok((search_engine, rag_engine, indexer))
}

pub(crate) async fn init_store(
    config: &AppConfig,
) -> papered::error::Result<(Arc<dyn VectorStore>, bool, Option<String>)> {
    let db_path = config.db_path();
    let store = papered::create_store(&db_path).await?;
    let store_dim = store.store_dimension().await;
    let placeholder_table = store_dim.is_none() || store_dim == Some(0);
    let current_fingerprint = config.embedding_fingerprint();

    match store
        .get_papers_by_status(PaperStatus::Processing.as_str())
        .await
    {
        Ok(stale_papers) => {
            for paper in &stale_papers {
                tracing::warn!("Marking stale processing paper as failed: {}", paper.id);
                if let Err(e) = store
                    .update_paper_status(
                        &paper.id,
                        PaperStatus::Failed.as_str(),
                        Some(
                            "Daemon restarted while paper was processing. Retry indexing manually.",
                        ),
                        None,
                    )
                    .await
                {
                    tracing::warn!("Failed to mark stale paper {} as failed: {}", paper.id, e);
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to query processing papers: {}", e);
        }
    }

    Ok((store, placeholder_table, current_fingerprint))
}

pub(crate) async fn queue_paper_for_indexing(
    store: &dyn VectorStore,
    import_tx: &tokio::sync::mpsc::Sender<papered::util::IndexJob>,
    paper: &mut Paper,
    file_path: Option<String>,
    sections_only: bool,
    reembed_only: bool,
) -> std::result::Result<(), (StatusCode, Json<ApiError>)> {
    paper.status = PaperStatus::Processing;
    paper.file_path = file_path;
    store.insert_paper(paper).await.map_err(map_err)?;
    let job = papered::util::IndexJob {
        paper_id: paper.id.clone(),
        file_path: paper.file_path.clone().unwrap_or_default(),
        is_reindex: false,
        retry_count: 0,
        sections_only,
        reembed_only,
    };
    import_tx.send(job).await.map_err(|_| {
        service_unavailable(ERR_QUEUE_CLOSED, "Indexing queue is closed".to_string())
    })?;
    Ok(())
}
