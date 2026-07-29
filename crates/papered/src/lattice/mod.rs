//! Lattice Local API client.
//!
//! Lattice is a macOS reference management application that exposes a
//! read-only HTTP API on `127.0.0.1:{port}`. This module provides
//! discovery and client types for interacting with that API.
//!
//! ## Attribution & Disclaimer
//!
//! Lattice is a trademark of the Lattice Project Contributors
//! (<https://github.com/stringer07/Lattice_release>). Papered communicates
//! with Lattice exclusively through its publicly documented Local API.
//! Papered is not affiliated with or endorsed by the Lattice project.
//!
//! This module contains original code that calls Lattice's HTTP API
//! endpoints; it does not include, derive from, or redistribute any
//! Lattice source code.

mod client;
pub mod syncer;
mod types;

pub use client::LatticeClient;
pub use client::urlencoding;
pub use types::*;

/// Build the `extra` JSON field from Lattice paper detail and optional CSL-JSON.
pub fn build_lattice_extra(p: &LatticePaperDetail, csl_json: Option<String>) -> String {
    let required = [
        ("lattice_id", p.id.clone()),
        ("citekey", p.citekey.clone()),
        ("paper_type", p.paper_type.clone()),
    ];
    let optional = [
        ("volume", p.volume.clone()),
        ("issue", p.issue.clone()),
        ("pages", p.pages.clone()),
        ("isbn", p.isbn.clone()),
    ];
    let mut extra = crate::util::build_extra_json(&required, &optional);
    if let Some(csl) = csl_json
        && let Ok(csl_val) = serde_json::from_str::<serde_json::Value>(&csl)
        && let Ok(mut map) =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&extra)
    {
        map.insert("csl_item".to_string(), csl_val);
        extra = serde_json::Value::Object(map).to_string();
    }
    extra
}

use crate::util::macos;

/// Default port for the Lattice Local API.
pub const DEFAULT_PORT: u16 = 29467;

const LATTICE_DEFAULTS_DOMAIN: &str = "com.aurelian.Lattice";
const PORT_DEFAULTS_KEY: &str = "citationBridgePort";

/// Discover the Lattice API port.
pub fn discover_port() -> u16 {
    macos::discover_port(
        LATTICE_DEFAULTS_DOMAIN,
        PORT_DEFAULTS_KEY,
        "LATTICE_PORT",
        DEFAULT_PORT,
    )
}
