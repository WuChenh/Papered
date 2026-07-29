//! Indexing pipeline for Papered.
//!
//! Orchestrates PDF parsing, section extraction via LLM, semantic chunking,
//! figure/table extraction and description, and embedding generation before
//! persisting to the vector and metadata stores.

pub mod export;
pub mod indexer;
pub mod multimodal;
