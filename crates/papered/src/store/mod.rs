//! Storage architecture for Papered.
//!
//! `TursoStore` is the sole storage backend. It uses Turso's built-in
//! SQL engine, Tantivy-backed FTS indexes, and native vector functions
//! (`vector32`, `vector_distance_cos`) for a unified pure-Rust storage
//! layer in a single `.db` file.

pub mod pager;
pub mod turso;
pub mod vector;

use crate::error::Result;
use crate::store::vector::VectorStore;
use std::path::Path;
use std::sync::Arc;

pub async fn create_store(db_path: &Path) -> Result<Arc<dyn VectorStore>> {
    let store = turso::TursoStore::new(db_path).await?;
    Ok(Arc::new(store))
}
