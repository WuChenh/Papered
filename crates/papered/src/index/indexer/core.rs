//! Core indexing pipeline — `Indexer` struct and main document ingestion methods.

use crate::config::AppConfig;
use crate::error::{PaperedError, Result};
use crate::index::indexer::helpers;
use crate::llm::embed::{BatchingEmbedder, EmbeddingClient};
use crate::llm::rate_limiter::RateLimiter;
use crate::paper::mineru::MinerUClient;
use crate::paper::parser::{
    ExtractedText, ExtractorSource, IndexAction, PaperMetadata, RichExtraction, compute_file_hash,
    preprocess_text, sanitize_paper_metadata,
};
use crate::paper::processor::split_text_windows;
use crate::paper::section::{PaperSections, Section};
use crate::paper::source::{DocumentSource, extract_document_text};
use crate::paper::{Paper, PaperStatus};
use crate::store::vector::{VectorRecord, VectorStore};
use crate::util::str_enum::StrLabel;
use indexmap::IndexMap;
use std::path::Path;
use std::sync::Arc;

/// Default batch size for embedding requests.
const EMBED_BATCH_SIZE: usize = 40;
/// Flush interval (ms) for the embedding batcher.
const EMBED_FLUSH_MS: u64 = 200;

/// Paper indexing pipeline: parse documents, extract sections, chunk text,
/// extract figures/tables, generate embeddings (including multimodal via
/// the unified [`EmbeddingClient`]), and persist to stores.
pub struct Indexer {
    pub(super) store: Arc<dyn VectorStore>,
    pub(super) embedding: EmbeddingClient,
    pub(super) batch_embedder: Arc<BatchingEmbedder>,
    pub(super) config: AppConfig,
    pub(super) mineru: Option<MinerUClient>,
    pub(super) rate_limiter: Option<RateLimiter>,
    pub(super) llm_client: Option<crate::llm::client::LlmClient>,
    /// Optional vision client for image semantic descriptions.
    /// When `None`, image description is skipped.
    pub(super) vision_client: Option<crate::llm::client::LlmClient>,
}

impl Indexer {
    pub fn new(
        store: Arc<dyn VectorStore>,
        embedding: EmbeddingClient,
        config: AppConfig,
    ) -> Result<Self> {
        // The indexer owns a store handle, so it wires usage/latency metrics
        // into every LLM client it builds (section/vision/multimodal). The
        // shared `embedding` client gets its sink from the caller (daemon).
        let metrics = crate::llm::metrics::store_metrics_sink(&store);

        let mineru = if config.mineru.enabled {
            match MinerUClient::new(config.mineru.clone()) {
                Ok(client) => {
                    tracing::info!(
                        "MinerU client initialized ({})",
                        config.mineru.mode.as_str()
                    );
                    Some(client)
                }
                Err(e) => {
                    tracing::warn!("Failed to initialize MinerU client: {}", e);
                    None
                }
            }
        } else {
            None
        };

        let section_endpoint = config.resolve_model(&config.purposes.section).ok();
        let rate_limiter = section_endpoint
            .as_ref()
            .and_then(RateLimiter::for_endpoint);

        // Pre-build LLM client for section extraction and figure/table descriptions
        let llm_client =
            section_endpoint.as_ref().and_then(
                |ep| match crate::llm::client::LlmClient::from_config(ep, rate_limiter.clone()) {
                    Ok(client) => Some(client.with_metrics(metrics.clone())),
                    Err(e) => {
                        tracing::warn!("Failed to initialize section LLM client: {}", e);
                        None
                    }
                },
            );

        // Optional vision client for image semantic descriptions.
        // When purposes.vision is not set, image description is skipped.
        let vision_client = config
            .purposes
            .vision
            .as_ref()
            .filter(|key| !key.is_empty())
            .and_then(|key| config.resolve_model(key).ok())
            .and_then(|endpoint| {
                let vision_rate_limiter = RateLimiter::for_endpoint(&endpoint);
                match crate::llm::client::LlmClient::from_config(&endpoint, vision_rate_limiter) {
                    Ok(client) => {
                        tracing::info!("Vision LLM client initialized: {}", endpoint.model);
                        Some(client.with_metrics(metrics.clone()))
                    }
                    Err(e) => {
                        tracing::warn!("Failed to initialize vision LLM client: {}", e);
                        None
                    }
                }
            });

        let batch_embedder = Arc::new(BatchingEmbedder::new(
            embedding.clone(),
            EMBED_BATCH_SIZE,
            EMBED_FLUSH_MS,
        ));

        Ok(Self {
            store,
            embedding,
            batch_embedder,
            config,
            mineru,
            rate_limiter,
            llm_client,
            vision_client,
        })
    }

