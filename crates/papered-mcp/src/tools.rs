//! MCP tool handlers for Papered, backed by rmcp's `#[tool]` macros.
//!
//! Each tool has a dedicated params struct that derives `JsonSchema` so rmcp
//! auto-generates the `inputSchema` at compile time — no more hand-syncing
//! JSON schemas with handler field names.

use papered::StrLabel;
use papered::VectorStore;
use papered::llm::rag::RagSourceView;
use papered::paper::format::resolve_abstract;
use papered::paper::section::{PaperSections, SectionType};
use papered::search::{DEFAULT_MIN_SCORE, MAX_RESULT_LIMIT, SearchMethod};
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;

use crate::PaperedMcpServer;
use crate::util::McpResultExt;

// ---------------------------------------------------------------------------
// Parameter types — derive JsonSchema so rmcp generates inputSchema for free
// ---------------------------------------------------------------------------

fn default_limit_10() -> usize {
    10
}
fn default_limit_20() -> usize {
    20
}
fn default_limit_5() -> usize {
    5
}
fn default_false() -> bool {
    false
}
fn default_true() -> bool {
    true
}
fn default_search_method() -> SearchMethod {
    SearchMethod::default()
}
fn default_offset() -> usize {
    0
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchPapersParams {
    pub query: String,
    #[serde(default = "default_search_method")]
    pub search_method: SearchMethod,
    #[serde(default)]
    pub section_filter: Option<SectionType>,
    #[serde(default)]
    pub paper_type: Option<String>,
    #[serde(default)]
    pub species: Option<String>,
    #[serde(default)]
    pub gene: Option<String>,
    #[serde(default)]
    pub technique: Option<String>,
    #[serde(default)]
    pub pathway: Option<String>,
    #[serde(default = "default_limit_10")]
    pub limit: usize,
    #[serde(default = "default_false")]
    pub include_abstract: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchPassagesParams {
    pub query: String,
    #[serde(default)]
    pub paper_id: Option<String>,
    #[serde(default = "default_limit_10")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchFiguresParams {
    pub query: String,
    #[serde(default = "default_limit_10")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetPaperPassagesParams {
    pub paper_id: String,
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_limit_20")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetPassageParams {
    pub paper_id: String,
    pub chunk_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskRagParams {
    pub question: String,
    #[serde(default = "default_search_method")]
    pub search_method: SearchMethod,
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default)]
    pub paper_id: Option<String>,
    #[serde(default)]
    pub use_enhancement: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListPapersParams {
    #[serde(default = "default_limit_20")]
    pub limit: usize,
    #[serde(default = "default_offset")]
    pub offset: usize,
    #[serde(default = "default_false")]
    pub include_abstract: bool,
    #[serde(default)]
    pub paper_type: Option<String>,
    #[serde(default)]
    pub species: Option<String>,
    #[serde(default)]
    pub gene: Option<String>,
    #[serde(default)]
    pub technique: Option<String>,
    #[serde(default)]
    pub pathway: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetPaperDetailsParams {
    pub paper_id: String,
    #[serde(default = "default_true")]
    pub include_sections: bool,
    #[serde(default = "default_true")]
    pub include_abstract: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindSimilarPapersParams {
    pub paper_id: String,
    #[serde(default = "default_limit_5")]
    pub limit: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetIndexStatusParams {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn tool_result_text(text: impl Into<String>) -> CallToolResult {
    let mut result = CallToolResult::default();
    result.content = vec![ContentBlock::text(text.into())];
    result.is_error = Some(false);
    result
}

fn json_text(v: &impl serde::Serialize) -> String {
    crate::util::json_text(v, "response")
}

fn chunks_to_json<T: serde::Serialize>(hits: &[T]) -> Vec<serde_json::Value> {
    hits.iter()
        .map(|h| serde_json::to_value(h).unwrap_or_default())
        .collect()
}

fn scored_papers_to_json(results: &[papered::paper::PaperSearchResult]) -> Vec<serde_json::Value> {
    results
        .iter()
        .map(|r| {
            let mut obj = serde_json::to_value(&r.paper).unwrap_or_default();
            obj["score"] = serde_json::json!(r.score);
            obj
        })
        .collect()
}

fn validate_paper_id(paper_id: &str) -> Result<(), ErrorData> {
    papered::util::paths::validate_paper_id_msg(paper_id)
        .map_err(|e| ErrorData::invalid_params(format!("Invalid paper ID: {e}"), None))
}

fn attach_abstracts(
    papers_json: &mut [serde_json::Value],
    papers: &[papered::paper::Paper],
    sections_list: &[PaperSections],
) {
    for (i, paper_json) in papers_json.iter_mut().enumerate() {
        if let Some(sections) = sections_list.get(i)
            && let Some(abs) = resolve_abstract(&papers[i], sections)
        {
            paper_json["abstract"] = serde_json::json!(abs);
        }
    }
}

async fn maybe_attach_abstracts(
    store: &Arc<dyn VectorStore>,
    papers: &[papered::paper::Paper],
    papers_json: &mut [serde_json::Value],
    include_abstract: bool,
) -> Result<(), String> {
    if include_abstract && !papers.is_empty() {
        let ids: Vec<&str> = papers.iter().map(|p| p.id.as_str()).collect();
        let sections_list = store
            .get_sections_batch(&ids)
            .await
            .map_err(|e| e.to_string())?;
        attach_abstracts(papers_json, papers, &sections_list);
    }
    Ok(())
}

fn entity_filter_from_params(params: &impl EntityFilterParams) -> papered::paper::EntityFilter {
    papered::paper::EntityFilter {
        species: params.species().map(str::to_string),
        gene: params.gene().map(str::to_string),
        technique: params.technique().map(str::to_string),
        pathway: params.pathway().map(str::to_string),
    }
}

trait EntityFilterParams {
    fn species(&self) -> Option<&str>;
    fn gene(&self) -> Option<&str>;
    fn technique(&self) -> Option<&str>;
    fn pathway(&self) -> Option<&str>;
}

macro_rules! impl_entity_filter {
    ($($t:ty),+) => {
        $(impl EntityFilterParams for $t {
            fn species(&self) -> Option<&str> { self.species.as_deref().filter(|s| !s.trim().is_empty()) }
            fn gene(&self) -> Option<&str> { self.gene.as_deref().filter(|s| !s.trim().is_empty()) }
            fn technique(&self) -> Option<&str> { self.technique.as_deref().filter(|s| !s.trim().is_empty()) }
            fn pathway(&self) -> Option<&str> { self.pathway.as_deref().filter(|s| !s.trim().is_empty()) }
        })+
    };
}

impl_entity_filter!(SearchPapersParams, ListPapersParams);

async fn apply_structured_filters(
    store: &Arc<dyn VectorStore>,
    results: Vec<papered::paper::PaperSearchResult>,
    entity_filter: &papered::paper::EntityFilter,
    paper_type: Option<&str>,
) -> Result<Vec<papered::paper::PaperSearchResult>, String> {
    if entity_filter.is_empty() && paper_type.is_none() {
        return Ok(results);
    }
    let mut candidates: Option<HashSet<String>> = None;
    for (kind, value) in entity_filter.pairs() {
        let ids = store
            .paper_ids_by_entity(kind, value)
            .await
            .map_err(|e| e.to_string())?;
        intersect_ids(&mut candidates, ids);
    }
    if let Some(pt) = paper_type {
        let ids = store
            .paper_ids_by_paper_type(pt)
            .await
            .map_err(|e| e.to_string())?;
        intersect_ids(&mut candidates, ids);
    }
    let keep = candidates.unwrap_or_default();
    Ok(results
        .into_iter()
        .filter(|r| keep.contains(&r.paper.id))
        .collect())
}

fn intersect_ids(candidates: &mut Option<HashSet<String>>, ids: Vec<String>) {
    let set: HashSet<String> = ids.into_iter().collect();
    *candidates = Some(match candidates.take() {
        None => set,
        Some(prev) => prev.intersection(&set).cloned().collect(),
    });
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

// rmcp's `#[tool]` macro transforms each method into a route-able handler.
// The `#[tool_router]` on the outer impl block collects them all.
#[rmcp::tool_router(vis = "pub(crate)")]
impl PaperedMcpServer {
    #[rmcp::tool(
        description = "Search for papers in the library — the starting point for answering research questions. Returns paper metadata (title, authors, venue, DOI, date, type, keywords) with relevance scores; set include_abstract=true to also fetch full abstracts. Structured bio-entity filters (species, gene, technique, pathway) and paper_type are applied after the search."
    )]
    async fn search_papers(
        &self,
        Parameters(params): Parameters<SearchPapersParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.min(MAX_RESULT_LIMIT);
        let engine = self.search_engine.read().await.clone();
        let results = engine
            .search_papers_by_method(
                &params.query,
                params.section_filter,
                params.search_method,
                limit,
                DEFAULT_MIN_SCORE,
            )
            .await
            .mcp()?;

        let entity_filter = entity_filter_from_params(&params);
        let results = apply_structured_filters(
            &self.store,
            results,
            &entity_filter,
            params.paper_type.as_deref(),
        )
        .await
        .mcp()?;

        let papers: Vec<papered::paper::Paper> = results.iter().map(|r| r.paper.clone()).collect();
        let mut papers_json = scored_papers_to_json(&results);
        maybe_attach_abstracts(
            &self.store,
            &papers,
            &mut papers_json,
            params.include_abstract,
        )
        .await
        .mcp()?;

        Ok(tool_result_text(json_text(&papers_json)))
    }

    #[rmcp::tool(
        description = "Search for text passages across all papers or within a specific paper, returning ranked passages whose original text matches the query."
    )]
    async fn search_passages(
        &self,
        Parameters(params): Parameters<SearchPassagesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.min(MAX_RESULT_LIMIT);

        let paper_ids: Vec<String> = match params.paper_id {
            Some(ref pid) => {
                validate_paper_id(pid)?;
                vec![pid.clone()]
            }
            None => {
                let engine = self.search_engine.read().await.clone();
                let papers = engine
                    .search(&params.query, None, limit.max(20), DEFAULT_MIN_SCORE)
                    .await
                    .mcp()?;
                papers.into_iter().map(|r| r.paper.id).collect()
            }
        };

        if paper_ids.is_empty() {
            return Ok(tool_result_text("No relevant papers found."));
        }

        let ids_ref: Vec<&str> = paper_ids.iter().map(String::as_str).collect();
        let hits = self
            .store
            .search_chunks(&ids_ref, &params.query, limit)
            .await
            .mcp()?;

        Ok(tool_result_text(json_text(&chunks_to_json(&hits))))
    }

    #[rmcp::tool(
        description = "Search for figures (images, charts, diagrams) across all papers by caption and description."
    )]
    async fn search_figures(
        &self,
        Parameters(params): Parameters<SearchFiguresParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.min(MAX_RESULT_LIMIT);
        let engine = self.search_engine.read().await.clone();
        let results = engine
            .search_figures(&params.query, limit, DEFAULT_MIN_SCORE)
            .await
            .mcp()?;

        let figs_json: Vec<serde_json::Value> = results
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "paper_id": r.paper.id,
                    "paper_title": r.paper.title,
                    "figure_id": r.figure.id,
                    "caption": r.figure.caption,
                    "description": r.figure.description,
                    "score": r.score,
                })
            })
            .collect();
        Ok(tool_result_text(json_text(&figs_json)))
    }

    #[rmcp::tool(description = "Retrieve text passages from a specific paper. \
                       When a `query` is provided, full-text search returns matching passages ranked by relevance. \
                       Without a query, returns passages in document order (useful for reading the full paper).")]
    async fn get_paper_passages(
        &self,
        Parameters(params): Parameters<GetPaperPassagesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.min(MAX_RESULT_LIMIT);
        validate_paper_id(&params.paper_id)?;

        if params.query.trim().is_empty() {
            // No query → list all passages in document order.
            let chunks = self.store.get_chunks(&params.paper_id).await.mcp()?;
            let hits: Vec<papered::store::vector::ChunkHit> = chunks
                .into_iter()
                .take(limit)
                .map(|chunk| papered::store::vector::ChunkHit { chunk, score: 1.0 })
                .collect();
            return Ok(tool_result_text(json_text(&chunks_to_json(&hits))));
        }

        let hits = self
            .store
            .search_chunks(&[&params.paper_id], &params.query, limit)
            .await
            .mcp()?;

        Ok(tool_result_text(json_text(&chunks_to_json(&hits))))
    }

    #[rmcp::tool(
        description = "Fetch a single passage (chunk) by its ID — the IDs returned in ask_rag source citations."
    )]
    async fn get_passage(
        &self,
        Parameters(params): Parameters<GetPassageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        validate_paper_id(&params.paper_id)?;

        let (chunk, heading_path) = papered::retrieval::chunk_with_heading_path(
            self.store.as_ref(),
            &params.paper_id,
            &params.chunk_id,
        )
        .await
        .mcp()?
        .ok_or_else(|| {
            ErrorData::invalid_params(format!("Chunk not found: {}", params.chunk_id), None)
        })?;

        Ok(tool_result_text(json_text(&serde_json::json!({
            "chunk_id": chunk.id,
            "paper_id": chunk.paper_id,
            "parent_id": chunk.parent_id,
            "chunk_type": chunk.chunk_type.as_str(),
            "content": chunk.content,
            "page_number": chunk.page_number,
            "heading_path": heading_path,
        }))))
    }

    #[rmcp::tool(
        description = "Ask a question and get an answer grounded in the paper library with citations. Optionally scope to a specific paper via paper_id."
    )]
    async fn ask_rag(
        &self,
        Parameters(params): Parameters<AskRagParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(ref pid) = params.paper_id {
            validate_paper_id(pid)?;
        }

        let result = self
            .rag_engine
            .read()
            .await
            .ask(
                &params.question,
                Some(params.search_method),
                params.prompt_id.as_deref(),
                params.use_enhancement,
                params.paper_id.as_deref(),
            )
            .await
            .mcp()?;

        let source_count = result.sources.len();
        let sources: Vec<serde_json::Value> = result
            .sources
            .into_iter()
            .map(|s| serde_json::to_value(RagSourceView::from(s)).unwrap_or_default())
            .collect();
        let text = format!(
            "## Answer\n{}\n\n## Sources ({source_count} via {} search)\n{}",
            result.answer,
            result.search_method_used,
            json_text(&sources)
        );
        Ok(tool_result_text(text))
    }

    #[rmcp::tool(
        description = "List papers with pagination and structured filters. Returns id, title, authors, venue, DOI, date, type, and keywords."
    )]
    async fn list_papers(
        &self,
        Parameters(params): Parameters<ListPapersParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.min(MAX_RESULT_LIMIT);
        let entity_filter = entity_filter_from_params(&params);

        let (papers, _total) = self
            .store
            .list_papers_filtered(
                None,
                params.paper_type.as_deref(),
                None,
                &entity_filter,
                None,
                true,
                limit,
                params.offset,
            )
            .await
            .mcp()?;

        let mut papers_json: Vec<serde_json::Value> = papers
            .iter()
            .map(|p| serde_json::to_value(p).unwrap_or_default())
            .collect();
        maybe_attach_abstracts(
            &self.store,
            &papers,
            &mut papers_json,
            params.include_abstract,
        )
        .await
        .mcp()?;

        Ok(tool_result_text(json_text(&papers_json)))
    }

    #[rmcp::tool(
        description = "Get paper metadata, extracted bio-entities, and optionally extracted sections."
    )]
    async fn get_paper_details(
        &self,
        Parameters(params): Parameters<GetPaperDetailsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        validate_paper_id(&params.paper_id)?;

        let paper = self
            .store
            .get_paper(&params.paper_id)
            .await
            .mcp()?
            .ok_or_else(|| {
                ErrorData::invalid_params(format!("Paper not found: {}", params.paper_id), None)
            })?;

        let sections = self.store.get_sections(&params.paper_id).await.mcp()?;
        let entities = self.store.paper_entities(&params.paper_id).await.mcp()?;

        let mut paper_json = serde_json::to_value(&paper).unwrap_or_default();
        paper_json["entities"] = serde_json::to_value(&entities).unwrap_or_default();
        if params.include_abstract {
            if let Some(abstract_text) = resolve_abstract(&paper, &sections) {
                paper_json["abstract_text"] = serde_json::json!(abstract_text);
            }
        } else {
            paper_json["abstract_text"] = serde_json::Value::Null;
        }

        let response = if params.include_sections {
            serde_json::json!({
                "paper": paper_json,
                "extracted_sections": sections.to_views(),
            })
        } else {
            serde_json::json!({ "paper": paper_json })
        };
        Ok(tool_result_text(json_text(&response)))
    }

    #[rmcp::tool(
        description = "Discover papers with semantic affinity to a given paper. Results include relevance scores (0.0–1.0)."
    )]
    async fn find_similar_papers(
        &self,
        Parameters(params): Parameters<FindSimilarPapersParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.limit.min(MAX_RESULT_LIMIT);
        validate_paper_id(&params.paper_id)?;

        let engine = self.search_engine.read().await.clone();
        let results = engine
            .find_similar(&params.paper_id, None, limit, DEFAULT_MIN_SCORE)
            .await
            .mcp()?;

        Ok(tool_result_text(json_text(&scored_papers_to_json(
            &results,
        ))))
    }

    #[rmcp::tool(
        description = "Get library status: total papers indexed and total vectors in the search index."
    )]
    async fn get_index_status(
        &self,
        _params: Parameters<GetIndexStatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let papers = self.store.paper_count().await.mcp()?;
        let vectors = self.store.count().await.mcp()?;

        Ok(tool_result_text(json_text(&serde_json::json!({
            "papers": papers,
            "vectors": vectors,
        }))))
    }
}
