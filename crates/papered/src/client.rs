//! HTTP client for the Papered daemon CLI.
//!
//! Provides a thin async wrapper around `reqwest` for talking to the
//! `papered-daemon` REST API. Used by the `papered` CLI binary.

use std::time::Duration;

/// Shared HTTP client for daemon API calls.
pub struct DaemonClient {
    /// Underlying reqwest client.
    pub client: reqwest::Client,
    /// Daemon base URL.
    pub base_url: String,
}

impl DaemonClient {
    pub fn new(base_url: String) -> crate::error::Result<Self> {
        Ok(Self {
            client: build_http_client(120, None)?,
            base_url,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub async fn health(&self) -> Result<bool, reqwest::Error> {
        match self
            .client
            .get(self.url(crate::routes::HEALTH))
            // Health probes must fail fast: the client default timeout (120s)
            // would let a wedged non-daemon listener stall daemon discovery.
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(e) if e.is_connect() || e.is_timeout() => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Build a shared `reqwest::Client` with configurable timeouts.
///
/// All workspace crates should use this helper instead of ad-hoc
/// `reqwest::Client::builder()` calls to ensure consistent error handling.
pub fn build_http_client(
    timeout_secs: u64,
    connect_timeout_secs: Option<u64>,
) -> crate::error::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(timeout_secs));
    if let Some(ct) = connect_timeout_secs {
        builder = builder.connect_timeout(Duration::from_secs(ct));
    }
    builder.build().map_err(crate::error::PaperedError::Http)
}

/// Send an HTTP request to the daemon.
pub async fn api_request(
    client: &DaemonClient,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> crate::Result<reqwest::Response> {
    let url = client.url(path);
    let req = match method {
        reqwest::Method::GET => client.client.get(url),
        reqwest::Method::POST => {
            let mut r = client.client.post(url);
            if let Some(json) = body {
                r = r.json(&json);
            }
            r
        }
        reqwest::Method::DELETE => client.client.delete(url),
        m => {
            return Err(crate::PaperedError::invalid_argument(format!(
                "unsupported HTTP method: {m}"
            )));
        }
    };
    Ok(req.send().await?)
}

/// Check that a response is successful, otherwise return a formatted error.
pub async fn check_response(resp: reqwest::Response) -> crate::Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) if !t.is_empty() => t,
        _ => format!("HTTP {status}"),
    };
    Err(crate::PaperedError::Unknown(format!(
        "HTTP {status}: {text}"
    )))
}

/// Send a daemon API request and deserialize the JSON response.
pub async fn api_send_json<T: serde::de::DeserializeOwned>(
    client: &DaemonClient,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> crate::Result<T> {
    let resp = api_request(client, method, path, body).await?;
    let resp = check_response(resp).await?;
    Ok(resp.json().await?)
}

/// Send a GET request and deserialize the JSON response.
pub async fn api_get_json<T: serde::de::DeserializeOwned>(
    client: &DaemonClient,
    path: &str,
) -> crate::Result<T> {
    api_send_json(client, reqwest::Method::GET, path, None).await
}

/// Read the body of an unsuccessful response for error reporting.
pub(crate) async fn error_body(resp: reqwest::Response) -> String {
    resp.text()
        .await
        .unwrap_or_else(|e| format!("(failed to read error body: {e})"))
}

/// Format a raw HTTP status code the way `reqwest::StatusCode` displays it
/// (e.g. `404 Not Found`), so error messages keep their historical wording.
pub(crate) fn status_text(code: u16) -> String {
    match reqwest::StatusCode::from_u16(code) {
        Ok(status) => status.to_string(),
        Err(_) => code.to_string(),
    }
}

/// Map a non-2xx response to an error, keeping the status text and response
/// body for diagnostics. Shared by the Lattice and Zotero API clients.
pub(crate) fn http_status_error(status: u16, body: String) -> crate::PaperedError {
    crate::PaperedError::Unknown(format!("HTTP {}: {body}", status_text(status)))
}

/// True when `host` is a loopback address (localhost, 127.0.0.1, or ::1).
/// IPv6 addresses wrapped in brackets (`[::1]`) are correctly stripped before
/// comparison. Used by the daemon's config probe (to forward API keys safely)
/// and the MCP server's origin validation.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.to_lowercase();
    let h = h.trim_matches(['[', ']']);
    matches!(h, "localhost" | "127.0.0.1" | "::1")
}

/// Maximum attempts (1 initial + 5 retries) for transient HTTP failures.
const HTTP_MAX_ATTEMPTS: u32 = 6;

/// Base backoff (before jitter) for the retry following `attempt` (1-based):
/// 1s after the first failure, doubling per retry, capped at 30s.
const BACKOFF_BASE_MS: u64 = 1_000;
const BACKOFF_CAP_MS: u64 = 30_000;

/// Exponential backoff before the retry following `attempt` (1-based):
/// 1s after the first failure, doubling each retry and capped at 30s. Jitter
/// (50–100% of the nominal delay) spreads concurrent retries, so a burst of
/// papers indexing together does not re-collide on a flaky remote API.
pub(crate) fn retry_backoff(attempt: u32) -> Duration {
    #[cfg(test)]
    {
        if let Some(delay) = TEST_RETRY_BACKOFF.with(|c| c.get()) {
            return delay;
        }
    }
    let nominal_ms = BACKOFF_BASE_MS
        .saturating_mul(1u64 << (attempt - 1).min(15))
        .min(BACKOFF_CAP_MS);
    let jittered_ms = nominal_ms as f64 * jitter_factor();
    Duration::from_millis(jittered_ms.round() as u64)
}

/// Jitter factor in [0.5, 1.0). Seeded from the process RNG via
/// `RandomState`, so concurrent retries spread without collapsing the
/// backoff floor.
fn jitter_factor() -> f64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u8(0);
    // High 53 bits → uniform [0, 1), scaled into [0.5, 1.0).
    0.5 + (((hasher.finish() >> 11) as f64) / ((1u64 << 53) as f64)) * 0.5
}

