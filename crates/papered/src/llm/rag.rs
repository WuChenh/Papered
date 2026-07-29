//! Retrieval-Augmented Generation (RAG) — the cognitive engine.
//!
//! Answers user questions by retrieving relevant papers and synthesizing
//! an LLM-generated response with citations.
//!
//! Optional query enhancement layer:
//! - A single LLM call produces both a rewritten query and a hypothetical
//!   document. Retrieval runs the original query, the rewritten query, and the
//!   hypothetical-document embedding as parallel representations, then fuses
//!   the result lists with Reciprocal Rank Fusion (RRF).

/// Multiplier applied to `top_k` when searching chunks within candidate papers,
/// to ensure enough fine-grained context is retrieved for citation assembly.
const RAG_CHUNK_MULTIPLIER: usize = 3;

use crate::config::AppConfig;
use crate::error::{PaperedError, Result};
use crate::llm::query_enhancer::{QueryEnhancer, QueryEnhancerConfig};
use crate::paper::Paper;
use crate::paper::PaperSearchResult;
use crate::retrieval;
use crate::search::SearchEngine;
use crate::search::SearchMethod;
use crate::search::query_analyzer::QueryProfile;
use crate::store::vector::VectorStore;
use crate::util::str_enum::StrLabel;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Maximum characters for a section snippet included in RAG context.
const RAG_SECTION_SNIPPET_CHARS: usize = 500;

/// A structured pointer to a specific chunk backing a RAG source.
///
/// Lets agents verify and navigate (open the section, fetch the figure) from
/// an answer, instead of relying on a flattened text blob. Mirrors the
/// `referenced_chunks` evidence model used by agentic retrieval systems.
#[derive(Debug, Clone, Serialize)]
pub struct CitationRef {
    pub chunk_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_number: Option<u32>,
}

/// A single retrieved source used as context.
#[derive(Debug, Clone, Serialize)]
pub struct RagSource {
    pub paper: Paper,
    pub content: String,
    pub score: f32,
    /// Structured chunk-level locators backing `content`. Empty when the
    /// source was assembled from section summaries rather than chunks.
    pub citations: Vec<CitationRef>,
}

/// Flattened, serializable view of a [`RagSource`] for API responses.
///
/// Both the REST daemon and the MCP server serialize this exact shape, so
/// clients see one source contract regardless of transport.
#[derive(Debug, Clone, Serialize)]
pub struct RagSourceView {
    pub paper_id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub published_date: Option<String>,
    pub venue: Option<String>,
    pub doi: Option<String>,
    pub content: String,
    pub score: f32,
    pub citations: Vec<CitationRef>,
}

impl From<RagSource> for RagSourceView {
    fn from(s: RagSource) -> Self {
        Self {
            paper_id: s.paper.id,
            title: s.paper.title,
            authors: s.paper.authors,
            published_date: s.paper.published_date,
            venue: s.paper.venue,
            doi: s.paper.doi,
            content: s.content,
            score: s.score,
            citations: s.citations,
        }
    }
}

fn citations_from_chunks(chunks: &[retrieval::ChunkContext]) -> Vec<CitationRef> {
    chunks
        .iter()
        .map(|c| CitationRef {
            chunk_id: c.chunk_id.clone(),
            section_path: c.heading_path.clone(),
            page_number: c.page_number,
        })
        .collect()
}

/// RAG answer with citations.
#[derive(Debug, Clone, Serialize)]
pub struct RagAnswer {
    pub answer: String,
    pub sources: Vec<RagSource>,
    pub search_method_used: String,
}

impl retrieval::RagContextSource for RagSource {
    fn paper(&self) -> &crate::paper::Paper {
        &self.paper
    }
    fn content(&self) -> &str {
        &self.content
    }
}

/// Cognitive engine that retrieves context and generates answers via LLM.
pub struct RagEngine {
    search_engine: SearchEngine,
    store: Arc<dyn VectorStore>,
    config: AppConfig,
    query_enhancer: Option<QueryEnhancer>,
    cached_default_prompt: Arc<RwLock<Option<(String, f32)>>>,
    llm_client: Option<crate::llm::client::LlmClient>,
}

