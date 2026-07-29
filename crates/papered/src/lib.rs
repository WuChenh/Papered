//! Core library for Papered — a semantic search and RAG engine for academic papers.
//!
//! This crate provides the full pipeline: PDF parsing, section extraction, embedding,
//! vector indexing, hybrid search, and RAG context assembly. It is accompanied by
//! `papered-daemon` (HTTP REST API + MCP server) and `papered-mcp` (MCP protocol
//! implementation) in the same workspace.

pub mod chunker;
pub mod client;
pub mod config;
pub mod cover;
pub mod error;
pub mod index;
pub mod json_repair;
pub mod lattice;
pub mod llm;
pub mod paper;
pub mod retrieval;
pub mod routes;
pub mod search;
pub mod store;
pub mod sync;
#[cfg(test)]
mod test_support;
pub mod util;
pub mod zotero;

pub use config::AppConfig;
pub use error::{ApiError, PaperedError, Result};
pub use index::indexer::Indexer;
pub use paper::ListPapersResponse;
pub use search::{FigureSearchResult, SearchEngine};
pub use store::create_store;
pub use store::pager::PaperPager;
pub use store::vector::VectorStore;
pub use util::IndexJob;
pub use util::str_enum::StrLabel;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A user-defined prompt template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub system_prompt: String,
    pub temperature: f32,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Prompt {
    pub fn new(name: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            description: None,
            system_prompt: system_prompt.into(),
            temperature: 0.2,
            is_default: false,
            created_at: now,
            updated_at: now,
        }
    }
}