#[cfg(test)]
thread_local! {
    /// Test-only override: when set, `retry_backoff` returns this delay
    /// unchanged, keeping retry-loop tests fast.
    static TEST_RETRY_BACKOFF: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// Extract a `Retry-After` delay (delta-seconds form) from a response.
fn retry_after_delay(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Returns `true` when the given status code signals a transient condition
/// worth retrying (5xx server error or 429 rate-limit).
pub(crate) fn is_retryable(status: reqwest::StatusCode) -> bool {
    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// Core retry loop: builds requests, retries transient failures, and
/// returns the final `reqwest::Response` on success or the caller's error
/// on non-retryable status codes.
async fn send_retry_core<B: serde::Serialize + ?Sized>(
    client: &reqwest::Client,
    url: &str,
    method: reqwest::Method,
    headers: reqwest::header::HeaderMap,
    api_key: Option<&str>,
    body: Option<&B>,
    err: impl Fn(u16, String) -> crate::PaperedError,
) -> crate::Result<reqwest::Response> {
    for attempt in 1..=HTTP_MAX_ATTEMPTS {
        let mut request = client.request(method.clone(), url).headers(headers.clone());
        if let Some(key) = api_key.filter(|k| !k.is_empty()) {
            request = request.bearer_auth(key);
        }
        if let Some(b) = body {
            request = request.json(b);
        }
        match request.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp);
                }
                if is_retryable(status) && attempt < HTTP_MAX_ATTEMPTS {
                    let delay = retry_after_delay(&resp).unwrap_or_else(|| retry_backoff(attempt));
                    tracing::warn!(
                        "{method} {url} failed with {status} (attempt {attempt}/{HTTP_MAX_ATTEMPTS}); retrying in {delay:?}"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(err(status.as_u16(), error_body(resp).await));
            }
            Err(e) => {
                if attempt < HTTP_MAX_ATTEMPTS {
                    let delay = retry_backoff(attempt);
                    tracing::warn!(
                        "{method} {url} transport error: {e} (attempt {attempt}/{HTTP_MAX_ATTEMPTS}); retrying in {delay:?}"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(crate::PaperedError::Http(e));
            }
        }
    }
    unreachable!("loop should return before exhausting all attempts")
}

/// Send an HTTP request with retries for transient failures.
///
/// Accepts any method (GET, POST, ...) and an optional request body. Retries
/// 5xx, 429, and transport errors up to [`HTTP_MAX_ATTEMPTS`] times with
/// exponential backoff and jitter, honouring `Retry-After` when present. 4xx
/// errors (other than 429) fail immediately.
pub(crate) async fn send_with_retry<
    B: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
>(
    client: &reqwest::Client,
    url: &str,
    method: reqwest::Method,
    headers: reqwest::header::HeaderMap,
    api_key: Option<&str>,
    body: Option<&B>,
    err: impl Fn(u16, String) -> crate::PaperedError,
) -> crate::Result<T> {
    let resp = send_retry_core(client, url, method, headers, api_key, body, err).await?;
    resp.json::<T>().await.map_err(crate::PaperedError::Http)
}

/// Like [`send_with_retry`] but returns the raw `reqwest::Response` instead
/// of deserializing JSON. Used by Zotero pagination where the caller needs
/// response headers and body.
pub(crate) async fn send_with_retry_raw<B: serde::Serialize + ?Sized>(
    client: &reqwest::Client,
    url: &str,
    method: reqwest::Method,
    headers: reqwest::header::HeaderMap,
    api_key: Option<&str>,
    body: Option<&B>,
    err: impl Fn(u16, String) -> crate::PaperedError,
) -> crate::Result<reqwest::Response> {
    send_retry_core(client, url, method, headers, api_key, body, err).await
}

/// POST `body` as JSON and deserialize the JSON response, with optional bearer
/// auth. Retries transient failures.
pub(crate) async fn post_json<B: serde::Serialize + ?Sized, T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    body: &B,
    api_key: Option<&str>,
    err: impl Fn(u16, String) -> crate::PaperedError,
) -> crate::Result<T> {
    send_with_retry::<B, T>(
        client,
        url,
        reqwest::Method::POST,
        reqwest::header::HeaderMap::new(),
        api_key,
        Some(body),
        err,
    )
    .await
}

/// GET JSON with the given headers and retries, erroring on non-2xx.
pub(crate) async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    headers: reqwest::header::HeaderMap,
    err: impl Fn(u16, String) -> crate::PaperedError,
) -> crate::Result<T> {
    send_with_retry::<serde_json::Value, T>(
        client,
        url,
        reqwest::Method::GET,
        headers,
        None,
        None,
        err,
    )
    .await
}

/// Send a POST request and deserialize the JSON response.
pub async fn api_post_json<T: serde::de::DeserializeOwned>(
    client: &DaemonClient,
    path: &str,
    body: Option<serde_json::Value>,
) -> crate::Result<T> {
    api_send_json(client, reqwest::Method::POST, path, body).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Spawn a single-threaded mock HTTP server that answers each request
    /// with the next status in `statuses` (the last entry repeats).
    /// Retryable statuses (429 and 5xx) carry `Retry-After: 0` so retry-loop
    /// tests stay fast. Returns the server URL and a shared request counter.
    fn spawn_status_server(statuses: Vec<u16>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let n = count2.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let status = statuses[n.min(statuses.len() - 1)];
                let (reason, body) = match status {
                    200 => ("OK", r#"{"ok":true}"#),
                    404 => ("Not Found", "not found"),
                    429 => ("Too Many Requests", "rate limited"),
                    _ => ("Internal Server Error", "server error"),
                };
                let retry_after = if status == 429 || status >= 500 {
                    "Retry-After: 0\r\n"
                } else {
                    ""
                };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{retry_after}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (format!("http://{addr}/"), count)
    }

    async fn post(url: &str) -> crate::Result<serde_json::Value> {
        let client = build_http_client(5, None).unwrap();
        post_json(
            &client,
            url,
            &serde_json::json!({"q": 1}),
            None,
            |status, body| crate::PaperedError::Unknown(format!("HTTP {status}: {body}")),
        )
        .await
    }

    #[tokio::test]
    async fn post_json_retries_on_5xx_then_succeeds() {
        let (url, count) = spawn_status_server(vec![500, 500, 200]);
        let value = post(&url).await.expect("should succeed after retries");
        assert_eq!(value["ok"], true);
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn post_json_gives_up_after_max_attempts() {
        let (url, count) = spawn_status_server(vec![500]);
        let err = post(&url).await.expect_err("should fail after retries");
        assert_eq!(
            count.load(Ordering::SeqCst),
            super::HTTP_MAX_ATTEMPTS as usize
        );
        assert!(err.to_string().contains("500"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn post_json_does_not_retry_4xx() {
        let (url, count) = spawn_status_server(vec![404]);
        let err = post(&url).await.expect_err("404 must not be retried");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(err.to_string().contains("404"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn post_json_retries_429_respecting_retry_after() {
        let (url, count) = spawn_status_server(vec![429, 200]);
        let start = std::time::Instant::now();
        let value = post(&url).await.expect("should succeed after 429");
        assert_eq!(value["ok"], true);
        assert_eq!(count.load(Ordering::SeqCst), 2);
        // Retry-After: 0 must be honored instead of the 500ms backoff.
        assert!(
            start.elapsed() < Duration::from_millis(400),
            "Retry-After was not honored: {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn post_json_retries_transport_error() {
        // Bind then drop a listener to get a port that refuses connections.
        let addr = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let url = format!("http://{addr}/");
        // Collapse the backoff so exhausting all attempts stays fast.
        TEST_RETRY_BACKOFF.with(|c| c.set(Some(Duration::from_millis(1))));
        let err = post(&url).await.expect_err("connection refused must fail");
        assert!(
            matches!(err, crate::PaperedError::Http(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn retry_backoff_grows_exponentially_with_jitter() {
        // Each delay stays within 50–100% of the nominal 1s→2s→4s→8s→16s
        // schedule, and the cap holds for far-future attempts.
        let nominal_ms = [1_000u64, 2_000, 4_000, 8_000, 16_000];
        for (attempt, &nominal) in nominal_ms.iter().enumerate() {
            let delay = retry_backoff(attempt as u32 + 1);
            let ms = delay.as_millis() as u64;
            assert!(
                ms >= nominal / 2 && ms <= nominal,
                "attempt {}: {delay:?} outside jitter window for {nominal}ms",
                attempt + 1
            );
        }
        let capped = retry_backoff(30);
        assert!(
            capped.as_millis() >= 15_000 && capped.as_millis() <= 30_000,
            "cap violated: {capped:?}"
        );
    }
}
