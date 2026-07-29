use crate::error::{PaperedError, Result};
use crate::index::multimodal::FigureInfo;
use crate::llm::embed::EmbeddingClient;
use crate::llm::reranker::RerankerClient;
use crate::paper::section::SectionType;
use crate::paper::{MatchedSection, Paper, PaperSearchResult, PaperStatus};
use crate::search::SearchMethod;
use crate::search::graph::{PaperGraph, build_paper_graph};
use crate::search::query_analyzer::{QueryComplexity, QueryProfile};
use crate::store::vector::VectorStore;
use crate::util::str_enum::StrLabel;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Semantic weight for hybrid search (0.0–1.0).
pub(crate) const HYBRID_SEMANTIC_WEIGHT: f32 = 0.5;
/// RRF constant k for rank fusion.
pub(crate) const RRF_K: f32 = 60.0;
/// Maximum number of documents to pass to the neural reranker.
pub(crate) const MAX_RERANK_DOCS: usize = 100;
/// Minimum number of candidate documents required before reranking is applied.
pub(crate) const MIN_DOCS_TO_RERANK: usize = 5;
/// Keyword boost factor for full-text search results.
pub(crate) const KEYWORD_BOOST_FACTOR: f32 = 0.5;

/// Result of a semantic figure search.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FigureSearchResult {
    pub paper: Paper,
    pub figure: FigureInfo,
    pub score: f32,
}

/// Result of a passage (verbatim source-chunk) lexical search.
///
/// Unlike [`PaperSearchResult`], which aggregates LLM-processed sections per
/// paper, a passage result points at a single raw chunk of the original text so
/// the UI can show the exact fragment that matched.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PassageSearchResult {
    pub paper: Paper,
    pub chunk: crate::chunker::Chunk,
    pub score: f32,
}

/// Private trait for multimodal item types used in generic search.
trait MultimodalItem: Clone {
    fn id(&self) -> &str;
}

impl MultimodalItem for FigureInfo {
    fn id(&self) -> &str {
        &self.id
    }
}

/// Search engine supporting semantic, hybrid, and full-text search with reranking.
#[derive(Clone)]
pub struct SearchEngine {
    store: Arc<dyn VectorStore>,
    pub embedding: EmbeddingClient,
    reranker: RerankerClient,
}

impl SearchEngine {
    pub fn new(
        store: Arc<dyn VectorStore>,
        embedding: EmbeddingClient,
        reranker: RerankerClient,
    ) -> Self {
        Self {
            store,
            embedding,
            reranker,
        }
    }

    /// Fuse multiple ranked paper lists using Reciprocal Rank Fusion (RRF).
    ///
    /// Each list contributes `1 / (rrf_k + rank)` to a paper's score, where rank
    /// is the zero-based position in that list. Matched sections are merged and
    /// deduplicated by `section_type`. The returned list is sorted by fused score
    /// descending. Empty or single-list inputs are handled without extra work.
    pub fn fuse_paper_results_rrf(
        results_list: &[Vec<PaperSearchResult>],
        rrf_k: f32,
    ) -> Vec<PaperSearchResult> {
        if results_list.is_empty() {
            return Vec::new();
        }
        if results_list.len() == 1 {
            return results_list[0].clone();
        }

        #[derive(Default)]
        struct Accum {
            paper: Option<Paper>,
            score: f32,
            sections: HashMap<String, MatchedSection>,
        }

        let mut fused: HashMap<String, Accum> = HashMap::new();
        for results in results_list {
            for (rank, result) in results.iter().enumerate() {
                let rrf_score = 1.0 / (rrf_k + rank as f32);
                let entry = fused.entry(result.paper.id.clone()).or_default();
                if entry.paper.is_none() {
                    entry.paper = Some(result.paper.clone());
                }
                entry.score += rrf_score;
                for section in &result.matched_sections {
                    entry
                        .sections
                        .entry(section.section_type.clone())
                        .or_insert_with(|| section.clone());
                }
            }
        }

        let mut results: Vec<PaperSearchResult> = fused
            .into_values()
            .filter_map(|accum| {
                let paper = accum.paper?;
                let matched_sections = accum.sections.into_values().take(5).collect();
                Some(PaperSearchResult {
                    paper,
                    score: accum.score,
                    matched_sections,
                })
            })
            .collect();
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        results
    }