    /// Add a document using a pre-existing paper ID (for placeholder-based async indexing).
    /// Detects the document source from the file extension.
    pub async fn add_document(&self, path: &Path, paper_id: &str) -> Result<Paper> {
        let source = DocumentSource::from_path(path).unwrap_or(DocumentSource::Pdf);
        self.ingest_document(path, source, Some(paper_id), false)
            .await
    }

    /// Add a document with an optional pre-existing paper ID (for re-indexing).
    /// When `is_reindex` is true, old associated data is cleared before
    /// inserting new data, and the paper record is preserved on failure.
    pub(super) async fn ingest_document(
        &self,
        path: &Path,
        source: DocumentSource,
        paper_id: Option<&str>,
        is_reindex: bool,
    ) -> Result<Paper> {
        let start = std::time::Instant::now();
        tracing::info!("Indexing {:?}: {}", source, path.display());

        // 1. Compute file hash and check for duplicates (skip on reindex)
        let (file_hash, paper_id_str, paper_data_dir) = self
            .step1_hash_and_create_dir(path, is_reindex, paper_id)
            .await?;

        // 2. Extract text (async, with MinerU priority) — images persisted to paper_data_dir
        let step2_start = std::time::Instant::now();
        self.store
            .update_paper_status(
                &paper_id_str,
                PaperStatus::Processing.as_str(),
                Some("extracting PDF text…"),
                None,
            )
            .await
            .ok();
        let extracted = self
            .step2_extract_text(path, source, &paper_data_dir)
            .await?;
        tracing::info!(
            "PDF text extraction took {:.1}s ({} chars)",
            step2_start.elapsed().as_secs_f64(),
            extracted.text.chars().count()
        );

        // Handle image source specially — no text chunking, use multimodal embedding
        if source == DocumentSource::Image {
            return self
                .index_image(path, &paper_id_str, &paper_data_dir, &file_hash)
                .await;
        }

        // 3. Preprocess
        let clean_text = self.step3_preprocess_and_validate(&extracted)?;

        // 4. Load existing paper if available (preserves metadata from external sources
        // like Zotero/Lattice imports). PDF-extracted data only overlays richer values.
        let mut paper = self
            .step4_load_or_create_paper(
                &paper_id_str,
                is_reindex,
                path,
                &file_hash,
                extracted.rich.as_ref(),
            )
            .await;

        // 6. Generate semantic chunks (CPU-bound — offload to the blocking pool)
        let chunk_start = std::time::Instant::now();
        let chunk_paper_id = paper.id.clone();
        let chunk_text = clean_text.clone();
        let is_structured = extracted.is_structured;
        let mut chunk_tree = tokio::task::spawn_blocking(move || {
            Self::step6_generate_chunks(&chunk_paper_id, is_structured, &chunk_text)
        })
        .await
        .map_err(|e| PaperedError::Indexing(format!("Chunking task failed: {e}")))??;
        tracing::info!(
            "Chunking took {:.1}s ({} chunks)",
            chunk_start.elapsed().as_secs_f64(),
            chunk_tree.len()
        );

        // 6b. Repair PDF hyphenation artifacts in heading titles before
        // persist — chunk paths (derived from titles at insert time) and
        // heading-path search both benefit.
        self.step6b_fix_heading_titles(&mut chunk_tree).await;

        // 6c. Repair remaining pdf_oxide body-text artifacts (stray intra-word
        // spaces) in paragraph chunks via batched LLM calls, before persist.
        self.step6c_fix_body_artifacts(&mut chunk_tree, extracted.source)
            .await;

        // 7. On reindex, clear old associated data before inserting new data.
        self.step7_clear_old_data_if_reindexing(is_reindex, &paper_id_str)
            .await;

        // 8. Persist paper metadata and chunks immediately
        self.step8_persist_paper_and_chunks(&paper, &chunk_tree)
            .await?;
        // Chunks are persisted — free the tree before the long LLM/embedding
        // stages below so its memory is not held for the rest of the job.
        drop(chunk_tree);

        // 9. Section extraction and figure extraction are independent LLM
        // tasks — run them in parallel. Figure extraction uses a separate
        // focused prompt; pdf_oxide then locates each caption on a page and
        // renders it. Falls back to MinerU figures when the LLM finds none.
        self.store
            .update_paper_status(
                &paper.id,
                PaperStatus::Processing.as_str(),
                Some("extracting sections via LLM…"),
                None,
            )
            .await
            .ok();
        let section_start = std::time::Instant::now();
        let figure_extraction_enabled = self.config.figure_extraction.enabled;
        let (sections_result, llm_figures) = tokio::join!(
            self.extract_sections(&paper, &clean_text),
            async {
                if !figure_extraction_enabled {
                    return Vec::new();
                }
                match self.extract_figures_with_llm(&clean_text).await {
                    Ok(figs) => {
                        tracing::info!(count = figs.len(), "Figure extraction returned figures");
                        figs
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Figure extraction failed, falling back to MinerU");
                        Vec::new()
                    }
                }
            }
        );
        let (sections, llm_meta) = sections_result?;
        tracing::info!(
            "Section extraction took {:.1}s",
            section_start.elapsed().as_secs_f64()
        );

        if !llm_figures.is_empty() {
            // Primary: LLM figures + pdf_oxide page location + rendering.
            // Any failure here degrades to text-only indexing — the paper's
            // text, sections, and vectors are already persisted by step 8, so
            // a broken figure must not fail the whole paper (mirrors the
            // MinerU fallback branch below).
            if let Err(e) = self
                .index_llm_figures(path, &paper, &llm_figures, &paper_data_dir)
                .await
            {
                tracing::warn!("Failed to index LLM figures for {}: {}", paper.id, e);
            }
        } else {
            // Fallback: MinerU figures.
            let rich_data =
                Self::step9_prepare_figure_rich_data(&paper.id, &extracted, &clean_text);
            if !rich_data.figures.is_empty()
                && let Err(e) = self.index_figures_and_tables(&paper.id, &rich_data).await
            {
                tracing::warn!("Failed to index MinerU figures for {}: {}", paper.id, e);
            }
        }

        // Apply all LLM metadata to paper
        super::helpers::apply_metadata(&mut paper, &llm_meta);
        self.store.update_paper(&paper).await?;
        // Replace stored bio-entities (delete + insert — reindex-safe).
        self.store
            .set_paper_entities(&paper.id, &paper.entities)
            .await?;

        // 10. Store sections
        self.step10_store_sections(&paper.id, &sections).await?;

        // 11. Generate embeddings and store vectors
        self.store
            .update_paper_status(
                &paper.id,
                PaperStatus::Processing.as_str(),
                Some("generating embeddings…"),
                None,
            )
            .await
            .ok();
        self.step11_index_section_vectors(&paper.id, &sections)
            .await?;

        tracing::info!(
            "Indexed paper: {} ({} sections) in {:.1}s",
            paper.title,
            sections.sections.len(),
            start.elapsed().as_secs_f64()
        );

        // Abstract is generated as a Section — ensure the paper field is clear.
        paper.abstract_text = None;
        if let Err(e) = self.store.update_paper(&paper).await {
            tracing::warn!(
                "Failed to clear abstract_text for paper {}: {}",
                paper.id,
                e
            );
        }

        if source == DocumentSource::Pdf {
            let data_dir = self.config.data_dir.clone();
            let paper_id = paper.id.clone();
            let path_owned = path.to_path_buf();
            // Cover rendering (150 DPI raster + Lanczos3 resize) is CPU-bound —
            // offload to the blocking pool.
            let cover_result = tokio::task::spawn_blocking(move || {
                crate::cover::generate_cover(&path_owned, &paper_id, &data_dir)
            })
            .await;
            match cover_result {
                Ok(Ok(Some(cover_rel))) => {
                    paper.cover_path = Some(cover_rel);
                    if let Err(e) = self.store.update_paper(&paper).await {
                        tracing::warn!(
                            paper_id = %paper.id,
                            "Failed to persist cover_path: {e}"
                        );
                    }
                }
                Ok(Ok(None)) => {
                    paper.cover_path = None;
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        paper_id = %paper.id,
                        "Cover generation failed: {e}"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        paper_id = %paper.id,
                        "Cover generation task failed: {e}"
                    );
                }
            }
        }