impl RagEngine {
    pub async fn new(
        store: Arc<dyn VectorStore>,
        search_engine: SearchEngine,
        config: AppConfig,
    ) -> crate::Result<Self> {
        // Resolve RAG endpoint for optional enhancement layer. An empty or
        // unconfigured `purposes.rag` (fresh install before the setup wizard)
        // degrades to "no LLM client" instead of failing daemon startup; the
        // query path already errors gracefully when `llm_client` is `None`.
        let rag_endpoint = config.resolve_model(&config.purposes.rag).ok();

        let rate_limiter = rag_endpoint
            .as_ref()
            .and_then(crate::llm::rate_limiter::RateLimiter::for_endpoint);

        let query_enhancer = build_query_enhancer(config.purposes.enhancement.as_deref(), &config);

        // Initialize prompt cache from DB
        let cached_prompt = store
            .get_default_prompt()
            .await?
            .map(|p| (p.system_prompt, p.temperature));

        // The engine owns a store handle, so it wires usage/latency metrics
        // into every LLM client it builds. Recording is best-effort (the
        // sink logs write failures at warn level).
        let metrics = crate::llm::metrics::store_metrics_sink(&store);

        let llm_client = rag_endpoint.as_ref().and_then(|ep| {
            crate::llm::client::LlmClient::from_config(ep, rate_limiter.clone())
                .map(|client| client.with_metrics(metrics.clone()))
                .ok()
        });

        let mut query_enhancer = query_enhancer;
        if let Some(ref mut enhancer) = query_enhancer {
            enhancer.set_metrics(metrics);
        }

        Ok(Self {
            search_engine,
            store,
            config,
            query_enhancer,
            cached_default_prompt: Arc::new(RwLock::new(cached_prompt)),
            llm_client,
        })
    }

    /// Invalidate the cached default prompt, forcing a fresh DB read on next access.
    pub async fn invalidate_prompt_cache(&self) {
        *self.cached_default_prompt.write().await = None;
    }

    /// Ask a question and get an answer grounded in your thought space.
    ///
    /// `search_method_override` allows per-request method selection.
    /// `prompt_id` selects a user-defined prompt from the database.
    /// `use_enhancement` enables the unified query enhancement layer
    ///   (rewriting + HyDE); when `None`, adaptive retrieval decides.
    /// `paper_id` scopes retrieval to a single paper (skips Tier-1 vector search).
    pub async fn ask(
        &self,
        question: &str,
        search_method_override: Option<SearchMethod>,
        prompt_id: Option<&str>,
        use_enhancement: Option<bool>,
        paper_id: Option<&str>,
    ) -> Result<RagAnswer> {
        if self.llm_client.is_none() {
            return Err(PaperedError::config(
                crate::config::unconfigured_model_message("chat"),
            ));
        }

        let (top_k, effective_use_enhancement, adaptive_profile) =
            self.adaptive_profile_for_query(question, use_enhancement);

        if let Some(pid) = paper_id {
            return self
                .search_within_single_paper(question, pid, top_k, prompt_id)
                .await;
        }

        let method = search_method_override.unwrap_or(self.config.rag.search_method);

        let result_lists = self
            .run_tier1_searches(
                question,
                top_k,
                method,
                adaptive_profile.as_ref(),
                effective_use_enhancement,
            )
            .await;

        self.fuse_and_generate(result_lists, question, method, top_k, prompt_id)
            .await
    }

    fn adaptive_profile_for_query(
        &self,
        question: &str,
        use_enhancement: Option<bool>,
    ) -> (usize, bool, Option<QueryProfile>) {
        if self.config.rag.adaptive_enabled {
            let profile = QueryProfile::analyze(question);
            tracing::info!(
                "Adaptive retrieval: {:?} query ({} words, {} chars), top_k={}",
                profile.complexity,
                profile.word_count,
                profile.char_count,
                profile.recommended_top_k(self.config.rag.top_k)
            );
            (
                profile.recommended_top_k(self.config.rag.top_k),
                use_enhancement.unwrap_or(profile.should_use_enhancement()),
                Some(profile),
            )
        } else {
            (
                self.config.rag.top_k,
                use_enhancement.unwrap_or(false),
                None,
            )
        }
    }