    /// Semantic search across all sections or a specific section type.
    pub async fn search(
        &self,
        query: &str,
        section_type: Option<SectionType>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<PaperSearchResult>> {
        if query.trim().is_empty() || top_k == 0 {
            return Ok(vec![]);
        }
        let query_embedding = self.embedding.embed_single(query).await?;
        self.search_with_embedding_internal(
            query,
            &query_embedding.embedding,
            section_type,
            top_k,
            min_score,
        )
        .await
    }

    /// Semantic search with a pre-computed query embedding (e.g., from HyDE).
    pub async fn search_with_embedding(
        &self,
        query: &str,
        query_embedding: &[f32],
        section_type: Option<SectionType>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<PaperSearchResult>> {
        if top_k == 0 {
            return Ok(vec![]);
        }
        self.search_with_embedding_internal(query, query_embedding, section_type, top_k, min_score)
            .await
    }

    /// Search papers using the specified method.
    /// The `min_score` parameter is only used for semantic/hybrid legs.
    pub async fn search_papers_by_method(
        &self,
        query: &str,
        section_type: Option<SectionType>,
        method: SearchMethod,
        limit: usize,
        min_score: f32,
    ) -> Result<Vec<PaperSearchResult>> {
        match method {
            SearchMethod::Fulltext => self.fulltext_search_papers(query, limit).await,
            SearchMethod::Hybrid => {
                self.hybrid_search_adaptive(query, section_type, limit, min_score)
                    .await
            }
            SearchMethod::Semantic => {
                let mut semantic_results =
                    self.search(query, section_type, limit, min_score).await?;
                if semantic_results.is_empty() {
                    semantic_results = self.fulltext_search_papers(query, limit).await?;
                }
                Ok(semantic_results)
            }
        }
    }

    fn should_skip_rerank(vector_count: usize) -> bool {
        vector_count < MIN_DOCS_TO_RERANK
    }

    async fn search_with_embedding_internal(
        &self,
        query: &str,
        query_embedding: &[f32],
        section_type: Option<SectionType>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<PaperSearchResult>> {
        if top_k == 0 {
            return Ok(vec![]);
        }
        let section_filter = section_type
            .as_ref()
            .map(super::super::paper::section::SectionType::as_str);
        let fetch_k = (top_k.saturating_mul(5)).min(500);
        let vector_results = self
            .store
            .search(query_embedding, section_filter, fetch_k, min_score)
            .await?;

        // Neural reranking — skipped when too few results; on reranker
        // failure/timeout, falls back to raw ANN order.
        let reranked_results = if Self::should_skip_rerank(vector_results.len()) {
            vector_results
        } else {
            Self::rerank_chunks(&self.reranker, query, &vector_results, MAX_RERANK_DOCS).await?
        };

        let results = self.aggregate_results(reranked_results, top_k).await?;
        Ok(results)
    }

    /// Find papers similar to a given paper using its stored section vectors.
    /// Does NOT re-embed — reads the paper's existing section vectors from the store.
    pub async fn find_similar(
        &self,
        paper_id: &str,
        section_type: Option<SectionType>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<PaperSearchResult>> {
        if top_k == 0 {
            return Ok(vec![]);
        }
        let st_filter = section_type
            .as_ref()
            .map(super::super::paper::section::SectionType::as_str);
        // Only average section vectors to avoid dilution by figures/tables
        let paper_vectors = self
            .store
            .get_paper_vectors_with_content_type(paper_id, st_filter, Some("section"))
            .await?;

        if paper_vectors.is_empty() {
            return Err(PaperedError::NotFound(
                format!("No vectors found for paper {paper_id} (section: {section_type:?})"),
                None,
            ));
        }

        // Average all vectors for this paper/section to create a single query vector
        let dims = paper_vectors[0].0.len();
        let mut avg_vector = vec![0.0f32; dims];
        for (vec, _) in &paper_vectors {
            for i in 0..dims {
                avg_vector[i] += vec[i];
            }
        }
        let n = paper_vectors.len() as f32;
        for v in &mut avg_vector {
            *v /= n;
        }

        // Search using the averaged vector (no re-embedding)
        let fetch_k = (top_k.saturating_mul(3)).min(500);
        let vector_results = self
            .store
            .search(&avg_vector, st_filter, fetch_k, min_score)
            .await?;

        let mut aggregated = self.aggregate_results(vector_results, top_k).await?;
        // Exclude the source paper
        aggregated.retain(|r| r.paper.id != paper_id);
        aggregated.truncate(top_k);
        Ok(aggregated)
    }

    /// Semantic search for figures across all papers.
    pub async fn search_figures(
        &self,
        query: &str,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<FigureSearchResult>> {
        self.search_multimodal(
            query,
            top_k,
            min_score,
            "figure",
            |store, pid| Box::pin(async move { store.get_figures(&pid).await }),
            |paper, figure, score| FigureSearchResult {
                paper,
                figure,
                score,
            },
        )
        .await
    }

    /// Lexical search for verbatim source-text passages (chunks) across all
    /// papers. Returns raw document fragments rather than LLM-processed
    /// sections, each enriched with its owning paper.
    pub async fn search_passages(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<PassageSearchResult>> {
        if query.trim().is_empty() || top_k == 0 {
            return Ok(vec![]);
        }
        let hits = self.store.search_all_chunks(query, top_k).await?;
        if hits.is_empty() {
            return Ok(vec![]);
        }
        let unique_ids: std::collections::HashSet<&str> =
            hits.iter().map(|h| h.chunk.paper_id.as_str()).collect();
        let paper_ids: Vec<&str> = unique_ids.into_iter().collect();
        let papers = self.store.get_papers_by_ids(&paper_ids).await?;
        let paper_lookup: HashMap<&str, &Paper> =
            papers.iter().map(|p| (p.id.as_str(), p)).collect();

        let mut results: Vec<PassageSearchResult> = hits
            .into_iter()
            .filter_map(|hit| {
                let paper = (*paper_lookup.get(hit.chunk.paper_id.as_str())?).clone();
                Some(PassageSearchResult {
                    paper,
                    chunk: hit.chunk,
                    score: hit.score,
                })
            })
            .collect();
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        results.truncate(top_k);
        Ok(results)
    }

    /// Build the paper relatedness graph over the library.
    ///
    /// Loads up to `limit` indexed papers, most recently updated first, and
    /// computes keyword-overlap + shared-entity similarity edges.
    /// `max_edges_per_node` caps each node's degree so the rendered network
    /// stays readable for large libraries.
    pub async fn paper_graph(&self, limit: usize, max_edges_per_node: usize) -> Result<PaperGraph> {
        let (mut papers, _) = self
            .store
            .list_papers_filtered(
                Some(PaperStatus::Indexed.as_str()),
                None,
                None,
                &Default::default(),
                None,
                true,
                limit,
                0,
            )
            .await?;
        // `list_papers_filtered` reads only the `papers` table; bio-entities
        // live in `paper_entities` and must be batch-loaded for the
        // entity-similarity leg of the edge weight.
        let ids: Vec<String> = papers.iter().map(|p| p.id.clone()).collect();
        let entities = self.store.papers_entities_batch(&ids).await?;
        for paper in papers.iter_mut() {
            if let Some(e) = entities.get(&paper.id) {
                paper.entities = e.clone();
            }
        }
        Ok(build_paper_graph(&papers, max_edges_per_node))
    }

    async fn search_multimodal<T, F, R, B>(
        &self,
        query: &str,
        top_k: usize,
        min_score: f32,
        content_type: &str,
        fetch_items: F,
        build_result: B,
    ) -> Result<Vec<R>>
    where
        T: MultimodalItem,
        F: Fn(Arc<dyn VectorStore>, String) -> Pin<Box<dyn Future<Output = Result<Vec<T>>> + Send>>,
        R: Send,
        B: Fn(Paper, T, f32) -> R + Send,
    {
        if query.trim().is_empty() || top_k == 0 {
            return Ok(vec![]);
        }
        let query_embedding = self.embedding.embed_single(query).await?.embedding;
        let fetch_k = (top_k.saturating_mul(3)).min(500);
        let vector_results = self
            .store
            .search_with_content_type(
                &query_embedding,
                None,
                Some(content_type),
                fetch_k,
                min_score,
            )
            .await?;

        // Multiple hits can belong to the same paper — dedup before the batch
        // fetch so `get_papers_by_ids` is not asked for duplicate ids.
        let unique_ids: HashSet<&str> = vector_results
            .iter()
            .map(|vr| vr.paper_id.as_str())
            .collect();
        let paper_ids: Vec<&str> = unique_ids.into_iter().collect();
        let papers = self.store.get_papers_by_ids(&paper_ids).await?;
        let paper_lookup: HashMap<&str, &Paper> =
            papers.iter().map(|p| (p.id.as_str(), p)).collect();
        let fetched_ids: Vec<&str> = paper_lookup.keys().copied().collect();
        let mut item_cache: HashMap<String, Vec<T>> = HashMap::new();
        let fetch_futures: Vec<_> = fetched_ids
            .iter()
            .map(|pid| fetch_items(self.store.clone(), pid.to_string()))
            .collect();
        for (pid, result) in fetched_ids
            .iter()
            .zip(futures_util::future::join_all(fetch_futures).await)
        {
            match result {
                Ok(items) => {
                    item_cache.insert(pid.to_string(), items);
                }
                Err(e) => {
                    tracing::warn!("{content_type} search: failed to get items for {pid}: {e}");
                }
            }
        }

        let mut item_lookup_cache: HashMap<&str, HashMap<&str, &T>> = HashMap::new();
        for (pid, items) in &item_cache {
            let lookup: HashMap<&str, &T> = items.iter().map(|item| (item.id(), item)).collect();
            item_lookup_cache.insert(pid.as_str(), lookup);
        }

        let mut scored = Vec::new();
        for vr in vector_results {
            let paper = match paper_lookup.get(vr.paper_id.as_str()) {
                Some(p) => p,
                None => {
                    tracing::warn!("{content_type} search: paper {} not found", vr.paper_id);
                    continue;
                }
            };
            if let Some(lookup) = item_lookup_cache.get(vr.paper_id.as_str())
                && let Some(item) = lookup.get(vr.section_type.as_str())
            {
                scored.push(((*paper).clone(), (*item).clone(), vr.score));
            }
        }

        scored.sort_by(|a, b| b.2.total_cmp(&a.2));
        scored.truncate(top_k);
        let results = scored
            .into_iter()
            .map(|(paper, item, score)| build_result(paper, item, score))
            .collect();
        Ok(results)
    }

    /// Full-text search across paper titles, abstracts, and keywords using Turso FTS.
    /// Returns real BM25 scores from Turso's `fts_score()` instead of
    /// heuristically deriving them from result position.
    pub async fn fulltext_search_papers(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<PaperSearchResult>> {
        if query.trim().is_empty() || top_k == 0 {
            return Ok(vec![]);
        }

        let limit = top_k.saturating_mul(2);
        let fts_results = self
            .store
            .fulltext_search_with_snippets(query, limit)
            .await?;

        let boost_factor = KEYWORD_BOOST_FACTOR;
        let mut results: Vec<PaperSearchResult> = fts_results
            .into_iter()
            .map(|(paper, score, snippet)| {
                let boosted = score * compute_keyword_boost(query, &paper.keywords, boost_factor);
                PaperSearchResult {
                    paper,
                    score: boosted,
                    matched_sections: vec![MatchedSection {
                        section_type: "fulltext".to_string(),
                        content_snippet: crate::util::truncate_chars(&snippet, 300).into_owned(),
                        score,
                    }],
                }
            })
            .collect();

        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        results.truncate(top_k);
        Ok(results)
    }

    /// Adaptive hybrid search that tunes semantic weight, min_score, and top-k
    /// based on query complexity analysis.
    pub async fn hybrid_search_adaptive(
        &self,
        query: &str,
        section_type: Option<SectionType>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<PaperSearchResult>> {
        self.hybrid_search_adaptive_with_embedding(
            query,
            None,
            section_type,
            top_k,
            min_score,
            None,
        )
        .await
    }

    /// Adaptive hybrid search with an optional pre-computed embedding (e.g., from
    /// HyDE) and/or an externally analyzed [`QueryProfile`].
    ///
    /// When `profile` is supplied, `top_k` is used as-is (the caller already
    /// applied any complexity-based top-k adjustment); otherwise the profile is
    /// recomputed from `query` and top-k is adjusted here.
    pub async fn hybrid_search_adaptive_with_embedding(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        section_type: Option<SectionType>,
        top_k: usize,
        min_score: f32,
        profile: Option<&QueryProfile>,
    ) -> Result<Vec<PaperSearchResult>> {
        let use_provided_profile = profile.is_some();
        let profile = profile
            .cloned()
            .unwrap_or_else(|| QueryProfile::analyze(query));

        let (semantic_weight, effective_min_score) = match profile.complexity {
            QueryComplexity::Simple => {
                // Very short queries (≤2 words, like "AI") are too generic for
                // keyword matching — BM25 returns many marginal matches that
                // dilute the semantic signal in RRF fusion. Push weight toward
                // semantic and filter noisy FTS results.
                if profile.word_count <= 2 {
                    (0.85, min_score)
                } else {
                    (0.6, min_score)
                }
            }
            QueryComplexity::Normal => (0.5, min_score),
            QueryComplexity::Complex => (0.3, min_score * 0.5),
        };

        let effective_top_k = if use_provided_profile {
            top_k
        } else {
            profile.recommended_top_k(top_k)
        };

        tracing::debug!(
            "Adaptive hybrid: {:?} query, weight={}, top_k={}, min_score={}",
            profile.complexity,
            semantic_weight,
            effective_top_k,
            effective_min_score
        );

        self.hybrid_search_with_embedding_weighted(
            query,
            query_embedding.unwrap_or(&[]),
            section_type,
            effective_top_k,
            effective_min_score,
            semantic_weight,
        )
        .await
    }

    /// Hybrid search with a pre-computed query embedding (e.g., from HyDE).
    /// When `query_embedding` is non-empty, uses it for the semantic leg instead of
    /// re-embedding the query string.
    pub async fn hybrid_search_with_embedding(
        &self,
        query: &str,
        query_embedding: &[f32],
        section_type: Option<SectionType>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<PaperSearchResult>> {
        self.hybrid_search_with_embedding_weighted(
            query,
            query_embedding,
            section_type,
            top_k,
            min_score,
            HYBRID_SEMANTIC_WEIGHT,
        )
        .await
    }

    async fn hybrid_search_with_embedding_weighted(
        &self,
        query: &str,
        query_embedding: &[f32],
        section_type: Option<SectionType>,
        top_k: usize,
        min_score: f32,
        semantic_weight: f32,
    ) -> Result<Vec<PaperSearchResult>> {
        if query.trim().is_empty() || top_k == 0 {
            return Ok(vec![]);
        }

        let (semantic_res, fulltext_res, path_res) = tokio::join!(
            async {
                if query_embedding.is_empty() {
                    self.search(query, section_type, top_k, min_score).await
                } else {
                    self.search_with_embedding(
                        query,
                        query_embedding,
                        section_type,
                        top_k,
                        min_score,
                    )
                    .await
                }
            },
            self.fulltext_search_papers(query, top_k.saturating_mul(2)),
            self.store
                .search_papers_by_path(query, top_k.saturating_mul(2)),
        );

        let semantic_results = match semantic_res {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Semantic search failed in hybrid: {}", e);
                if let Err(ft_err) = &fulltext_res {
                    return Err(PaperedError::Search(format!(
                        "Both hybrid search legs failed. semantic: {e}; fulltext: {ft_err}"
                    )));
                }
                Vec::new()
            }
        };
        let fulltext_results = match fulltext_res {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Fulltext search failed in hybrid: {}", e);
                if semantic_results.is_empty() {
                    return Err(PaperedError::Search(format!(
                        "Hybrid search failed and semantic leg returned no results: {e}"
                    )));
                }
                Vec::new()
            }
        };

        // Filter out low-quality FTS results that would pollute RRF fusion.
        // For short generic queries the BM25 scores are often flat and
        // noisy — keep only results with a meaningful signal.
        let fulltext_results = if fulltext_results.len() > top_k {
            let max_ft = fulltext_results.first().map(|r| r.score).unwrap_or(0.0);
            if max_ft > 0.0 {
                let threshold = if semantic_weight > 0.7 {
                    max_ft * 0.15
                } else {
                    0.0
                };
                fulltext_results
                    .into_iter()
                    .filter(|r| r.score >= threshold)
                    .collect()
            } else {
                fulltext_results
            }
        } else {
            fulltext_results
        };

        // Heading-path channel: papers whose section/chapter paths match the
        // query (e.g. "in the Methods section"). Non-fatal on failure.
        let path_results = match path_res {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Path search failed in hybrid: {}", e);
                Vec::new()
            }
        };
        let path_paper_results: Vec<PaperSearchResult> = if path_results.is_empty() {
            Vec::new()
        } else {
            let score_map: HashMap<&str, f32> = path_results
                .iter()
                .map(|(id, s)| (id.as_str(), *s))
                .collect();
            let ids: Vec<&str> = path_results.iter().map(|(id, _)| id.as_str()).collect();
            let papers = self.store.get_papers_by_ids(&ids).await.unwrap_or_default();
            papers
                .into_iter()
                .filter_map(|p| {
                    let s = score_map.get(p.id.as_str()).copied().unwrap_or(0.0);
                    if s <= 0.0 {
                        return None;
                    }
                    Some(PaperSearchResult {
                        paper: p,
                        score: s,
                        matched_sections: vec![MatchedSection {
                            section_type: "path".to_string(),
                            content_snippet: String::new(),
                            score: s,
                        }],
                    })
                })
                .collect()
        };

        // Reciprocal Rank Fusion (RRF)
        let rrf_k = RRF_K;
        let semantic_mul = semantic_weight * 2.0;
        let fulltext_mul = (1.0 - semantic_weight) * 2.0;

        // Collect all referenced papers once and fuse by paper_id without cloning
        // papers during the fusion loop.
        let mut paper_lookup: HashMap<&str, &Paper> = HashMap::new();
        for r in &semantic_results {
            paper_lookup.entry(r.paper.id.as_str()).or_insert(&r.paper);
        }
        for r in &fulltext_results {
            paper_lookup.entry(r.paper.id.as_str()).or_insert(&r.paper);
        }
        for r in &path_paper_results {
            paper_lookup.entry(r.paper.id.as_str()).or_insert(&r.paper);
        }

        // Aggregate matched sections by reference during fusion to avoid cloning
        // sections that will later be discarded. Only the top-5 sections are cloned
        // when the final result vector is built.
        let mut fused: HashMap<&str, (f32, Vec<&MatchedSection>)> = HashMap::new();

        // RRF: semantic results
        fuse_rrf_results(
            &semantic_results,
            &mut fused,
            rrf_k,
            semantic_mul,
            |sections, matched| {
                let mut seen: HashSet<String> =
                    sections.iter().map(|s| s.section_type.clone()).collect();
                for sk in matched {
                    if seen.insert(sk.section_type.clone()) {
                        sections.push(sk);
                    }
                }
            },
        );

        // RRF: fulltext results
        fuse_rrf_results(
            &fulltext_results,
            &mut fused,
            rrf_k,
            fulltext_mul,
            |sections, matched| {
                let seen: HashSet<String> =
                    sections.iter().map(|s| s.section_type.clone()).collect();
                if !seen.contains("fulltext") {
                    sections.extend(matched.iter());
                }
            },
        );

        // RRF: heading-path channel. Smaller weight than the two main legs so
        // it breaks ties and surfaces section-named queries without dominating.
        const PATH_RRF_WEIGHT: f32 = 0.5;
        fuse_rrf_results(
            &path_paper_results,
            &mut fused,
            rrf_k,
            PATH_RRF_WEIGHT,
            |sections, matched| {
                let seen: HashSet<String> =
                    sections.iter().map(|s| s.section_type.clone()).collect();
                if !seen.contains("path") {
                    sections.extend(matched.iter());
                }
            },
        );

        let mut results: Vec<PaperSearchResult> = fused
            .into_iter()
            .filter_map(|(paper_id, (score, matched_sections))| {
                paper_lookup.get(paper_id).map(|&paper| PaperSearchResult {
                    paper: paper.clone(),
                    score,
                    matched_sections: matched_sections.into_iter().take(5).cloned().collect(),
                })
            })
            .collect();

        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        let max_score = results.first().map_or(1.0, |r| r.score);
        if max_score > 0.0 {
            for r in &mut results {
                r.score /= max_score;
            }
        }
        results.truncate(top_k);
        Ok(results)
    }

    /// Rerank chunk-level vector results using a neural cross-encoder.
    ///
    /// Any reranker failure (HTTP error or `skip_timeout_secs` timeout) is
    /// non-fatal: log a warning and fall back to the pre-rerank ANN ordering
    /// so no search path fails outright because of the reranker.
    async fn rerank_chunks(
        reranker: &RerankerClient,
        query: &str,
        vector_results: &[crate::store::vector::VectorSearchResult],
        max_rerank_docs: usize,
    ) -> Result<Vec<crate::store::vector::VectorSearchResult>> {
        // Build documents, skipping empty chunks (figures without text captions).
        // SiliconFlow's reranker rejects empty document strings with code 20015.
        let mut documents: Vec<String> = Vec::new();
        let mut doc_indices: Vec<usize> = Vec::new();
        for (i, r) in vector_results.iter().enumerate().take(max_rerank_docs) {
            let trimmed = r.chunk_text.trim();
            if !trimmed.is_empty() {
                documents.push(trimmed.to_string());
                doc_indices.push(i);
            }
        }

        // If every chunk text is empty, fall back to original vector results
        // (e.g., figures without captions).
        if documents.is_empty() {
            return Ok(vector_results.to_vec());
        }

        let rerank_results = match reranker.rerank(query, &documents).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Reranking failed, using raw ANN ordering: {e}");
                return Ok(vector_results.to_vec());
            }
        };

        let mut reranked: Vec<crate::store::vector::VectorSearchResult> =
            Vec::with_capacity(rerank_results.len());
        for result in rerank_results {
            if let Some(&original_idx) = doc_indices.get(result.index)
                && let Some(original) = vector_results.get(original_idx)
            {
                let mut updated = (*original).clone();
                updated.score = result.relevance_score;
                reranked.push(updated);
            }
        }

        // Append any results beyond max_rerank_docs with their original scores
        // so no vector results are lost.
        for r in vector_results.iter().skip(max_rerank_docs) {
            reranked.push((*r).clone());
        }

        Ok(reranked)
    }

    // === Private helpers ===

    async fn aggregate_results(
        &self,
        vector_results: Vec<crate::store::vector::VectorSearchResult>,
        top_k: usize,
    ) -> Result<Vec<PaperSearchResult>> {
        let mut paper_map: HashMap<String, Vec<crate::store::vector::VectorSearchResult>> =
            HashMap::new();

        for result in vector_results {
            paper_map
                .entry(result.paper_id.clone())
                .or_default()
                .push(result);
        }

        let mut results: Vec<PaperSearchResult> = Vec::new();
        // Batch-load all papers in a single query
        let paper_id_refs: Vec<&str> = paper_map.keys().map(std::string::String::as_str).collect();
        let papers = self.store.get_papers_by_ids(&paper_id_refs).await?;
        let paper_lookup: HashMap<&str, &Paper> =
            papers.iter().map(|p| (p.id.as_str(), p)).collect();

        for (paper_id, matches) in paper_map {
            if let Some(&paper) = paper_lookup.get(paper_id.as_str()) {
                let max_score = matches.iter().map(|m| m.score).fold(0.0f32, f32::max);
                let matched_sections: Vec<MatchedSection> = matches
                    .into_iter()
                    .map(|m| {
                        let display_type = if m.content_type.as_str() == "figure" {
                            m.content_type.clone()
                        } else {
                            m.section_type.clone()
                        };
                        MatchedSection {
                            section_type: display_type,
                            content_snippet: crate::util::truncate_chars(&m.chunk_text, 300)
                                .into_owned(),
                            score: m.score,
                        }
                    })
                    .collect();

                results.push(PaperSearchResult {
                    paper: paper.clone(),
                    score: max_score,
                    matched_sections,
                });
            }
        }

        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        results.truncate(top_k);
        Ok(results)
    }
}

fn fuse_rrf_results<'a>(
    results: &'a [PaperSearchResult],
    fused: &mut HashMap<&'a str, (f32, Vec<&'a MatchedSection>)>,
    rrf_k: f32,
    weight: f32,
    merge_sections: impl Fn(&mut Vec<&'a MatchedSection>, &'a [MatchedSection]),
) {
    for (rank, result) in results.iter().enumerate() {
        let rrf_score = 1.0 / (rrf_k + rank as f32) * weight;
        fused
            .entry(result.paper.id.as_str())
            .and_modify(|(score, sections)| {
                *score += rrf_score;
                merge_sections(sections, &result.matched_sections);
            })
            .or_insert_with(|| (rrf_score, result.matched_sections.iter().collect()));
    }
}

/// Compute a score multiplier based on how many query tokens match a paper's keywords.
///
/// Each query token (word) that appears as a substring of any keyword (case-insensitive)
/// counts as one match. The multiplier is `1.0 + boost_factor * min(match_count, 5)`.
fn compute_keyword_boost(query: &str, keywords: &[String], boost_factor: f32) -> f32 {
    if keywords.is_empty() {
        return 1.0;
    }

    let query_lower = query.to_lowercase();
    let kw_lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();

    let match_count = query_lower
        .split_whitespace()
        .filter(|word| word.len() >= 2)
        .filter(|word| kw_lower.iter().any(|kw| kw.contains(*word)))
        .count()
        .min(5);

    if match_count == 0 {
        1.0
    } else {
        1.0 + boost_factor * match_count as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelEndpoint;
    use crate::llm::embed::EmbeddingClient;
    use crate::llm::reranker::{RerankerClient, RerankerConfig};
    use crate::store::vector::VectorSearchResult;
    use crate::test_support::MockVectorStore;

    /// Six ANN results in descending score order, one per paper (p1..p6).
    fn ann_results() -> Vec<VectorSearchResult> {
        (1..=6)
            .map(|i| VectorSearchResult {
                paper_id: format!("p{i}"),
                section_type: "body".to_string(),
                score: 1.0 - i as f32 * 0.1,
                chunk_text: format!("chunk text {i}"),
                content_type: "section".to_string(),
            })
            .collect()
    }

    fn dummy_embedding() -> EmbeddingClient {
        EmbeddingClient::new(
            "http://127.0.0.1:1",
            None::<String>,
            "test-embedding",
            &crate::config::EmbeddingConfig::default(),
        )
        .unwrap()
    }

    fn make_reranker(api_base: String, skip_timeout_secs: Option<u64>) -> RerankerClient {
        let config = RerankerConfig {
            skip_timeout_secs,
            ..RerankerConfig::default()
        };
        let endpoint = ModelEndpoint {
            api_base,
            api_key: None,
            model: "test-reranker".to_string(),
            concurrency: 0,
            rpm: 0,
            tpm: 0,
            extra_body: None,
            reasoning_effort: None,
            context_window: None,
            max_output_tokens: None,
        };
        RerankerClient::new(&config, &endpoint).unwrap()
    }

    /// Search with a store returning six ANN hits and the given reranker.
    /// The search must succeed even when the reranker is unusable.
    async fn search_with_reranker(reranker: RerankerClient) -> Vec<PaperSearchResult> {
        let store = MockVectorStore::default();
        for i in 1..=6 {
            let mut paper = Paper::new(format!("Paper {i}"));
            paper.id = format!("p{i}");
            store.insert_paper(&paper).await.unwrap();
        }
        store.set_search_results(ann_results());
        let store: Arc<dyn VectorStore> = Arc::new(store);
        let engine = SearchEngine::new(store, dummy_embedding(), reranker);
        engine
            .search_with_embedding("attention", &[0.1, 0.2, 0.3], None, 5, 0.0)
            .await
            .expect("reranker failure must not fail the search")
    }

    #[tokio::test]
    async fn rerank_failure_falls_back_to_ann_order() {
        // Port 1 refuses connections immediately.
        let results =
            search_with_reranker(make_reranker("http://127.0.0.1:1".to_string(), None)).await;
        let ids: Vec<&str> = results.iter().map(|r| r.paper.id.as_str()).collect();
        assert_eq!(ids, ["p1", "p2", "p3", "p4", "p5"]);
    }

    #[tokio::test]
    async fn rerank_timeout_falls_back_to_ann_order() {
        // A server that accepts connections but never responds forces the
        // reranker to hang until skip_timeout_secs elapses.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                drop(stream);
            }
        });

        let reranker = make_reranker(format!("http://127.0.0.1:{port}"), Some(1));
        let start = std::time::Instant::now();
        let results = search_with_reranker(reranker).await;
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "skip_timeout_secs=1 should abort the rerank quickly, took {:?}",
            start.elapsed()
        );
        let ids: Vec<&str> = results.iter().map(|r| r.paper.id.as_str()).collect();
        assert_eq!(ids, ["p1", "p2", "p3", "p4", "p5"]);
    }

    fn make_result(id: &str, score: f32) -> PaperSearchResult {
        let mut paper = Paper::new(format!("Paper {id}"));
        paper.id = id.to_string();
        PaperSearchResult {
            paper,
            score,
            matched_sections: vec![MatchedSection {
                section_type: "body".to_string(),
                content_snippet: format!("snippet {id}"),
                score,
            }],
        }
    }

    /// Three indexed papers sharing a keyword/gene, oldest first, plus one
    /// processing paper that must be excluded from the graph.
    async fn graph_engine() -> SearchEngine {
        let store = MockVectorStore::default();
        for (id, days_old) in [("p1", 3i64), ("p2", 2), ("p3", 1)] {
            let mut paper = Paper::new(format!("Paper {id}"));
            paper.id = id.to_string();
            paper.keywords = vec!["crispr".to_string()];
            paper.entities.genes = vec!["BRCA1".to_string()];
            paper.updated_at = chrono::Utc::now() - chrono::Duration::days(days_old);
            store.insert_paper(&paper).await.unwrap();
        }
        let mut processing = Paper::new("Still processing".to_string());
        processing.id = "p4".to_string();
        processing.status = PaperStatus::Processing;
        processing.keywords = vec!["crispr".to_string()];
        store.insert_paper(&processing).await.unwrap();

        let store: Arc<dyn VectorStore> = Arc::new(store);
        SearchEngine::new(
            store,
            dummy_embedding(),
            make_reranker("http://127.0.0.1:1".to_string(), None),
        )
    }

    #[tokio::test]
    async fn paper_graph_includes_only_indexed_papers() {
        let engine = graph_engine().await;
        let graph = engine.paper_graph(10, 10).await.expect("graph");
        let mut ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        ids.sort_unstable();
        // The processing paper (p4) must not appear; the three indexed papers
        // must, and their shared keyword/gene must produce edges.
        assert_eq!(ids, ["p1", "p2", "p3"]);
        assert!(
            !graph.edges.is_empty(),
            "papers sharing keywords/entities must be linked"
        );
    }

    #[tokio::test]
    async fn paper_graph_limit_keeps_most_recent() {
        let engine = graph_engine().await;
        let graph = engine.paper_graph(2, 10).await.expect("graph");
        let mut ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        ids.sort_unstable();
        // p3 (1 day old) and p2 (2 days) are more recent than p1 (3 days).
        assert_eq!(ids, ["p2", "p3"], "limit must keep the most recent papers");
    }

    #[test]
    fn rrf_fusion_overlapping_lists() {
        let a = vec![make_result("p1", 1.0), make_result("p2", 0.9)];
        let b = vec![make_result("p2", 0.8), make_result("p3", 0.7)];
        let fused = SearchEngine::fuse_paper_results_rrf(&[a, b], 60.0);
        let ids: Vec<&str> = fused.iter().map(|r| r.paper.id.as_str()).collect();
        assert_eq!(ids, vec!["p2", "p1", "p3"]);
        // p2 appears in both lists, so it should have the highest fused score.
        assert!(fused[0].score > fused[1].score);
        assert!(fused[0].score > fused[2].score);
    }

    #[test]
    fn rrf_fusion_disjoint_lists() {
        let a = vec![make_result("p1", 1.0), make_result("p2", 0.9)];
        let b = vec![make_result("p3", 0.8), make_result("p4", 0.7)];
        let fused = SearchEngine::fuse_paper_results_rrf(&[a, b], 60.0);
        let ids: Vec<&str> = fused.iter().map(|r| r.paper.id.as_str()).collect();
        // p1/p3 share the top rank score; p2/p4 share the second rank score.
        let mut sorted_ids = ids.clone();
        sorted_ids.sort();
        assert_eq!(sorted_ids, vec!["p1", "p2", "p3", "p4"]);
        // Higher-scored items must precede lower-scored items.
        let pos = |id: &str| ids.iter().position(|x| *x == id).unwrap();
        assert!(pos("p1") < pos("p2"));
        assert!(pos("p3") < pos("p4"));
    }

    #[test]
    fn rrf_fusion_identical_lists() {
        let a = vec![make_result("p1", 1.0), make_result("p2", 0.9)];
        let b = a.clone();
        let fused = SearchEngine::fuse_paper_results_rrf(&[a, b], 60.0);
        let ids: Vec<&str> = fused.iter().map(|r| r.paper.id.as_str()).collect();
        assert_eq!(ids, vec!["p1", "p2"]);
        // Each paper appears twice, so scores are doubled but order is preserved.
        assert!(fused[0].score > fused[1].score);
    }

    #[test]
    fn rrf_fusion_single_list_is_unchanged() {
        let a = vec![make_result("p1", 1.0), make_result("p2", 0.9)];
        let fused = SearchEngine::fuse_paper_results_rrf(&[a], 60.0);
        let ids: Vec<&str> = fused.iter().map(|r| r.paper.id.as_str()).collect();
        assert_eq!(ids, vec!["p1", "p2"]);
    }

    #[test]
    fn rrf_fusion_empty_list_returns_empty() {
        let fused = SearchEngine::fuse_paper_results_rrf(&[], 60.0);
        assert!(fused.is_empty());
    }
}