        Ok(paper)
    }

    // ── private step methods for ingest_document ──────────────────────────

    /// Step 1: Compute file hash, check for duplicates, generate paper ID and create data dir.
    async fn step1_hash_and_create_dir(
        &self,
        path: &Path,
        is_reindex: bool,
        paper_id: Option<&str>,
    ) -> Result<(String, String, std::path::PathBuf)> {
        // Hashing streams the whole file — offload to the blocking pool so the
        // async runtime stays responsive for HTTP requests.
        let path_buf = path.to_path_buf();
        let file_hash = tokio::task::spawn_blocking(move || compute_file_hash(&path_buf))
            .await
            .map_err(|e| PaperedError::Indexing(format!("File hash task failed: {e}")))??;
        if !is_reindex && let Some(existing) = self.store.get_paper_by_file_hash(&file_hash).await?
        {
            return Err(PaperedError::Duplicate {
                title: existing.title,
                id: existing.id,
            });
        }

        let generated_id = uuid::Uuid::new_v4().to_string();
        let paper_id_str = paper_id.map(|s| s.to_string()).unwrap_or(generated_id);
        let paper_data_dir = self.config.data_dir.join("papers").join(&paper_id_str);
        std::fs::create_dir_all(&paper_data_dir).map_err(|e| {
            PaperedError::Indexing(format!(
                "Failed to create paper data directory {}: {}",
                paper_data_dir.display(),
                e
            ))
        })?;
        Ok((file_hash, paper_id_str, paper_data_dir))
    }

    /// Step 2: Extract text from the document (async, with MinerU priority).
    async fn step2_extract_text(
        &self,
        path: &Path,
        source: DocumentSource,
        paper_data_dir: &Path,
    ) -> Result<ExtractedText> {
        let extraction_start = std::time::Instant::now();
        let extracted = extract_document_text(
            path,
            source,
            self.mineru.as_ref(),
            Some(paper_data_dir),
            &self.config.pdf_extraction,
        )
        .await?;
        tracing::info!(
            "Extraction took {:.1}s",
            extraction_start.elapsed().as_secs_f64()
        );
        Ok(extracted)
    }

    /// Step 3: Preprocess extracted text and validate quality.
    fn step3_preprocess_and_validate(&self, extracted: &ExtractedText) -> Result<String> {
        let preprocess_start = std::time::Instant::now();
        let (clean_text, quality) = preprocess_text(extracted, &self.config.pdf_extraction);
        tracing::info!(
            "Preprocessing took {:.1}s, quality_score={}, action={:?}, issues={:?}",
            preprocess_start.elapsed().as_secs_f64(),
            quality.score,
            quality.action,
            quality.issues
        );
        if quality.action == IndexAction::Reject {
            return Err(PaperedError::Indexing(format!(
                "Text quality too low (score: {}), issues: {:?}",
                quality.score, quality.issues
            )));
        }
        Ok(clean_text)
    }

    /// Step 4: Load existing paper or create a new one, applying MinerU rich metadata.
    async fn step4_load_or_create_paper(
        &self,
        paper_id_str: &str,
        is_reindex: bool,
        path: &Path,
        file_hash: &str,
        rich: Option<&RichExtraction>,
    ) -> Paper {
        let mut paper = if !is_reindex {
            self.store
                .get_paper(paper_id_str)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| {
                    let mut p = Paper::new(
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("Untitled")
                            .to_string(),
                    );
                    p.id = paper_id_str.to_string();
                    p
                })
        } else {
            let mut p = Paper::new(
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Untitled")
                    .to_string(),
            );
            p.id = paper_id_str.to_string();
            p
        };
        if paper.title == "Processing…" {
            paper.title = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Untitled")
                .to_string();
        }
        paper.file_path = Some(path.to_string_lossy().into_owned());
        paper.file_hash = Some(file_hash.to_string());
        paper.status = PaperStatus::Processing;

        // Apply MinerU rich metadata as initial values
        if let Some(rich) = rich {
            let mut rich_meta = PaperMetadata {
                title: rich.title.clone(),
                authors: rich.authors.clone(),
                affiliations: rich.affiliations.clone(),
                emails: rich.emails.clone(),
                keywords: rich.keywords.clone(),
                urls: rich.urls.clone(),
                extra: rich.extra.clone(),
                abstract_text: rich.abstract_text.clone(),
                doi: rich.doi.clone(),
                ..Default::default()
            };
            sanitize_paper_metadata(&mut rich_meta);
            helpers::apply_metadata(&mut paper, &rich_meta);
        }

        paper
    }

    /// Step 6: Generate semantic chunks from structured or plain text.
    fn step6_generate_chunks(
        paper_id: &str,
        is_structured: bool,
        clean_text: &str,
    ) -> Result<crate::chunker::ChunkTree> {
        if is_structured {
            Ok(crate::chunker::chunk_markdown(paper_id, clean_text))
        } else {
            crate::chunker::chunk_fixed_size(paper_id, clean_text, 1500, 300)
                .map_err(|e| PaperedError::Indexing(format!("Chunking failed: {e}")))
        }
    }

    /// Step 7: On reindex, clear old vectors, sections, chunks, and figures.
    async fn step7_clear_old_data_if_reindexing(&self, is_reindex: bool, paper_id_str: &str) {
        if is_reindex {
            let (v, s, c, f) = tokio::join!(
                self.store.delete_by_paper(paper_id_str),
                self.store.delete_sections(paper_id_str),
                self.store.delete_chunks(paper_id_str),
                self.store.delete_figures(paper_id_str),
            );
            for (name, result) in [
                ("vectors", v),
                ("sections", s),
                ("chunks", c),
                ("figures", f),
            ] {
                if let Err(e) = result {
                    tracing::warn!("Failed to delete old {name} for reindex {paper_id_str}: {e}");
                }
            }
        }
    }

    /// Step 8: Persist paper metadata and chunks to the store.
    async fn step8_persist_paper_and_chunks(
        &self,
        paper: &Paper,
        chunk_tree: &crate::chunker::ChunkTree,
    ) -> Result<()> {
        let db_insert_start = std::time::Instant::now();
        self.store.insert_paper(paper).await?;
        self.store.insert_chunks(&paper.id, chunk_tree).await?;
        tracing::info!(
            "DB insert (paper+chunks) took {:.1}s",
            db_insert_start.elapsed().as_secs_f64()
        );
        Ok(())
    }

    /// Step 9: Prepare figure rich data from MinerU extraction or markdown fallback.
    fn step9_prepare_figure_rich_data(
        paper_id: &str,
        extracted: &ExtractedText,
        clean_text: &str,
    ) -> RichExtraction {
        if let Some(ref rich) = extracted.rich {
            tracing::info!(
                "Indexing figures for {} ({} figures)",
                paper_id,
                rich.figures.len(),
            );
            rich.clone()
        } else {
            let markdown_figures =
                crate::index::multimodal::parse_figures_from_markdown(paper_id, clean_text);
            RichExtraction {
                figures: markdown_figures
                    .into_iter()
                    .map(|f| crate::paper::mineru::MinerUFigure {
                        caption: f.caption,
                        image_path: f.image_path,
                        page_number: f.page_number,
                    })
                    .collect(),
                ..Default::default()
            }
        }
    }

    /// Step 10: Store extracted sections.
    async fn step10_store_sections(&self, paper_id: &str, sections: &PaperSections) -> Result<()> {
        self.store.insert_sections(paper_id, sections).await
    }

    /// Step 11: Generate embeddings for sections and store vectors.
    async fn step11_index_section_vectors(
        &self,
        paper_id: &str,
        sections: &PaperSections,
    ) -> Result<()> {
        tracing::info!(
            "About to index vectors for {} with {} sections",
            paper_id,
            sections.sections.len()
        );
        let vector_start = std::time::Instant::now();
        self.index_vectors(paper_id, sections).await?;
        tracing::info!(
            "Vector indexing took {:.1}s",
            vector_start.elapsed().as_secs_f64()
        );
        Ok(())
    }

    /// Delete a paper and all associated data.
    pub async fn delete_paper(&self, paper_id: &str) -> Result<()> {
        self.store.delete_paper(paper_id).await?;

        // Clean up internal extracted data directory
        let paper_data_dir = self.config.data_dir.join("papers").join(paper_id);
        if paper_data_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&paper_data_dir) {
                tracing::warn!(
                    "Failed to remove paper data dir {}: {}",
                    paper_data_dir.display(),
                    e
                );
            } else {
                tracing::info!("Removed paper data dir: {}", paper_data_dir.display());
            }
        }

        Ok(())
    }

    pub(super) async fn extract_sections(
        &self,
        paper: &Paper,
        clean_text: &str,
    ) -> Result<(PaperSections, PaperMetadata)> {
        self.extract_sections_with_llm(paper, clean_text).await
    }

    /// Step 6b: Repair PDF hyphenation artifacts ("frame work" → "framework")
    /// in chapter/section titles via one LLM call per paper. Runs before chunk
    /// persist so stored titles and derived heading paths are consistent.
    /// No-op without an LLM client.
    async fn step6b_fix_heading_titles(&self, chunk_tree: &mut [crate::chunker::Chunk]) {
        let Some(ref client) = self.llm_client else {
            return;
        };
        let mut idxs: Vec<usize> = Vec::new();
        let mut titles: Vec<String> = Vec::new();
        for (i, chunk) in chunk_tree.iter().enumerate() {
            if matches!(
                chunk.chunk_type,
                crate::chunker::ChunkType::Chapter | crate::chunker::ChunkType::Section
            ) {
                idxs.push(i);
                titles.push(chunk.content.clone());
            }
        }
        crate::llm::headings::fix_hyphenated_headings(client, &mut titles).await;
        for (pos, i) in idxs.into_iter().enumerate() {
            if chunk_tree[i].content != titles[pos] {
                tracing::debug!(
                    "Heading repaired: {:?} -> {:?}",
                    chunk_tree[i].content,
                    titles[pos]
                );
                chunk_tree[i].content = std::mem::take(&mut titles[pos]);
            }
        }
    }

    /// Step 6c: Repair remaining pdf_oxide body-text artifacts (stray
    /// intra-word spaces like "framew ork") in paragraph chunks via batched LLM
    /// calls, before persist so stored chunks, search and RAG all benefit.
    /// Line-break hyphenation is already fixed deterministically in step 3.
    /// Skipped for non-pdf_oxide sources (MinerU output is clean), when the
    /// feature is disabled, or without an LLM client.
    async fn step6c_fix_body_artifacts(
        &self,
        chunk_tree: &mut [crate::chunker::Chunk],
        source: ExtractorSource,
    ) {
        if source != ExtractorSource::PdfOxide || !self.config.pdf_extraction.fix_text_artifacts {
            return;
        }
        let Some(ref client) = self.llm_client else {
            return;
        };
        let idxs: Vec<usize> = chunk_tree
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                matches!(c.chunk_type, crate::chunker::ChunkType::Paragraph)
                    && crate::llm::artifacts::chunk_likely_has_artifacts(&c.content)
            })
            .map(|(i, _)| i)
            .collect();
        if idxs.is_empty() {
            return;
        }
        tracing::debug!("Body artifact repair: {} candidate chunk(s)", idxs.len());
        crate::llm::artifacts::fix_body_artifacts(client, chunk_tree, &idxs).await;
    }

    /// Separate, focused LLM call for figure extraction. Much shorter prompt
    /// than the main extraction — only asks for figures, so the model can
    /// focus on that single task.
    async fn extract_figures_with_llm(
        &self,
        clean_text: &str,
    ) -> Result<Vec<crate::paper::parser::LlmFigure>> {
        const FIGURE_EXTRACTION_PROMPT: &str = include_str!("figure_extraction_prompt.txt");

        let client = self.llm_client.as_ref().ok_or_else(|| {
            PaperedError::SectionExtraction(
                "LLM client unavailable — endpoint may be misconfigured".into(),
                None,
            )
        })?;

        let truncated: String = clean_text
            .chars()
            .take(self.config.section.max_input_chars)
            .collect();
        let prompt = FIGURE_EXTRACTION_PROMPT.replace("{truncated_text}", &truncated);
        let system =
            "You extract figure metadata from academic papers. Respond ONLY with valid JSON.";

        let response = client
            .generate_json(system, &prompt, self.config.section.max_output_tokens, 0.1)
            .await
            .map_err(|e| {
                PaperedError::SectionExtraction(
                    format!("Figure extraction LLM call failed: {e}"),
                    Some(Box::new(e)),
                )
            })?;

        let parsed: serde_json::Value = helpers::try_parse_llm_json(&response).map_err(|e| {
            PaperedError::SectionExtraction(
                format!("Figure extraction response was not valid JSON: {e}"),
                None,
            )
        })?;

        Ok(helpers::parse_llm_figures(&parsed))
    }

    async fn extract_sections_with_llm(
        &self,
        paper: &Paper,
        clean_text: &str,
    ) -> Result<(PaperSections, PaperMetadata)> {
        // Input budget per window: max_input_chars is a soft user preference.
        // If the model declares its context_window (tokens), derive a hard upper
        // bound by reserving space for the output budget. Otherwise use max_input_chars as-is.
        let section_model = self
            .config
            .resolve_model(&self.config.purposes.section)
            .ok();
        let max_input_chars =
            if let Some(ctx_window) = section_model.as_ref().and_then(|m| m.context_window) {
                let estimated_output_chars = (self.config.section.max_output_tokens * 4) / 5;
                let estimated_input_chars = ctx_window.saturating_mul(4); // rough token→char
                let hard_limit = estimated_input_chars.saturating_sub(estimated_output_chars);
                self.config.section.max_input_chars.min(hard_limit)
            } else {
                self.config.section.max_input_chars
            };

        let client = self.llm_client.as_ref().ok_or_else(|| {
            PaperedError::SectionExtraction(
                "LLM client unavailable — endpoint may be misconfigured".into(),
                None,
            )
        })?;
        let system = "You are an expert academic research assistant. Respond ONLY with valid JSON.";

        // Multi-pass extraction: split long papers into context-sized windows so
        // the tail is no longer silently truncated. A single window keeps the
        // previous single-call behavior.
        let windows = split_text_windows(clean_text, max_input_chars);
        if windows.len() > 1 {
            tracing::info!(
                "Paper {} exceeds context budget; extracting across {} windows",
                paper.id,
                windows.len()
            );
        }

        let mut section_lists: Vec<Vec<Section>> = Vec::with_capacity(windows.len());
        let mut merged_meta: Option<PaperMetadata> = None;
        let mut window_entities: Vec<crate::paper::BioEntities> = Vec::with_capacity(windows.len());

        for window in &windows {
            let prompt = helpers::build_extraction_prompt(paper, window);
            let response = client
                .generate_json(system, &prompt, self.config.section.max_output_tokens, 0.1)
                .await
                .map_err(|e| {
                    PaperedError::SectionExtraction(
                        format!("Section LLM call failed: {e}"),
                        Some(Box::new(e)),
                    )
                })?;

            let parsed: serde_json::Value =
                helpers::try_parse_llm_json(&response).map_err(|e| {
                    let preview: String =
                        response.chars().take(helpers::PREVIEW_MAX_CHARS).collect();
                    tracing::error!(
                        "Failed to parse LLM JSON response for paper {}: {}; preview: {:?}",
                        paper.id,
                        e,
                        preview
                    );
                    PaperedError::SectionExtraction("LLM response was not valid JSON".into(), None)
                })?;

            let meta = helpers::parse_llm_metadata(&parsed, paper);
            let secs = helpers::build_sections_from_json(&parsed, &meta);
            window_entities.push(meta.entities.clone());
            // Front matter (title/authors/abstract) lives in the head window.
            if merged_meta.is_none() {
                merged_meta = Some(meta);
            }

            section_lists.push(secs.sections);
        }

        let mut meta = merged_meta.expect("split_text_windows always yields >= 1 window");
        // Bio-entities can appear anywhere in the paper: merge every window's
        // extraction, deduplicating case-insensitively.
        meta.entities = crate::paper::BioEntities::merge(&window_entities);
        let merged_sections = if section_lists.len() <= 1 {
            Self::concat_sections_by_type(&section_lists)
        } else {
            self.llm_merge_sections(client, paper, &section_lists)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        "LLM section merge failed for paper {}: {e}; falling back to concatenation",
                        paper.id
                    );
                    Self::concat_sections_by_type(&section_lists)
                })
        };
        let mut sections = PaperSections {
            sections: merged_sections,
            input_hash: None,
        };
        let meta_section = helpers::build_metadata_section(&meta, paper);
        sections.sections.insert(0, meta_section);

        Ok((sections, meta))
    }

    /// LLM-based merge: takes per-window section fragments and produces a
    /// single coherent section per type, eliminating repetition and unifying
    /// the narrative across windows.
    async fn llm_merge_sections(
        &self,
        client: &crate::llm::client::LlmClient,
        paper: &Paper,
        section_lists: &[Vec<Section>],
    ) -> Result<Vec<Section>> {
        let system = "You are an expert academic editor. Respond ONLY with valid JSON.";
        let prompt = helpers::build_merge_prompt(paper, section_lists);
        let response = client
            .generate_json(system, &prompt, self.config.section.max_output_tokens, 0.1)
            .await
            .map_err(|e| {
                PaperedError::SectionExtraction(
                    format!("Section merge LLM call failed: {e}"),
                    Some(Box::new(e)),
                )
            })?;

        let parsed: serde_json::Value = helpers::try_parse_llm_json(&response).map_err(|e| {
            let preview: String = response.chars().take(helpers::PREVIEW_MAX_CHARS).collect();
            tracing::error!("Failed to parse merge JSON: {e}; preview: {preview:?}");
            PaperedError::SectionExtraction("Merge LLM response was not valid JSON".into(), None)
        })?;

        let mut merged = Vec::new();
        if let Some(obj) = parsed.as_object() {
            use crate::paper::section::SectionType;
            for (key, val) in obj {
                match serde_json::from_value::<SectionType>(serde_json::Value::String(key.clone()))
                {
                    Ok(section_type) => {
                        let content = val.as_str().unwrap_or("").trim().to_string();
                        if !content.is_empty() {
                            let content_hash = crate::util::sha256_hex(content.as_bytes());
                            merged.push(Section {
                                section_type,
                                content,
                                content_hash,
                            });
                        }
                    }
                    Err(_) => {
                        tracing::warn!("Unknown section type in merge response: {key}");
                    }
                }
            }
        }
        Ok(merged)
    }

    /// Mechanical concatenation fallback: concatenate same-type sections from
    /// different windows, separated by double newlines. Used for single-window
    /// papers and as fallback when LLM merge fails.
    fn concat_sections_by_type(lists: &[Vec<Section>]) -> Vec<Section> {
        use crate::paper::section::SectionType;
        let mut merged: IndexMap<SectionType, String> = IndexMap::new();
        for list in lists {
            for s in list {
                let trimmed = s.content.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let entry = merged.entry(s.section_type).or_default();
                if !entry.is_empty() {
                    entry.push_str("\n\n");
                }
                entry.push_str(trimmed);
            }
        }
        merged
            .into_iter()
            .map(|(section_type, content)| {
                let content_hash = crate::util::sha256_hex(content.as_bytes());
                Section {
                    section_type,
                    content,
                    content_hash,
                }
            })
            .collect()
    }

    /// Embed all sections for a paper and upsert the resulting vectors.
    pub(super) async fn index_vectors(
        &self,
        paper_id: &str,
        sections: &PaperSections,
    ) -> Result<()> {
        let n = sections.sections.len();
        if n == 0 {
            tracing::warn!("No sections to index for {}", paper_id);
            return Ok(());
        }

        let texts: Vec<&str> = sections
            .sections
            .iter()
            .map(|s| s.content.as_str())
            .collect();

        tracing::info!(
            "Indexing vectors for {}: {} sections",
            paper_id,
            texts.len()
        );

        let embeddings = self.batch_embedder.embed_batch(&texts).await?;
        tracing::info!("Generated {} embeddings for {}", embeddings.len(), paper_id);

        let mut records = Vec::with_capacity(n);
        for (section, embedding) in sections.sections.iter().zip(embeddings.iter()) {
            records.push(VectorRecord::section(
                paper_id.to_string(),
                section.section_type.to_string(),
                embedding.embedding.clone(),
                section.content.clone(),
            ));
        }

        self.store.upsert(&records).await?;

        // Store embedding model fingerprint on the paper metadata.
        if let Some(fingerprint) = self.config.embedding_fingerprint()
            && let Err(e) = self
                .store
                .set_paper_embedding_model(paper_id, &fingerprint)
                .await
        {
            tracing::warn!("Failed to set embedding model for {}: {}", paper_id, e);
        }

        Ok(())
    }
}
