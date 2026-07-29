//! Paper indexing pipeline.
//!
//! This module is split into submodules by responsibility:
//! - `core`: main `Indexer` struct and document ingestion pipeline
//! - `figures`: figure/table multimodal indexing and LLM description
//! - `images`: standalone image indexing
//! - `reindex`: reindex, sections-only reindex, and re-embed operations
//! - `helpers`: shared helper functions for JSON parsing, metadata extraction, etc.

pub mod core;
pub mod figures;
pub mod helpers;
pub mod images;
pub mod reindex;

pub use core::Indexer;
