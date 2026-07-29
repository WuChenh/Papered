//! Zotero sync subsystem for Papered.
//!
//! Connects to the Zotero desktop app's local HTTP API for incremental
//! import, collection filtering, and background synchronization.

#![allow(non_snake_case)]

mod client;
mod source;
pub mod syncer;
mod types;

pub use client::{ZoteroApi, ZoteroClient, ZoteroItemListResponse};
pub use types::*;

pub fn build_zotero_extra(
    key: &str,
    item_type: &str,
    doi: Option<&str>,
    url: Option<&str>,
    extra_fields: Option<&str>,
) -> String {
    let required = vec![
        ("zotero_key", key.to_string()),
        ("item_type", item_type.to_string()),
    ];
    let optional = vec![
        ("doi", doi.map(|s| s.to_string())),
        ("url", url.map(|s| s.to_string())),
        ("extra", extra_fields.map(|s| s.to_string())),
    ];
    crate::util::build_extra_json(&required, &optional)
}

use crate::util::macos;

pub const DEFAULT_PORT: u16 = 23119;

pub fn discover_zotero_port() -> u16 {
    macos::discover_port(
        "org.zotero.zotero",
        "localAPIPort",
        "ZOTERO_PORT",
        DEFAULT_PORT,
    )
}

/// Zotero data directory. The `defaults read` resolution runs once per
/// process (cached) — this is called per item from async sync code.
pub fn zotero_data_dir() -> std::path::PathBuf {
    static DATA_DIR: std::sync::LazyLock<std::path::PathBuf> =
        std::sync::LazyLock::new(resolve_zotero_data_dir);
    DATA_DIR.clone()
}

fn resolve_zotero_data_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(path) = macos::read_defaults("org.zotero.zotero", "dataDir") {
            return std::path::PathBuf::from(path);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("Zotero")
}
