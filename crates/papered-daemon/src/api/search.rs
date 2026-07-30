use axum::extract::Query;
use axum::{extract::State, response::Json};
use papered::paper::section::SectionType;
use std::sync::Arc;

use super::types::{
    ApiResult, ContentSearchRequest, GraphQuery, MAX_LIMIT, SearchRequest, SimilarRequest, map_err,
};
use crate::AppState;
use papered::util::paths::resolve_figure_search_results;

pub async fn search(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> ApiResult<Vec<papered::paper::PaperSearchResult>> {
    let section_type = req.section_type.and_then(|s| SectionType::from_name(&s));
    let limit = req.limit.min(MAX_LIMIT);
    let method = req.search_method.unwrap_or_default();
    let results = state
        .search_engine
        .read()
        .await
        .search_papers_by_method(&req.query, section_type, method, limit, req.min_score)
        .await
        .map_err(map_err)?;
    Ok(Json(results))
}

pub async fn find_similar(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SimilarRequest>,
) -> ApiResult<Vec<papered::paper::PaperSearchResult>> {
    let section_type = req.section_type.and_then(|s| SectionType::from_name(&s));
    let limit = req.limit.min(MAX_LIMIT);
    let results = state
        .search_engine
        .read()
        .await
        .find_similar(
            &req.paper_id,
            section_type,
            limit,
            papered::search::DEFAULT_MIN_SCORE,
        )
        .await
        .map_err(map_err)?;
    Ok(Json(results))
}

pub async fn search_figures(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ContentSearchRequest>,
) -> ApiResult<Vec<papered::search::FigureSearchResult>> {
    let limit = req.limit.min(MAX_LIMIT);
    let data_dir = state.config.read().await.data_dir.clone();
    let mut results = state
        .search_engine
        .read()
        .await
        .search_figures(&req.query, limit, req.min_score)
        .await
        .map_err(map_err)?;
    resolve_figure_search_results(&data_dir, &mut results).await;
    Ok(Json(results))
}

/// Lexical search over verbatim source-text chunks (passages) across all
/// papers. Surfaces the original document fragments that match the query,
/// rather than the LLM-processed section summaries used by `/search`.
pub async fn search_passages(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ContentSearchRequest>,
) -> ApiResult<Vec<papered::search::PassageSearchResult>> {
    let limit = req.limit.min(MAX_LIMIT);
    let results = state
        .search_engine
        .read()
        .await
        .search_passages(&req.query, limit)
        .await
        .map_err(map_err)?;
    Ok(Json(results))
}

/// Build the paper relatedness graph (keyword overlap + shared entities) over
/// the library. Serves the interactive network / timeline view.
pub async fn paper_graph(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GraphQuery>,
) -> ApiResult<papered::search::PaperGraph> {
    let limit = params.limit.min(MAX_LIMIT);
    let graph = state
        .search_engine
        .read()
        .await
        .paper_graph(limit, params.max_edges_per_node, params.focus.as_deref())
        .await
        .map_err(map_err)?;
    Ok(Json(graph))
}
