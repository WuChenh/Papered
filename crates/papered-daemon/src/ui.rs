//! Embedded web UI: static assets compiled into the daemon binary.

use axum::{
    body::Body,
    extract::Path,
    http::{HeaderValue, Response, StatusCode, header},
    response::Redirect,
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "ui/"]
struct UiAssets;

const CSP: &str = "default-src 'self'; style-src 'self' 'unsafe-inline'";

/// Content-Type for the asset kinds shipped in `ui/`. Everything else is
/// octet-stream; embedded keys are literal, so `..` segments can never
/// escape — an unknown key simply falls back to `index.html`.
fn content_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

/// Resolve a `/ui/` path to `(content_type, bytes)`. Unknown paths (SPA
/// client-side routes) fall back to `index.html`.
pub(crate) fn resolve_asset(path: &str) -> (&'static str, Vec<u8>) {
    let path = path.trim_start_matches('/');
    if !path.is_empty()
        && let Some(file) = UiAssets::get(path)
    {
        return (content_type_for(path), file.data.into_owned());
    }
    let index = UiAssets::get("index.html").expect("index.html is embedded");
    ("text/html; charset=utf-8", index.data.into_owned())
}

fn ui_response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response<Body> {
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    resp
}

/// `GET /` and `GET /ui` → redirect to the canonical `/ui/`.
pub async fn ui_redirect() -> Redirect {
    Redirect::temporary("/ui/")
}

/// `GET /ui/` → the SPA entry point.
pub async fn serve_index() -> Response<Body> {
    let (ct, bytes) = resolve_asset("");
    ui_response(StatusCode::OK, ct, bytes)
}

/// `GET /ui/{*path}` → an embedded asset, or `index.html` for unknown paths.
pub async fn serve_ui(Path(path): Path<String>) -> Response<Body> {
    let (ct, bytes) = resolve_asset(&path);
    ui_response(StatusCode::OK, ct, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_asset_with_js_content_type() {
        let (ct, bytes) = resolve_asset("js/app.js");
        assert_eq!(ct, "text/javascript; charset=utf-8");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn unknown_path_falls_back_to_index_html() {
        let (ct, bytes) = resolve_asset("no/such/route");
        assert_eq!(ct, "text/html; charset=utf-8");
        let html = String::from_utf8(bytes).expect("index.html is utf-8");
        assert!(html.contains("id=\"app\""));
    }

    #[test]
    fn empty_path_resolves_to_index_html() {
        let (ct, _) = resolve_asset("");
        assert_eq!(ct, "text/html; charset=utf-8");
    }

    #[test]
    fn css_content_type() {
        let (ct, _) = resolve_asset("style.css");
        assert_eq!(ct, "text/css; charset=utf-8");
    }

    #[test]
    fn ui_response_sets_csp_header() {
        let resp = ui_response(StatusCode::OK, "text/plain", b"x".to_vec());
        assert_eq!(
            resp.headers().get(header::CONTENT_SECURITY_POLICY).unwrap(),
            "default-src 'self'; style-src 'self' 'unsafe-inline'"
        );
    }
}
