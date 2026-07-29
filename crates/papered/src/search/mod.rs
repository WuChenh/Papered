//! Search subsystem for Papered.
//!
//! Provides semantic search via Turso vector functions, full-text search via
//! Turso's tantivy-backed FTS indexes, and hybrid fusion with Reciprocal Rank
//! Fusion (RRF). Includes neural reranking, query complexity analysis, and
//! result caching.

pub mod engine;
pub mod graph;
pub mod method;
pub mod query_analyzer;

pub use engine::{FigureSearchResult, PassageSearchResult, SearchEngine};
pub use graph::{GraphEdge, GraphNode, PaperGraph};
pub use method::SearchMethod;

/// Maximum number of results any search or list endpoint may return.
/// Shared by the REST daemon and the MCP server so limits cannot drift.
pub const MAX_RESULT_LIMIT: usize = 1000;

/// Default minimum similarity score when a caller does not specify one.
pub const DEFAULT_MIN_SCORE: f32 = 0.1;
