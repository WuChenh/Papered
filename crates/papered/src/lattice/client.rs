//! HTTP client for the Lattice Local API.

use super::discover_port;
use super::types::*;
use crate::error::{PaperedError, Result};

/// Client for Lattice's local HTTP API.
///
/// Communicates with a running Lattice desktop application on localhost.
/// No authentication is required — security is enforced through loopback binding.
#[derive(Debug, Clone)]
pub struct LatticeClient {
    http: reqwest::Client,
    base_url: String,
}

impl LatticeClient {
    /// Build a reqwest::Client with timeouts suitable for localhost.
    fn build_http() -> Result<reqwest::Client> {
        crate::client::build_http_client(10, Some(3))
    }

    /// Create a new client, auto-discovering the Lattice port.
    pub fn new() -> Result<Self> {
        let port = discover_port();
        let base_url = format!("http://127.0.0.1:{port}/api/v1");
        Ok(Self {
            http: Self::build_http()?,
            base_url,
        })
    }

    /// The base URL this client is targeting.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // --- API methods ---

    /// Check if Lattice is reachable and get capabilities.
    pub async fn status(&self) -> Result<LatticeStatus> {
        let url = format!("{}/status", self.base_url);
        crate::client::get_json(
            &self.http,
            &url,
            reqwest::header::HeaderMap::new(),
            crate::client::http_status_error,
        )
        .await
    }

    /// List collections exposed by the Lattice Local API.
    ///
    /// The endpoint returns a bare JSON array of collections (verified
    /// against Lattice 2.4.2, apiVersion 1) — not a wrapped object.
    pub async fn list_collections(&self) -> Result<Vec<LatticeCollection>> {
        let url = format!("{}/collections", self.base_url);
        crate::client::get_json(
            &self.http,
            &url,
            reqwest::header::HeaderMap::new(),
            crate::client::http_status_error,
        )
        .await
    }

    /// Search Lattice papers by query.
    ///
    /// An empty query returns recently added papers.
    pub async fn search(&self, query: &str, limit: u32) -> Result<LatticeSearchResponse> {
        self.search_collection_page(query, None, limit, 0).await
    }

    /// Search Lattice papers by query, optionally within a single collection.
    ///
    /// `collection` is a collection id. Pass `None` for a library-wide search.
    pub async fn search_collection_page(
        &self,
        query: &str,
        collection: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<LatticeSearchResponse> {
        let collection_param = collection
            .map(|id| format!("&collection={}", urlencoding(id)))
            .unwrap_or_default();
        let url = format!(
            "{}/search?q={}&limit={}&offset={}{}",
            self.base_url,
            urlencoding(query),
            limit,
            offset,
            collection_param
        );
        crate::client::get_json(
            &self.http,
            &url,
            reqwest::header::HeaderMap::new(),
            crate::client::http_status_error,
        )
        .await
    }

    /// Get full paper detail including CSL-JSON item.
    ///
    /// Requests `pdfPath` and `abstract` via the `?include=` query so callers
    /// get the Lattice-resolved PDF path (authoritative, vs. guessing by
    /// filename) and any stored abstract without an extra round trip.
    pub async fn get_paper(&self, id: &str) -> Result<LatticePaperDetail> {
        let url = format!("{}/papers/{}?include=pdfPath,abstract", self.base_url, id);
        crate::client::get_json(
            &self.http,
            &url,
            reqwest::header::HeaderMap::new(),
            |status, body| {
                if status == reqwest::StatusCode::NOT_FOUND.as_u16() {
                    PaperedError::NotFound(format!("Paper not found in Lattice: {id}"), None)
                } else {
                    crate::client::http_status_error(status, body)
                }
            },
        )
        .await
    }
}

/// URL-encode a string for safe inclusion in query parameters.
/// Encodes all characters except RFC 3986 unreserved characters.
pub fn urlencoding(s: &str) -> String {
    use std::fmt::Write as _;
    let mut result = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push_str("%20"),
            _ => write!(&mut result, "%{byte:02X}").expect("writing to a String cannot fail"),
        }
    }
    result
}
