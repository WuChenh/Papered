use super::discover_zotero_port;
use super::types::*;
use crate::client::is_loopback_host;
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

    #[cfg(test)]
    fn for_test(base_url: String) -> Self {
        Self {
            http: Self::build_http(),
            base_url,
        }
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
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.bytes().await?.to_vec());
        }
        // Zotero's local API answers stored-file requests with a redirect to
        // a `file://` URL. reqwest cannot follow non-HTTP redirects, so read
        // the file directly from disk instead.
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok());
            if let Some(path) = location.and_then(file_url_to_path) {
                return tokio::fs::read(&path).await.map_err(|e| {
                    PaperedError::io_other(format!(
                        "Zotero file redirect target {path} unreadable: {e}"
                    ))
                });
            }
        }
        let body = resp.text().await.unwrap_or_default();
        Err(crate::client::http_status_error(status.as_u16(), body))
    }
}

/// Convert a `file://` URL (as emitted by Zotero's local API) into a local
/// filesystem path, percent-decoding UTF-8 escapes. Returns `None` for
/// non-`file://` URLs, non-loopback hosts, or malformed escapes.
fn file_url_to_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file://")?;
    let path = if let Some(p) = rest.strip_prefix('/') {
        // `file:///path` — the common form.
        format!("/{p}")
    } else {
        // `file://host/path` — only a loopback host is meaningful here.
        let (host, p) = rest.split_once('/')?;
        if !is_loopback_host(host) {
            return None;
        }
        format!("/{p}")
    };
    let mut decoded = percent_decode(&path)?;
    // Windows drive paths arrive as `/C:/...`; strip the leading slash.
    let b = decoded.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        decoded.remove(0);
    }
    Some(decoded)
}

/// Percent-decode a URL path (UTF-8). Returns `None` on malformed `%`
/// escapes or invalid UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_string());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn percent_decode_handles_plain_escaped_and_malformed() {
        assert_eq!(percent_decode("/a/b.pdf"), Some("/a/b.pdf".to_string()));
        assert_eq!(
            percent_decode("/a/some%20paper%20-%20T%C3%ADtulo.pdf"),
            Some("/a/some paper - Título.pdf".to_string())
        );
        assert_eq!(percent_decode("/a/%zz.pdf"), None);
        assert_eq!(percent_decode("/a/%4"), None);
    }

    #[test]
    fn file_url_to_path_decodes_and_rejects_non_local() {
        assert_eq!(
            file_url_to_path("file:///Users/u/Zotero/storage/ABC/paper%20%26%20more.pdf"),
            Some("/Users/u/Zotero/storage/ABC/paper & more.pdf".to_string())
        );
        assert_eq!(
            file_url_to_path("file://localhost/tmp/x.pdf"),
            Some("/tmp/x.pdf".to_string())
        );
        assert_eq!(
            file_url_to_path("file://127.0.0.1/tmp/x.pdf"),
            Some("/tmp/x.pdf".to_string())
        );
        assert_eq!(
            file_url_to_path("file://[::1]/tmp/x.pdf"),
            Some("/tmp/x.pdf".to_string())
        );
        assert_eq!(
            file_url_to_path("file:///C:/Users/u/paper.pdf"),
            Some("C:/Users/u/paper.pdf".to_string())
        );
        assert_eq!(file_url_to_path("https://example.com/x.pdf"), None);
        assert_eq!(file_url_to_path("file://otherhost/tmp/x.pdf"), None);
    }

    /// Percent-encode a path the way Zotero emits `file://` URLs (UTF-8,
    /// keeping `/` and unreserved characters).
    fn percent_encode_path(path: &str) -> String {
        let mut out = String::new();
        for &b in path.as_bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    /// Spawn a mock Zotero server answering every request with the given
    /// status line and optional Location header.
    fn spawn_redirect_server(status_line: &'static str, location: Option<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let location_header = location
                    .as_deref()
                    .map(|l| format!("Location: {l}\r\n"))
                    .unwrap_or_default();
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\n{location_header}Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}/api")
    }

    #[tokio::test]
    async fn download_file_reads_local_file_on_redirect() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("some paper - 2026 - Título & more.pdf");
        std::fs::write(&file_path, b"pdf-bytes").unwrap();

        let location = format!(
            "file://{}",
            percent_encode_path(file_path.to_str().unwrap())
        );
        let base_url = spawn_redirect_server("302 Found", Some(location));

        let client = ZoteroClient::for_test(base_url);
        let bytes = client
            .download_file("ABC123")
            .await
            .expect("file:// redirect should be read from disk");
        assert_eq!(bytes, b"pdf-bytes");
    }

    #[tokio::test]
    async fn download_file_returns_error_on_404_instead_of_panicking() {
        let base_url = spawn_redirect_server("404 Not Found", None);
        let client = ZoteroClient::for_test(base_url);
        let err = client
            .download_file("MISSING")
            .await
            .expect_err("404 must be an error, not a panic");
        assert!(err.to_string().contains("404"), "unexpected error: {err}");
    }
}
