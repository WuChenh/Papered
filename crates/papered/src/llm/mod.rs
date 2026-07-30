//! LLM subsystem for Papered.
//!
//! Coordinates batching embedding clients, neural rerankers, the RAG engine
//! for context assembly and generation, unified query enhancement, and
//! rate-limited LLM calls.

pub mod artifacts;
pub mod cache;
pub mod client;
pub mod embed;
pub mod headings;
pub mod insight;
pub mod metrics;
pub mod provider;
pub mod query_enhancer;
pub mod rag;
pub mod rate_limiter;
pub mod reranker;
pub mod transformer;
pub mod translation;

pub use provider::Provider;
