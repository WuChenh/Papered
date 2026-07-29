use super::discover_zotero_port;
use super::types::*;
use crate::error::{PaperedError, Result};
use async_trait::async_trait;

/// Trait abstracting the Zotero local HTTP API.
/// Enables mocking the client for unit tests.
#[async_trait]
pub trait ZoteroApi: Send + Sync {
    async fn list_top_items(&self, limit: u32, since: u64) -> Result<ZoteroItemListResponse>;
    async fn list_collections(&self) -> Result<Vec<ZoteroCollection>>;
    async fn get_collection_items(
        &self,
        collection_key: &str,
        limit: u32,
        since: u64,
    ) -> Result<ZoteroItemListResponse>;
    async fn get_collection_top_items(
        &self,
        collection_key: &str,
        limit: u32,
        since: u64,
    ) -> Result<ZoteroItemListResponse>;
    async fn get_children(&self, parent_key: &str) -> Result<Vec<ZoteroChildItem>>;
    async fn download_file(&self, item_key: &str) -> Result<Vec<u8>>;
}

#[derive(Debug, Clone)]
pub struct ZoteroClient {
    http: reqwest::Client,
    base_url: String,
}

impl ZoteroClient {
    fn build_http() -> reqwest::Client {
        crate::client::build_http_client(30, Some(3)).unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to build Zotero HTTP client with custom config: {e}, using default"
            );
            reqwest::Client::new()
        })
    }

    pub fn new() -> Self {
        let port = discover_zotero_port();
        let base_url = format!("http://127.0.0.1:{port}/api");
        Self {
            http: Self::build_http(),
            base_url,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn headers() -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Zotero-API-Version",
            reqwest::header::HeaderValue::from_static("3"),
        );
        headers
    }

    pub async fn status(&self) -> Result<ZoteroApiStatus> {
        let url = format!("{}/keys/current", self.base_url);
        crate::client::get_json(
            &self.http,
            &url,
            Self::headers(),
            crate::client::http_status_error,
        )
        .await
    }

    async fn paginate_items(&self, base_url: &str) -> Result<ZoteroItemListResponse> {
        let mut all_items = Vec::new();
        let mut last_modified_version: u64 = 0;
        let mut start: u32 = 0;
        loop {
            let url = if start == 0 {
                base_url.to_string()
            } else {
                format!("{base_url}&start={start}")
            };
            let resp = crate::client::send_with_retry_raw::<serde_json::Value>(
                &self.http,
                &url,
                reqwest::Method::GET,
                Self::headers(),
                None,
                None,
                crate::client::http_status_error,
            )
            .await?;
            let total_results: u32 = resp
                .headers()
                .get("Total-Results")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            last_modified_version = resp
                .headers()
                .get("Last-Modified-Version")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(last_modified_version);
            let body_text = resp.text().await?;
            let items: Vec<ZoteroItem> = serde_json::from_str(&body_text).map_err(|e| {
                let preview = body_text.chars().take(800).collect::<String>();
                tracing::error!(
                    "Failed to decode Zotero items JSON from {url}: {e}. Body preview: {preview}"
                );
                PaperedError::Unknown(format!(
                    "Zotero JSON decode error for {url}: {e}. Preview: {preview}"
                ))
            })?;
            let batch_len = items.len() as u32;
            all_items.extend(items);
            start += batch_len;
            if start >= total_results || batch_len == 0 {
                break;
            }
        }
        Ok(ZoteroItemListResponse {
            items: all_items,
            last_modified_version,
        })
    }
}

#[async_trait]
impl ZoteroApi for ZoteroClient {
    async fn list_top_items(&self, limit: u32, since: u64) -> Result<ZoteroItemListResponse> {
        let url = format!(
            "{}/users/0/items/top?limit={}&since={}&format=json",
            self.base_url, limit, since
        );
        self.paginate_items(&url).await
    }

    async fn list_collections(&self) -> Result<Vec<ZoteroCollection>> {
        let url = format!("{}/users/0/collections?format=json", self.base_url);
        crate::client::get_json(
            &self.http,
            &url,
            Self::headers(),
            crate::client::http_status_error,
        )
        .await
    }

    async fn get_collection_items(
        &self,
        collection_key: &str,
        limit: u32,
        since: u64,
    ) -> Result<ZoteroItemListResponse> {
        let url = format!(
            "{}/users/0/collections/{}/items?limit={}&since={}&format=json",
            self.base_url, collection_key, limit, since
        );
        self.paginate_items(&url).await
    }

    async fn get_collection_top_items(
        &self,
        collection_key: &str,
        limit: u32,
        since: u64,
    ) -> Result<ZoteroItemListResponse> {
        let url = format!(
            "{}/users/0/collections/{}/items/top?limit={}&since={}&format=json",
            self.base_url, collection_key, limit, since
        );
        self.paginate_items(&url).await
    }

    async fn get_children(&self, parent_key: &str) -> Result<Vec<ZoteroChildItem>> {
        let url = format!(
            "{}/users/0/items/{}/children?format=json",
            self.base_url, parent_key
        );
        crate::client::get_json(
            &self.http,
            &url,
            Self::headers(),
            crate::client::http_status_error,
        )
        .await
    }

    async fn download_file(&self, item_key: &str) -> Result<Vec<u8>> {
        let url = format!("{}/users/0/items/{}/file", self.base_url, item_key);
        let resp = self.http.get(&url).headers(Self::headers()).send().await?;
        if !resp.status().is_success() {
            return Err(PaperedError::Http(resp.error_for_status().unwrap_err()));
        }
        Ok(resp.bytes().await?.to_vec())
    }
}

impl Default for ZoteroClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(non_snake_case)]
pub struct ZoteroApiStatus {
    pub userID: u32,
    pub username: String,
    pub access: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ZoteroItemListResponse {
    pub items: Vec<ZoteroItem>,
    pub last_modified_version: u64,
}
