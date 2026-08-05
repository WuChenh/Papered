use crate::Prompt;
use crate::error::Result;
use crate::paper::Paper;
use crate::paper::section::PaperSections;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Unified vector storage trait. Implemented by TursoStore.
///
/// # Why `async_trait` instead of native `async fn` in traits (RPITIT)?
///
/// Rust 1.75+ stabilizes `async fn` in traits, but the returned future is an
/// opaque `impl Future` which is **not** object-safe. Since this trait is used
/// as `Arc<dyn VectorStore>` throughout the daemon and MCP crates, we must
/// retain `#[async_trait]` (which boxes the future) to preserve dyn-compatibility.
/// If the codebase ever moves to static dispatch (`impl VectorStore` generics
/// only), this attribute can be removed.
/// A paper reference carrying both id and title, for health-check listings
/// that need to display identifiable papers rather than bare UUIDs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaperRef {
    pub id: String,
    pub title: String,
}

/// A figure whose stored image file is missing from disk.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MissingFigureImage {
    pub paper_id: String,
    pub figure_id: String,
    pub paper_title: String,
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    // === Vector operations ===

    /// Upsert vector records (paper_id, section_type, vector, chunk_text).
    async fn upsert(&self, records: &[VectorRecord]) -> Result<()>;

    /// Search for similar vectors.
    async fn search(
        &self,
        query: &[f32],
        section_type: Option<&str>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<VectorSearchResult>> {
        self.search_with_content_type(query, section_type, None, top_k, min_score)
            .await
    }

    /// Search for similar vectors with content type filter.
    async fn search_with_content_type(
        &self,
        query: &[f32],
        section_type: Option<&str>,
        content_type: Option<&str>,
        top_k: usize,
        min_score: f32,
    ) -> Result<Vec<VectorSearchResult>>;

    /// Delete all vectors for a paper from the vector index.
    async fn delete_by_paper(&self, paper_id: &str) -> Result<()>;

    /// Delete vectors for a paper filtered by content_type (e.g., "section" only).
    async fn delete_by_paper_and_content_type(
        &self,
        paper_id: &str,
        content_type: &str,
    ) -> Result<()>;

    /// Count total vectors in store.
    async fn count(&self) -> Result<usize>;

    // === Paper metadata operations ===

    async fn insert_paper(&self, paper: &Paper) -> Result<()>;
    async fn get_paper(&self, paper_id: &str) -> Result<Option<Paper>>;
    async fn get_papers_by_ids(&self, ids: &[&str]) -> Result<Vec<Paper>>;
    async fn get_paper_by_file_hash(&self, file_hash: &str) -> Result<Option<Paper>>;
    async fn list_papers(&self, limit: usize, offset: usize) -> Result<Vec<Paper>>;
    #[allow(clippy::too_many_arguments)]
    async fn list_papers_filtered(
        &self,
        _status: Option<&str>,
        _paper_type: Option<&str>,
        _keyword: Option<&str>,
        _entity_filter: &crate::paper::EntityFilter,
        _sort_by: Option<&str>,
        _sort_desc: bool,
        _limit: usize,
        _offset: usize,
    ) -> Result<(Vec<Paper>, usize)>;
    async fn paper_count(&self) -> Result<usize>;
    async fn count_papers_by_status(&self, status: &str) -> Result<usize>;
    async fn delete_paper(&self, paper_id: &str) -> Result<()>;
    /// Delete many papers in a single transaction. Cascaded deletes (chunks,
    /// figures, translations, vectors) hit the tantivy-backed FTS index once
    /// per commit; batching N papers collapses N commits into one.
    async fn delete_papers(&self, paper_ids: &[&str]) -> Result<()>;
    async fn update_paper_status(
        &self,
        paper_id: &str,
        status: &str,
        error_message: Option<&str>,
        retry_count: Option<u32>,
    ) -> Result<()>;

    async fn update_paper_cover(&self, paper_id: &str, cover_path: &str) -> Result<()>;
    async fn set_paper_embedding_model(&self, paper_id: &str, embedding_model: &str) -> Result<()>;
    async fn update_paper(&self, paper: &Paper) -> Result<()>;
    async fn update_prompt(&self, prompt: &crate::Prompt) -> Result<()>;

    // === Bio-entities ===

    /// Replace all bio-entities for a paper (delete + insert in one
    /// transaction — reindex-safe). Default is a no-op for stores without
    /// entity support.
    async fn set_paper_entities(
        &self,
        _paper_id: &str,
        _entities: &crate::paper::BioEntities,
    ) -> Result<()> {
        Ok(())
    }

    /// Load the bio-entities extracted for a paper. Default: empty.
    async fn paper_entities(&self, _paper_id: &str) -> Result<crate::paper::BioEntities> {
        Ok(crate::paper::BioEntities::default())
    }

    /// Batch-load bio-entities for multiple papers, keyed by paper ID.
    /// Papers without entities are omitted from the map. Default: one
    /// [`paper_entities`](Self::paper_entities) call per ID.
    async fn papers_entities_batch(
        &self,
        paper_ids: &[String],
    ) -> Result<std::collections::HashMap<String, crate::paper::BioEntities>> {
        let mut out = std::collections::HashMap::new();
        for id in paper_ids {
            let entities = self.paper_entities(id).await?;
            if !entities.is_empty() {
                out.insert(id.clone(), entities);
            }
        }
        Ok(out)
    }

    /// Paper IDs carrying an exact (case-sensitive) `kind`/`value` entity
    /// match. `kind` is one of "species", "gene", "technique", "pathway".
    /// Default returns nothing.
    async fn paper_ids_by_entity(&self, _kind: &str, _value: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Paper IDs with the given paper_type. Used to filter search results.
    /// Default returns nothing.
    async fn paper_ids_by_paper_type(&self, _paper_type: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    // === Ratings & comments (user annotations) ===

    /// Read the user's star rating (1–5) for a paper, if any. Default: none.
    async fn get_paper_rating(&self, _paper_id: &str) -> Result<Option<i64>> {
        Ok(None)
    }

    /// Create or update the user's star rating (1–5) for a paper.
    /// Default is a no-op.
    async fn set_paper_rating(&self, _paper_id: &str, _rating: i64) -> Result<()> {
        Ok(())
    }

    /// Delete the user's rating for a paper. Default is a no-op.
    async fn delete_paper_rating(&self, _paper_id: &str) -> Result<()> {
        Ok(())
    }

    /// List the user's comments on a paper, oldest first. Default: empty.
    async fn list_paper_comments(&self, _paper_id: &str) -> Result<Vec<PaperComment>> {
        Ok(Vec::new())
    }

    /// Add a comment to a paper, returning the stored record. Default returns
    /// an empty record for stores without comment support.
    async fn add_paper_comment(&self, paper_id: &str, content: &str) -> Result<PaperComment> {
        Ok(PaperComment {
            id: 0,
            paper_id: paper_id.to_string(),
            content: content.to_string(),
            created_at: String::new(),
        })
    }

    /// Delete a comment by id, scoped to a paper so callers cannot delete
    /// comments belonging to other papers. Default is a no-op.
    async fn delete_paper_comment(&self, _paper_id: &str, _comment_id: i64) -> Result<()> {
        Ok(())
    }

    /// Batch-read annotation state for a set of papers in one call —
    /// the list-view replacement for per-paper `get_paper_rating` +
    /// `list_paper_comments` round trips (2 requests per row).
    ///
    /// The returned map has an entry for every requested id that carries at
    /// least one annotation (a rating or one or more comments). Ids with no
    /// annotations — including unknown ids — are absent; callers treat a
    /// missing entry as "no rating, no comments". Stores must implement this
    /// with a single aggregate query, not a per-id loop. Default: empty.
    async fn annotation_summaries(
        &self,
        paper_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, AnnotationSummary>> {
        let _ = paper_ids;
        Ok(std::collections::HashMap::new())
    }

    // === Section operations ===

    async fn insert_sections(&self, paper_id: &str, sections: &PaperSections) -> Result<()>;
    async fn get_sections(&self, paper_id: &str) -> Result<PaperSections>;
    async fn delete_sections(&self, paper_id: &str) -> Result<()>;
    async fn get_sections_batch(&self, paper_ids: &[&str]) -> Result<Vec<PaperSections>> {
        let mut results = Vec::with_capacity(paper_ids.len());
        for id in paper_ids {
            results.push(self.get_sections(id).await?);
        }
        Ok(results)
    }
    // === Vector retrieval ===

    /// Retrieve all vectors for a paper, optionally filtered by section type and content type.
    /// Returns (vector, chunk_text) pairs.
    async fn get_paper_vectors_with_content_type(
        &self,
        paper_id: &str,
        section_type: Option<&str>,
        content_type: Option<&str>,
    ) -> Result<Vec<(Vec<f32>, String)>>;

    /// Full-text search returning papers with BM25 scores and highlighted snippets.
    async fn fulltext_search_with_snippets(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(Paper, f32, String)>>;

    // === Chunks ===

    async fn insert_chunks(&self, paper_id: &str, chunks: &[crate::chunker::Chunk]) -> Result<()>;
    async fn get_chunks(&self, paper_id: &str) -> Result<Vec<crate::chunker::Chunk>>;

    /// Fetch a single chunk by id within a paper. Returns `None` when absent.
    /// Default scans `get_chunks`; stores should override with a targeted query.
    async fn get_chunk(
        &self,
        paper_id: &str,
        chunk_id: &str,
    ) -> Result<Option<crate::chunker::Chunk>> {
        Ok(self
            .get_chunks(paper_id)
            .await?
            .into_iter()
            .find(|c| c.id == chunk_id))
    }

    /// Retrieve the ancestor chain chunks for a set of chunk IDs within a paper.
    /// Returns the requested chunks plus every ancestor reachable via `parent_id`.
    async fn get_chunk_ancestors(
        &self,
        paper_id: &str,
        _chunk_ids: &[&str],
    ) -> Result<Vec<crate::chunker::Chunk>> {
        // Default implementation falls back to loading all chunks.
        self.get_chunks(paper_id).await
    }

    async fn delete_chunks(&self, paper_id: &str) -> Result<()>;

    // === Prompts ===

    async fn list_prompts(&self) -> Result<Vec<Prompt>>;
    async fn get_prompt(&self, prompt_id: &str) -> Result<Option<Prompt>>;
    async fn get_default_prompt(&self) -> Result<Option<Prompt>>;
    async fn insert_prompt(&self, prompt: &Prompt) -> Result<()>;
    async fn delete_prompt(&self, prompt_id: &str) -> Result<()>;
    async fn set_default_prompt(&self, prompt_id: &str) -> Result<()>;

    // === Chunk lexical search ===

    /// Search chunks via full-text search within a set of papers.
    async fn search_chunks(
        &self,
        paper_ids: &[&str],
        query: &str,
        limit: usize,
    ) -> Result<Vec<ChunkHit>>;

    /// Search chunks via full-text search across ALL papers (no paper filter).
    /// Used by the passages search channel, which surfaces verbatim source-text
    /// fragments rather than LLM-processed sections. Default returns nothing;
    /// stores with an FTS index over chunk content should override.
    async fn search_all_chunks(&self, _query: &str, _limit: usize) -> Result<Vec<ChunkHit>> {
        Ok(Vec::new())
    }

    /// Paper-level heading-path search: match the query against chunk heading
    /// paths (e.g. "Methods > Transformer Architecture") and return the best
    /// `(paper_id, score)` per paper, highest first. Used as a retrieval
    /// channel for queries that name a section. Default returns nothing.
    async fn search_papers_by_path(
        &self,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        Ok(Vec::new())
    }

    // === Figures ===

    async fn insert_figures(
        &self,
        paper_id: &str,
        figures: &[crate::index::multimodal::FigureInfo],
    ) -> Result<()>;
    async fn get_figures(
        &self,
        paper_id: &str,
    ) -> Result<Vec<crate::index::multimodal::FigureInfo>>;
    async fn delete_figures(&self, _paper_id: &str) -> Result<()>;

    /// Update the image_path of a single figure.
    async fn update_figure_image_path(&self, figure_id: &str, image_path: &str) -> Result<()> {
        let _ = figure_id;
        let _ = image_path;
        Ok(())
    }
    // === Health check ===

    async fn papers_without_vectors(&self) -> Result<Vec<PaperRef>>;
    async fn orphaned_vector_paper_ids(&self) -> Result<Vec<String>>;
    async fn papers_with_missing_files(&self) -> Result<Vec<PaperRef>>;
    async fn figures_with_missing_images(
        &self,
        data_dir: &std::path::Path,
    ) -> Result<Vec<MissingFigureImage>>;
    /// Find directories under data_dir/papers that are not referenced by any paper in the DB.
    async fn orphaned_data_directories(&self, data_dir: &std::path::Path) -> Result<Vec<String>>;

    /// Get papers by their indexing status.
    async fn get_papers_by_status(&self, status: &str) -> Result<Vec<Paper>>;
    /// Get papers by status with retry_count below the given maximum.
    async fn get_papers_by_status_with_retry_below(
        &self,
        status: &str,
        max_retry: u32,
    ) -> Result<Vec<Paper>>;

    async fn figure_count(&self) -> Result<usize>;

    // === Maintenance ===

    /// Optimize the underlying storage (compaction, index refresh, etc.).
    /// Default is a no-op.
    async fn optimize(&self) -> Result<()> {
        Ok(())
    }

    /// Return the current embedding dimension of the vector table.
    async fn store_dimension(&self) -> Option<usize> {
        None
    }

    /// Clear all vectors from the store and recreate the table with the given dimension.
    async fn clear_all_vectors(&self, _new_dimension: usize) -> Result<()> {
        Ok(())
    }

    /// Clear all data (papers, chunks, vectors, metadata) for a full factory reset.
    async fn clear_all_data(&self) -> Result<()>;

    /// Return a lightweight view of all papers for duplicate detection.
    async fn duplicate_scan_papers(&self) -> Result<Vec<DuplicatePaperInfo>>;

    /// Store a metadata key-value pair.
    async fn set_meta(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }

    /// Read a metadata value by key.
    async fn get_meta(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    // === LLM call metrics ===

    /// Persist one LLM/embedding/rerank call metric. Default is a no-op so
    /// test stores and alternate backends need no instrumentation.
    async fn insert_llm_call_metric(
        &self,
        _metric: &crate::llm::metrics::LlmCallMetric,
    ) -> Result<()> {
        Ok(())
    }

    /// Aggregate call metrics grouped by (kind, model). When `since` is set,
    /// only calls recorded at or after that timestamp are included.
    /// Default returns an empty list.
    async fn llm_call_metrics_summary(
        &self,
        _since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<crate::llm::metrics::LlmCallMetricGroup>> {
        Ok(Vec::new())
    }

    // === Translations ===

    /// Insert or update a translation. Uses UPSERT on the unique constraint
    /// `(paper_id, content_type, content_ref, target_language)`.
    async fn upsert_translation(&self, t: &TranslationInfo) -> Result<()> {
        let _ = t;
        Ok(())
    }

    /// Get all translations for a paper in a given language.
    async fn get_translations(&self, _paper_id: &str, _lang: &str) -> Result<Vec<TranslationInfo>> {
        Ok(Vec::new())
    }

    /// Get a single translation by its composite key.
    async fn get_translation(
        &self,
        _paper_id: &str,
        _content_type: &str,
        _content_ref: &str,
        _lang: &str,
    ) -> Result<Option<TranslationInfo>> {
        Ok(None)
    }

    /// Delete all translations for a paper.
    async fn delete_translations(&self, _paper_id: &str) -> Result<()> {
        Ok(())
    }

    /// Full-text search across translated content.
    async fn search_translations(
        &self,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<TranslationInfo>> {
        Ok(Vec::new())
    }
}

/// Lightweight paper metadata used for duplicate detection scans.
#[derive(Debug, Clone)]
pub struct DuplicatePaperInfo {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub published_date: Option<String>,
    pub file_hash: Option<String>,
    pub status: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// A chunk matched by full-text lexical search, with its relevance score.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkHit {
    pub chunk: crate::chunker::Chunk,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct VectorRecord {
    pub paper_id: String,
    pub section_type: String,
    pub vector: Vec<f32>,
    pub chunk_text: String,
    pub content_type: String,
}

impl VectorRecord {
    pub fn section(
        paper_id: String,
        section_type: String,
        vector: Vec<f32>,
        chunk_text: String,
    ) -> Self {
        Self {
            paper_id,
            section_type,
            vector,
            chunk_text,
            content_type: "section".to_string(),
        }
    }

    pub fn figure(
        paper_id: String,
        figure_id: String,
        vector: Vec<f32>,
        chunk_text: String,
    ) -> Self {
        Self {
            paper_id,
            section_type: figure_id,
            vector,
            chunk_text,
            content_type: "figure".to_string(),
        }
    }
}

/// Result of a vector similarity search.
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    pub paper_id: String,
    pub section_type: String,
    /// Cosine similarity score (0.0–1.0), derived from L2 distance.
    /// Higher values indicate greater similarity.
    pub score: f32,
    pub chunk_text: String,
    pub content_type: String,
}

/// A user-authored comment attached to a paper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperComment {
    pub id: i64,
    pub paper_id: String,
    pub content: String,
    pub created_at: String,
}

/// Aggregated user-annotation state for one paper, as shown on list views.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationSummary {
    /// The user's star rating (1–5), if any.
    pub rating: Option<i64>,
    /// Number of user comments on the paper.
    pub comment_count: i64,
}

/// A stored translation of a paper's content piece.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationInfo {
    pub id: i64,
    pub paper_id: String,
    pub content_type: String,
    pub content_ref: String,
    pub source_hash: String,
    pub target_language: String,
    pub translated_text: String,
    pub model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