    async fn search_within_single_paper(
        &self,
        question: &str,
        paper_id: &str,
        top_k: usize,
        prompt_id: Option<&str>,
    ) -> Result<RagAnswer> {
        let sources = match self.search_within_paper(question, paper_id, top_k).await {
            Ok(s) => s,
            Err(PaperedError::NotFound(msg, None)) => {
                return Ok(RagAnswer {
                    answer: msg,
                    sources: vec![],
                    search_method_used: "paper".to_string(),
                });
            }
            Err(e) => return Err(e),
        };
        let method_used = "paper".to_string();
        if sources.is_empty() {
            return Ok(RagAnswer {
                answer: format!(
                    "Paper {paper_id} was found but contains no searchable content matching your question."
                ),
                sources: vec![],
                search_method_used: method_used,
            });
        }
        let sources = deduplicate_sources(sources);
        let context = retrieval::build_rag_context(
            &sources,
            self.config.rag.max_context_chars,
            self.config.rag.use_compact_context,
            &meta_fields(&self.config.rag.include_meta_fields),
        );
        let answer = self.generate_answer(question, &context, prompt_id).await?;
        Ok(RagAnswer {
            answer,
            sources,
            search_method_used: method_used,
        })
    }

    async fn run_tier1_searches(
        &self,
        question: &str,
        top_k: usize,
        method: SearchMethod,
        adaptive_profile: Option<&QueryProfile>,
        effective_use_enhancement: bool,
    ) -> Vec<Vec<PaperSearchResult>> {
        let mut result_lists: Vec<Vec<PaperSearchResult>> = Vec::new();

        let (baseline_result, enhancement_result) = tokio::join!(
            async {
                match self
                    .tier1_search(question, top_k, method, adaptive_profile, None)
                    .await
                {
                    Ok(results) if !results.is_empty() => {
                        tracing::info!(
                            "baseline {} search returned {} results",
                            method.as_str(),
                            results.len()
                        );
                        Some(results)
                    }
                    Ok(_) => {
                        tracing::warn!("baseline {} search returned no results", method.as_str());
                        None
                    }
                    Err(e) => {
                        tracing::warn!("baseline {} search failed: {}", method.as_str(), e);
                        None
                    }
                }
            },
            async {
                if effective_use_enhancement {
                    if let Some(ref enhancer) = self.query_enhancer {
                        match enhancer.enhance(question).await {
                            Ok(result) => Some(result),
                            Err(e) => {
                                tracing::warn!(
                                    "query enhancement failed: {}, using baseline only",
                                    e
                                );
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        );

        if let Some(results) = baseline_result {
            result_lists.push(results);
        }

        if let Some(enhancement) = enhancement_result {
            tracing::info!(
                "query enhancement succeeded: rewritten_len={} hypothetical_len={}",
                enhancement.rewritten_query.len(),
                enhancement.hypothetical_document.len()
            );

            let (rewritten_res, hyde_res) = tokio::join!(
                self.tier1_search(
                    &enhancement.rewritten_query,
                    top_k,
                    method,
                    adaptive_profile,
                    None,
                ),
                async {
                    match self
                        .search_engine
                        .embedding
                        .embed_single(&enhancement.hypothetical_document)
                        .await
                    {
                        Ok(e) => Some(e.embedding),
                        Err(e) => {
                            tracing::warn!(
                                "hypothetical-document embedding failed: {}, skipping",
                                e
                            );
                            None
                        }
                    }
                }
            );

            match rewritten_res {
                Ok(results) if !results.is_empty() => {
                    tracing::info!("rewritten-query search returned {} results", results.len());
                    result_lists.push(results);
                }
                Ok(_) => {
                    tracing::warn!("rewritten-query search returned no results");
                }
                Err(e) => {
                    tracing::warn!("rewritten-query search failed: {}", e);
                }
            }

            if let Some(emb) = hyde_res {
                match self
                    .tier1_search(
                        question,
                        top_k,
                        SearchMethod::Semantic,
                        adaptive_profile,
                        Some(&emb),
                    )
                    .await
                {
                    Ok(results) if !results.is_empty() => {
                        tracing::info!("HyDE search returned {} results", results.len());
                        result_lists.push(results);
                    }
                    Ok(_) => {
                        tracing::warn!("HyDE search returned no results");
                    }
                    Err(e) => {
                        tracing::warn!("HyDE search failed: {}", e);
                    }
                }
            }
        }

        result_lists
    }

    async fn fuse_and_generate(
        &self,
        result_lists: Vec<Vec<PaperSearchResult>>,
        question: &str,
        method: SearchMethod,
        top_k: usize,
        prompt_id: Option<&str>,
    ) -> Result<RagAnswer> {
        let should_fuse = result_lists.len() > 1;
        let mut fused = if should_fuse {
            tracing::info!(
                "fusing {} result lists with RRF (k={})",
                result_lists.len(),
                crate::search::engine::RRF_K
            );
            SearchEngine::fuse_paper_results_rrf(&result_lists, crate::search::engine::RRF_K)
        } else {
            result_lists.into_iter().next().unwrap_or_default()
        };

        if fused.is_empty() {
            return Ok(RagAnswer {
                answer: "I couldn't find any relevant papers in your thought space to answer this question.".to_string(),
                sources: vec![],
                search_method_used: method.to_string(),
            });
        }

        // Normalize scores relative to the top result so RAG sources display on
        // the same [0,1] scale as the Search UI (top = 100%) instead of raw RRF
        // sums (~0.03), which read as "5% relevant" and confused users.
        let max_score = fused.iter().map(|r| r.score).fold(0.0f32, f32::max);
        if max_score > 0.0 {
            for r in &mut fused {
                r.score /= max_score;
            }
        }

        let sources = self
            .build_sources_from_results(fused, question, top_k, |_, r| r.score)
            .await?;
        let sources = deduplicate_sources(sources);
        let context = retrieval::build_rag_context(
            &sources,
            self.config.rag.max_context_chars,
            self.config.rag.use_compact_context,
            &meta_fields(&self.config.rag.include_meta_fields),
        );

        let answer = self.generate_answer(question, &context, prompt_id).await?;

        Ok(RagAnswer {
            answer,
            sources,
            search_method_used: method.to_string(),
        })
    }

    // ------------------------------------------------------------------
    // Retrieval
    // ------------------------------------------------------------------

    /// Tier-1 candidate paper search. Returns raw paper-level results so that
    /// multiple representations (original, rewritten, HyDE) can be fused before
    /// Tier-2 chunk assembly.
    async fn tier1_search(
        &self,
        query: &str,
        top_k: usize,
        method: SearchMethod,
        profile: Option<&QueryProfile>,
        hyde_embedding: Option<&[f32]>,
    ) -> Result<Vec<PaperSearchResult>> {
        match method {
            SearchMethod::Fulltext => {
                self.search_engine
                    .fulltext_search_papers(query, top_k)
                    .await
            }
            SearchMethod::Hybrid => {
                if let Some(profile) = profile {
                    self.search_engine
                        .hybrid_search_adaptive_with_embedding(
                            query,
                            hyde_embedding,
                            None,
                            top_k,
                            0.1,
                            Some(profile),
                        )
                        .await
                } else {
                    self.search_engine
                        .hybrid_search_with_embedding(
                            query,
                            hyde_embedding.unwrap_or(&[]),
                            None,
                            top_k,
                            0.1,
                        )
                        .await
                }
            }
            SearchMethod::Semantic => {
                if let Some(emb) = hyde_embedding {
                    self.search_engine
                        .search_with_embedding(query, emb, None, top_k, 0.1)
                        .await
                } else {
                    self.search_engine.search(query, None, top_k, 0.1).await
                }
            }
        }
    }

    /// Shared Tier-2 resolution: full-text chunk search + context assembly +
    /// with matched-section fallback for papers without chunk hits.
    async fn build_sources_from_results(
        &self,
        results: Vec<PaperSearchResult>,
        question: &str,
        top_k: usize,
        score: impl Fn(usize, &PaperSearchResult) -> f32,
    ) -> Result<Vec<RagSource>> {
        let paper_ids: Vec<String> = results.iter().map(|r| r.paper.id.clone()).collect();
        let ids: Vec<&str> = paper_ids.iter().map(std::string::String::as_str).collect();
        let hits = self
            .store
            .search_chunks(&ids, question, top_k * RAG_CHUNK_MULTIPLIER)
            .await?;
        let chunk_contexts = retrieval::assemble_chunk_context(&*self.store, &hits).await?;

        let mut paper_chunks: HashMap<String, Vec<retrieval::ChunkContext>> = HashMap::new();
        for ctx in chunk_contexts {
            paper_chunks
                .entry(ctx.paper.id.clone())
                .or_default()
                .push(ctx);
        }

        // Split the total context budget across the papers that actually have
        // chunk hits so a small result set is not artificially capped by the
        // per-paper limit. With the old fixed per-paper cap, a 2-paper answer
        // only used ~8k of the 24k total budget and answer-relevant chunks
        // were dropped. Papers served by the matched-section fallback do not
        // consume chunk budget, so counting them (via `results.len()`) would
        // dilute the share of the papers that do. The overall
        // `max_context_chars` cap in `build_rag_context` still bounds the total.
        let per_paper_budget = per_paper_chunk_budget(&self.config.rag, paper_chunks.len());

        let mut sources = Vec::new();
        for (idx, r) in results.into_iter().enumerate() {
            let score = score(idx, &r);
            if let Some(chunks) = paper_chunks.get(&r.paper.id) {
                let content = retrieval::build_rag_context_compact(
                    chunks,
                    per_paper_budget,
                    &meta_fields(&self.config.rag.include_meta_fields),
                );
                if !content.is_empty() {
                    sources.push(RagSource {
                        paper: r.paper,
                        content,
                        score,
                        citations: citations_from_chunks(chunks),
                    });
                    continue;
                }
            }
            let content = r
                .matched_sections
                .iter()
                .map(|s| format!("[{}] {}", s.section_type, s.content_snippet))
                .collect::<Vec<_>>()
                .join("\n\n");

            sources.push(RagSource {
                paper: r.paper,
                content,
                score,
                citations: Vec::new(),
            });
        }

        Ok(sources)
    }

    /// Paper-scoped retrieval: load chunks + sections for a single paper.
    /// Skips Tier-1 paper search — we already know the target paper.
    /// Returns `NotFound` error when the paper doesn't exist in the store.
    async fn search_within_paper(
        &self,
        question: &str,
        paper_id: &str,
        top_k: usize,
    ) -> Result<Vec<RagSource>> {
        let (paper_result, sections_result) = tokio::join!(
            self.store.get_paper(paper_id),
            self.store.get_sections(paper_id)
        );
        let paper = match paper_result? {
            Some(p) => p,
            None => {
                return Err(PaperedError::NotFound(
                    format!("Paper {paper_id} not found in thought space"),
                    None,
                ));
            }
        };
        let sections = sections_result?;
        if !sections.sections.is_empty() {
            let content = retrieval::sections_to_context(&sections, RAG_SECTION_SNIPPET_CHARS);
            return Ok(vec![RagSource {
                paper,
                content,
                score: 1.0,
                citations: Vec::new(),
            }]);
        }

        // Fallback: full-text chunk search
        let ids = vec![paper_id];
        let hits = self
            .store
            .search_chunks(&ids, question, top_k * RAG_CHUNK_MULTIPLIER)
            .await?;
        let chunk_contexts = retrieval::assemble_chunk_context(&*self.store, &hits).await?;

        if !chunk_contexts.is_empty() {
            let content = retrieval::build_rag_context_full_meta(
                &chunk_contexts,
                self.config.rag.max_paper_scoped_chars,
            );
            if !content.is_empty() {
                return Ok(vec![RagSource {
                    paper,
                    content,
                    score: 1.0,
                    citations: citations_from_chunks(&chunk_contexts),
                }]);
            }
        }

        Ok(Vec::new())
    }

    // ------------------------------------------------------------------
    // Generation
    // ------------------------------------------------------------------

    async fn generate_answer(
        &self,
        question: &str,
        context: &str,
        prompt_id: Option<&str>,
    ) -> Result<String> {
        // Resolve prompt: explicit ID → default DB prompt → config fallback
        let (system_prompt, temperature) = self.resolve_prompt(prompt_id).await?;

        let user_prompt = format!("## Context\n{context}\n\n## Question\n{question}");

        let client = self
            .llm_client
            .as_ref()
            .ok_or_else(|| PaperedError::config("RAG LLM client not initialized"))?;

        client
            .generate(
                &system_prompt,
                &user_prompt,
                self.config.rag.max_output_tokens,
                temperature,
            )
            .await
    }

    async fn resolve_prompt(&self, prompt_id: Option<&str>) -> Result<(String, f32)> {
        // Specific prompt ID always fetches from DB (may have changed)
        if let Some(id) = prompt_id {
            if let Some(prompt) = self.store.get_prompt(id).await? {
                return Ok((prompt.system_prompt, prompt.temperature));
            }
            return Ok((
                self.config.rag.system_prompt.clone(),
                self.config.rag.temperature,
            ));
        }

        // Default prompt from memory cache
        {
            let cache = self.cached_default_prompt.read().await;
            if let Some(ref cached) = *cache {
                return Ok(cached.clone());
            }
        }

        // Fallback: load from DB and cache
        let result = if let Some(prompt) = self.store.get_default_prompt().await? {
            Ok((prompt.system_prompt, prompt.temperature))
        } else {
            Ok((
                self.config.rag.system_prompt.clone(),
                self.config.rag.temperature,
            ))
        };

        // Update cache (only cache successful results)
        if let Ok(ref val) = result {
            *self.cached_default_prompt.write().await = Some(val.clone());
        }
        result
    }
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

/// Borrow configured meta-field names as `&str` for the retrieval layer.
fn meta_fields(fields: &[String]) -> Vec<&str> {
    fields.iter().map(String::as_str).collect()
}

/// Build the optional unified query enhancer from its configured purpose.
/// Returns `None` — with a warning — when the purpose is unset, cannot be
/// resolved to an endpoint, or fails to construct.
pub fn build_query_enhancer(purpose_id: Option<&str>, config: &AppConfig) -> Option<QueryEnhancer> {
    let purpose_id = purpose_id?;
    let Ok(endpoint) = config.resolve_model(purpose_id) else {
        tracing::warn!("enhancement endpoint '{purpose_id}' not found in registry");
        return None;
    };
    let limiter = crate::llm::rate_limiter::RateLimiter::for_endpoint(&endpoint);
    let QueryEnhancerConfig {
        temperature,
        max_output_tokens,
    } = config.rag.enhancement.clone().unwrap_or_default();
    match QueryEnhancer::with_rate_limiter(&endpoint, temperature, max_output_tokens, limiter) {
        Ok(enhancer) => Some(enhancer),
        Err(e) => {
            tracing::warn!("Failed to create query enhancer: {e}");
            None
        }
    }
}

/// Per-paper chunk-context budget: the total context budget divided across
/// the `papers_with_chunks` papers that have chunk hits, floored by the
/// configured per-paper cap. Only papers with actual chunk content share the
/// budget — matched-section fallback papers do not consume it.
fn per_paper_chunk_budget(rag: &crate::config::RagConfig, papers_with_chunks: usize) -> usize {
    rag.max_paper_context_chars
        .max(rag.max_context_chars / papers_with_chunks.max(1))
}

/// Merge sources that reference the same paper, keeping the highest score
/// and unioning citations (deduplicated by `chunk_id`).
fn deduplicate_sources(sources: Vec<RagSource>) -> Vec<RagSource> {
    let mut map: HashMap<String, RagSource> = HashMap::new();
    for src in sources {
        if let Some(existing) = map.get_mut(&src.paper.id) {
            existing.score = existing.score.max(src.score);
            existing.content.push_str("\n\n");
            existing.content.push_str(&src.content);
            for citation in src.citations {
                if !existing
                    .citations
                    .iter()
                    .any(|c| c.chunk_id == citation.chunk_id)
                {
                    existing.citations.push(citation);
                }
            }
        } else {
            map.insert(src.paper.id.clone(), src);
        }
    }
    let mut result: Vec<_> = map.into_values().collect();
    result.sort_by(|a, b| b.score.total_cmp(&a.score));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rag_config(total: usize, per_paper: usize) -> crate::config::RagConfig {
        crate::config::RagConfig {
            max_context_chars: total,
            max_paper_context_chars: per_paper,
            ..Default::default()
        }
    }

    #[test]
    fn budget_splits_across_papers_with_chunks_only() {
        let rag = rag_config(24_000, 4_000);
        // 5 result papers but only 2 have chunk hits → split by 2, not 5.
        assert_eq!(per_paper_chunk_budget(&rag, 2), 12_000);
        assert_eq!(per_paper_chunk_budget(&rag, 5), 4_800);
    }

    #[test]
    fn budget_never_drops_below_configured_per_paper_cap() {
        let rag = rag_config(24_000, 4_000);
        assert_eq!(per_paper_chunk_budget(&rag, 10), 4_000);
        // Degenerate case: no chunk hits at all — no division blow-up.
        assert_eq!(per_paper_chunk_budget(&rag, 0), 24_000);
    }
}
