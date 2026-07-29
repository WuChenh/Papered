//! MinerU API Client
//!
//! Supports two modes:
//!
//! 1. **Precision API** — MinerU.net `/api/v4/extract/task` (token required).
//!    - Higher accuracy, up to 200 pages
//!    - Batch signed-upload flow
//!    - Downloads ZIP result, extracts Markdown
//!
//! 2. **Lightweight API** — MinerU.net Agent Lightweight API. No token required.
//!    - Uploads file via signed URL
//!    - Polls for completion
//!    - Downloads Markdown result

use crate::config::PdfExtractionConfig;
use crate::error::{PaperedError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use strum::{Display, EnumString, IntoStaticStr};

/// MinerU extraction mode: lightweight (no token) or precision (v4 token API).
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    EnumString,
    Display,
    IntoStaticStr,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase", ascii_case_insensitive)]
pub enum MinerUMode {
    #[default]
    Precision,
    Lightweight,
}

impl MinerUMode {
    /// Human-readable API label for logging.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Precision => "Precision",
            Self::Lightweight => "Agent",
        }
    }

    /// Base URL of the mode's MinerU API.
    #[must_use]
    pub const fn base_url(&self) -> &'static str {
        match self {
            Self::Precision => "https://mineru.net/api/v4",
            Self::Lightweight => "https://mineru.net/api/v1/agent",
        }
    }
}

/// Configuration for MinerU API integration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MinerUConfig {
    /// Whether to enable MinerU extraction.
    pub enabled: bool,
    /// API mode: lightweight (no token) or precision (v4 token API).
    #[serde(default)]
    pub mode: MinerUMode,
    /// Model version: "pipeline" (default), "vlm", or "MinerU-HTML" (precision only).
    #[serde(default = "default_model_version")]
    pub model_version: String,
    /// Request timeout in seconds for individual HTTP calls.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// API key. Required for Precision mode.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Maximum polling time in seconds.
    #[serde(default = "default_max_poll_time")]
    pub max_poll_time_secs: u64,
    /// Enable table recognition.
    #[serde(default = "default_true")]
    pub enable_table: bool,
    /// Enable OCR for scanned pages.
    #[serde(default = "default_true")]
    pub is_ocr: bool,
    /// Enable formula recognition.
    #[serde(default = "default_true")]
    pub enable_formula: bool,
    /// Document language for OCR and text extraction.
    #[serde(default = "default_language")]
    pub language: String,
    /// Optional page range, e.g. "1-10". Lightweight only.
    #[serde(default)]
    pub page_range: Option<String>,
}

fn default_model_version() -> String {
    "pipeline".to_string()
}
fn default_timeout() -> u64 {
    30
}
fn default_true() -> bool {
    true
}
fn default_max_poll_time() -> u64 {
    600
}
fn default_language() -> String {
    "ch".to_string()
}

impl Default for MinerUConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: MinerUMode::default(),
            model_version: default_model_version(),
            timeout_secs: default_timeout(),
            api_key: None,
            max_poll_time_secs: default_max_poll_time(),
            enable_table: true,
            is_ocr: true,
            enable_formula: true,
            language: default_language(),
            page_range: None,
        }
    }
}

/// A figure extracted by MinerU.
#[derive(Debug, Clone)]
pub struct MinerUFigure {
    pub caption: Option<String>,
    pub image_path: Option<String>,
    pub page_number: Option<u32>,
}

/// Result of a successful MinerU extraction.
#[derive(Debug, Clone)]
pub struct MinerUResult {
    /// Extracted content in Markdown format.
    pub markdown: String,
    /// Title detected by MinerU, if available.
    pub title: Option<String>,
    /// Authors detected by MinerU, if available.
    pub authors: Vec<String>,
    /// Affiliations detected by MinerU, if available.
    pub affiliations: Vec<String>,
    /// Emails detected by MinerU, if available.
    pub emails: Vec<String>,
    /// Keywords detected by MinerU, if available.
    pub keywords: Vec<String>,
    /// URLs detected by MinerU, if available.
    pub urls: Vec<String>,
    /// Abstract detected by MinerU, if available.
    pub abstract_text: Option<String>,
    /// Extra metadata from MinerU as a JSON string.
    pub extra: Option<String>,
    /// Figures extracted by MinerU.
    pub figures: Vec<MinerUFigure>,
}

/// HTTP client for MinerU API.
pub struct MinerUClient {
    client: reqwest::Client,
    config: MinerUConfig,
}

/// Shared signed-URL info returned by mode-specific submit handlers.
struct SignedUrlInfo {
    task_id: String,
    file_url: String,
    poll_url: String,
}

/// Result of a Precision API submit, allowing fallback on invalid token.
enum PrecisionSubmitResult {
    Urls(SignedUrlInfo),
    InvalidToken,
}

impl MinerUClient {
    /// Create a new MinerU client from configuration.
    pub fn new(config: MinerUConfig) -> Result<Self> {
        let client = crate::client::build_http_client(config.timeout_secs, None)?;

        Ok(Self { client, config })
    }

    /// Check if MinerU is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Extract text from PDF bytes via MinerU API.
    /// `paper_data_dir` is where extracted images and assets are persisted.
    pub async fn extract_pdf(
        &self,
        pdf_bytes: Vec<u8>,
        paper_data_dir: &Path,
        pdf_config: &PdfExtractionConfig,
    ) -> Result<MinerUResult> {
        if !self.config.enabled {
            return Err(PaperedError::config("MinerU is not enabled"));
        }

        self.run_mineru_extraction(self.config.mode, pdf_bytes, paper_data_dir, pdf_config)
            .await
    }

    // ------------------------------------------------------------------
    // Generic extraction pipeline
    // ------------------------------------------------------------------

    /// Core extraction pipeline shared by both modes.
    async fn run_mineru_extraction(
        &self,
        mode: MinerUMode,
        pdf_bytes: Vec<u8>,
        paper_data_dir: &Path,
        pdf_config: &PdfExtractionConfig,
    ) -> Result<MinerUResult> {
        let label = mode.label();
        tracing::info!("Extracting PDF via MinerU {label} API");
        let start = std::time::Instant::now();

        let base = mode.base_url();

        // 1. Request signed upload URL
        let (task_id, file_url, poll_url, token) = match mode {
            MinerUMode::Lightweight => {
                let info = self.get_lightweight_urls(base).await?;
                (info.task_id, info.file_url, info.poll_url, None)
            }
            MinerUMode::Precision => {
                let token = self.config.api_key.as_deref().ok_or_else(|| {
                    PaperedError::config("MinerU Precision API requires an api_key")
                })?;
                match self.get_precision_urls(base, token).await? {
                    PrecisionSubmitResult::Urls(info) => (
                        info.task_id,
                        info.file_url,
                        info.poll_url,
                        Some(token.to_string()),
                    ),
                    PrecisionSubmitResult::InvalidToken => {
                        tracing::warn!(
                            "MinerU Precision API token invalid (A0202), falling back to lightweight mode"
                        );
                        return Box::pin(self.run_mineru_extraction(
                            MinerUMode::Lightweight,
                            pdf_bytes,
                            paper_data_dir,
                            pdf_config,
                        ))
                        .await;
                    }
                }
            }
        };

        match mode {
            MinerUMode::Lightweight => {
                tracing::debug!("MinerU task_id={task_id}, uploading file...");
            }
            MinerUMode::Precision => {
                tracing::debug!("MinerU v4 batch_id={task_id}, uploading file...");
            }
        }

        // 2. Upload file to signed URL
        let upload_resp = self
            .client
            .put(&file_url)
            .body(pdf_bytes)
            .send()
            .await
            .map_err(PaperedError::Http)?;
        if !upload_resp.status().is_success() {
            return Err(PaperedError::pdf_parse(format!(
                "MinerU file upload failed: HTTP {}",
                upload_resp.status()
            )));
        }

        match mode {
            MinerUMode::Lightweight => {
                tracing::debug!("MinerU file uploaded, polling for result...");
            }
            MinerUMode::Precision => {
                tracing::debug!("MinerU v4 file uploaded, polling for result...");
            }
        }

        // 3. Poll for result with exponential backoff
        let max_poll_time = std::time::Duration::from_secs(self.config.max_poll_time_secs);
        let status_json = Self::poll_task_status(
            &self.client,
            &poll_url,
            token.as_deref(),
            std::time::Duration::from_secs(2),
            max_poll_time,
        )
        .await?;

        // 4. Download and extract result
        let (markdown, figures) = match mode {
            MinerUMode::Lightweight => {
                let markdown_url = status_json
                    .get("data")
                    .and_then(|d| d.get("markdown_url"))
                    .and_then(|u| u.as_str())
                    .ok_or_else(|| {
                        PaperedError::pdf_parse("MinerU done response missing markdown_url")
                    })?;

                let md_resp = self
                    .client
                    .get(markdown_url)
                    .send()
                    .await
                    .map_err(PaperedError::Http)?;
                let markdown = md_resp.text().await.map_err(PaperedError::Http)?;

                let base_url = markdown_url.rfind('/').map(|i| &markdown_url[..i + 1]);
                self.persist_images_from_markdown(&markdown, base_url, paper_data_dir, pdf_config)
                    .await?
            }
            MinerUMode::Precision => {
                let first_result = status_json
                    .get("data")
                    .and_then(|d| d.get("extract_result"))
                    .and_then(|a| a.as_array())
                    .and_then(|a| a.first());

                let zip_url = first_result
                    .and_then(|r| r.get("full_zip_url"))
                    .and_then(|u| u.as_str())
                    .ok_or_else(|| {
                        PaperedError::pdf_parse("MinerU v4 done response missing full_zip_url")
                    })?;

                let zip_resp = self
                    .client
                    .get(zip_url)
                    .send()
                    .await
                    .map_err(PaperedError::Http)?;
                let zip_bytes = zip_resp.bytes().await.map_err(PaperedError::Http)?;
                // ZIP decompression and per-figure image optimization are
                // CPU-heavy; keep them off the async runtime threads.
                let paper_data_dir = paper_data_dir.to_path_buf();
                let pdf_config = pdf_config.clone();
                tokio::task::spawn_blocking(move || {
                    Self::extract_all_from_zip(&zip_bytes, &paper_data_dir, &pdf_config)
                })
                .await
                .map_err(|e| {
                    PaperedError::Indexing(format!(
                        "spawn_blocking panicked for MinerU ZIP extraction: {e}"
                    ))
                })??
            }
        };

        match mode {
            MinerUMode::Lightweight => {
                tracing::info!(
                    "MinerU Agent extraction completed in {:?}, output: {} chars",
                    start.elapsed(),
                    markdown.len()
                );
            }
            MinerUMode::Precision => {
                tracing::info!(
                    "MinerU Precision extraction completed in {:?}, output: {} chars, {} figures",
                    start.elapsed(),
                    markdown.len(),
                    figures.len()
                );
            }
        }

        // 5. Build result
        Ok(Self::build_result(markdown, figures))
    }

    /// Build a `MinerUResult` from markdown text and figures, applying fallback logic.
    fn build_result(markdown: String, figures: Vec<MinerUFigure>) -> MinerUResult {
        let mut result = Self::parse_response(&markdown).unwrap_or_else(|_| MinerUResult {
            markdown: markdown.trim().to_string(),
            title: None,
            authors: Vec::new(),
            affiliations: Vec::new(),
            emails: Vec::new(),
            keywords: Vec::new(),
            urls: Vec::new(),
            abstract_text: None,
            extra: None,
            figures: figures.clone(),
        });

        if result.figures.is_empty() && !figures.is_empty() {
            result.figures = figures;
        }

        if result.markdown != markdown.trim() && result.title.is_none() && result.authors.is_empty()
        {
            result.markdown = markdown.trim().to_string();
        }

        result
    }

    // ------------------------------------------------------------------
    // Mode-specific submit handlers
    // ------------------------------------------------------------------

    async fn get_lightweight_urls(&self, base: &str) -> Result<SignedUrlInfo> {
        #[derive(Serialize)]
        struct SubmitRequest {
            file_name: String,
            enable_table: bool,
            is_ocr: bool,
            enable_formula: bool,
            language: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            page_range: Option<String>,
        }

        let submit_url = format!("{base}/parse/file");
        let submit_body = SubmitRequest {
            file_name: "document.pdf".to_string(),
            enable_table: self.config.enable_table,
            is_ocr: self.config.is_ocr,
            enable_formula: self.config.enable_formula,
            language: self.config.language.clone(),
            page_range: self.config.page_range.clone(),
        };

        let submit_resp = self
            .client
            .post(&submit_url)
            .json(&submit_body)
            .send()
            .await
            .map_err(PaperedError::Http)?;

        let submit_status = submit_resp.status();
        let submit_json: serde_json::Value =
            submit_resp.json().await.map_err(PaperedError::Http)?;

        if submit_status.is_success()
            && submit_json.get("code").and_then(serde_json::Value::as_i64) != Some(0)
        {
            let msg = submit_json
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(PaperedError::pdf_parse(format!(
                "MinerU submit error: {msg}"
            )));
        }

        if !submit_status.is_success() {
            return Err(PaperedError::pdf_parse(format!(
                "MinerU submit HTTP error {submit_status}: {submit_json}"
            )));
        }

        let data = submit_json
            .get("data")
            .ok_or_else(|| PaperedError::pdf_parse("MinerU submit response missing data"))?;

        let task_id = data
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PaperedError::pdf_parse("MinerU submit response missing task_id"))?;

        let file_url = data
            .get("file_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PaperedError::pdf_parse("MinerU submit response missing file_url"))?;

        Ok(SignedUrlInfo {
            task_id: task_id.to_string(),
            file_url: file_url.to_string(),
            poll_url: format!("{base}/parse/{task_id}"),
        })
    }

    async fn get_precision_urls(&self, base: &str, token: &str) -> Result<PrecisionSubmitResult> {
        #[derive(Serialize)]
        struct FileInfo {
            name: String,
            data_id: String,
            is_ocr: bool,
        }

        #[derive(Serialize)]
        struct BatchUrlRequest {
            files: Vec<FileInfo>,
            model_version: String,
            enable_table: bool,
            enable_formula: bool,
            language: String,
        }

        let url_req = BatchUrlRequest {
            files: vec![FileInfo {
                name: "document.pdf".to_string(),
                data_id: "papered".to_string(),
                is_ocr: self.config.is_ocr,
            }],
            model_version: self.config.model_version.clone(),
            enable_table: self.config.enable_table,
            enable_formula: self.config.enable_formula,
            language: self.config.language.clone(),
        };

        let url_resp = self
            .client
            .post(format!("{base}/file-urls/batch"))
            .bearer_auth(token)
            .json(&url_req)
            .send()
            .await
            .map_err(PaperedError::Http)?;

        let url_json: serde_json::Value = url_resp.json().await.map_err(PaperedError::Http)?;

        if url_json.get("code").and_then(serde_json::Value::as_i64) != Some(0) {
            let msg = url_json
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            let msg_code = url_json
                .get("msgCode")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if msg_code == "A0202" {
                return Ok(PrecisionSubmitResult::InvalidToken);
            }
            return Err(PaperedError::pdf_parse(format!(
                "MinerU v4 batch URL error: {msg}"
            )));
        }

        let batch_id = url_json
            .get("data")
            .and_then(|d| d.get("batch_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| PaperedError::pdf_parse("MinerU v4 response missing batch_id"))?;

        let file_url = url_json
            .get("data")
            .and_then(|d| d.get("file_urls"))
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .ok_or_else(|| PaperedError::pdf_parse("MinerU v4 response missing file_urls"))?;

        Ok(PrecisionSubmitResult::Urls(SignedUrlInfo {
            task_id: batch_id.to_string(),
            file_url: file_url.to_string(),
            poll_url: format!("{base}/extract-results/batch/{batch_id}"),
        }))
    }

    /// Poll a MinerU task until it completes or fails, with exponential backoff.
    async fn poll_task_status(
        client: &reqwest::Client,
        status_url: &str,
        token: Option<&str>,
        poll_interval: std::time::Duration,
        max_poll_time: std::time::Duration,
    ) -> Result<serde_json::Value> {
        let start = std::time::Instant::now();
        let mut interval = poll_interval;
        loop {
            tokio::time::sleep(interval).await;
            if start.elapsed() > max_poll_time {
                return Err(PaperedError::pdf_parse("MinerU polling timed out"));
            }
            let mut req = client.get(status_url);
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            let resp = req.send().await.map_err(PaperedError::Http)?;
            let status_json: serde_json::Value = resp.json().await.map_err(PaperedError::Http)?;

            // Lightweight path
            if let Some(state) = status_json["data"]["state"].as_str() {
                match state {
                    "done" => return Ok(status_json),
                    "failed" => {
                        let msg = status_json["data"]["err_msg"]
                            .as_str()
                            .unwrap_or("unknown error");
                        return Err(PaperedError::pdf_parse(format!(
                            "MinerU extraction failed: {msg}"
                        )));
                    }
                    _ => {}
                }
            }

            // Precision path
            if let Some(state) = status_json["data"]["extract_result"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|r| r["state"].as_str())
            {
                match state {
                    "done" => return Ok(status_json),
                    "failed" => {
                        let msg = status_json["data"]["extract_result"]
                            .as_array()
                            .and_then(|a| a.first())
                            .and_then(|r| r["err_msg"].as_str())
                            .unwrap_or("unknown error");
                        return Err(PaperedError::pdf_parse(format!(
                            "MinerU extraction failed: {msg}"
                        )));
                    }
                    _ => {}
                }
            }

            interval = (interval * 2).min(std::time::Duration::from_secs(30));
        }
    }

    // ------------------------------------------------------------------
    // ZIP extraction (Precision mode only)
    // ------------------------------------------------------------------

    /// Extract all contents from a MinerU v4 ZIP archive to internal storage.
    /// Returns the markdown text and a list of figure metadata with local paths.
    fn extract_all_from_zip(
        zip_bytes: &[u8],
        paper_data_dir: &Path,
        pdf_config: &PdfExtractionConfig,
    ) -> Result<(String, Vec<MinerUFigure>)> {
        use std::io::Cursor;
        use zip::read::ZipArchive;

        // Extract to a temp directory, then copy only images to paper_data_dir.
        // This prevents MinerU intermediate files (model.json, layout.json,
        // _origin.pdf, full.md) from contaminating the paper data directory.
        let temp_dir = tempfile::TempDir::new().map_err(|e| {
            PaperedError::pdf_parse_with_source(format!("Failed to create temp dir: {e}"), e)
        })?;
        let temp_path = temp_dir.path().to_path_buf();
        let cursor = Cursor::new(zip_bytes);
        let mut archive = ZipArchive::new(cursor).map_err(|e| {
            PaperedError::pdf_parse_with_source(
                format!("Failed to open MinerU ZIP archive: {e}"),
                e,
            )
        })?;

        // Extract all files to paper_data_dir, preserving structure
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| {
                PaperedError::pdf_parse_with_source(format!("Failed to read ZIP entry: {e}"), e)
            })?;

            // Use enclosed_name() to prevent ZIP path traversal attacks.
            let safe_name = match file.enclosed_name() {
                Some(p) => p.to_string_lossy().into_owned(),
                None => {
                    tracing::warn!("Skipping ZIP entry with unsafe path: {}", file.name());
                    continue;
                }
            };

            // Skip directories and hidden files
            if safe_name.ends_with('/') || safe_name.starts_with("__MACOSX") {
                continue;
            }

            let dest_path = temp_path.join(&safe_name);

            // Create parent directories first (ZIP entries may arrive in any order).
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    PaperedError::pdf_parse_with_source(
                        format!("Failed to create directory {}: {}", parent.display(), e),
                        e,
                    )
                })?;
            }

            // Defensive: ensure the resolved path is still within temp_dir.
            let Some(canonical_dest) = crate::util::resolve_within(&temp_path, &dest_path) else {
                tracing::warn!(
                    "Blocking ZIP path traversal attempt: {} resolves outside temp dir",
                    safe_name
                );
                continue;
            };

            let mut out = std::fs::File::create(&canonical_dest).map_err(|e| {
                PaperedError::pdf_parse_with_source(
                    format!("Failed to create file {}: {}", canonical_dest.display(), e),
                    e,
                )
            })?;
            std::io::copy(&mut file, &mut out).map_err(|e| {
                PaperedError::pdf_parse_with_source(
                    format!("Failed to write file {}: {}", canonical_dest.display(), e),
                    e,
                )
            })?;
        }

        // Find and read the markdown file from temp directory
        let mut markdown = String::new();
        let md_path = temp_path.join("auto").join("paper.md");
        if md_path.exists() {
            markdown = std::fs::read_to_string(&md_path).map_err(|e| {
                PaperedError::pdf_parse_with_source(format!("Failed to read markdown: {e}"), e)
            })?;
        } else {
            // Search for any .md file
            for entry in walkdir::WalkDir::new(&temp_path)
                .max_depth(2)
                .into_iter()
                .filter_map(std::result::Result::ok)
            {
                let name = entry.file_name().to_string_lossy();
                if name.ends_with(".md") || name.ends_with(".markdown") {
                    markdown = std::fs::read_to_string(entry.path()).map_err(|e| {
                        PaperedError::pdf_parse_with_source(
                            format!("Failed to read markdown: {e}"),
                            e,
                        )
                    })?;
                    break;
                }
            }
        }

        if markdown.is_empty() {
            return Err(PaperedError::pdf_parse(
                "MinerU ZIP archive contains no Markdown file",
            ));
        }

        // Parse figures from extracted markdown (paths relative to temp dir)
        let figures = Self::parse_figures_from_markdown_local(&temp_path, &markdown);

        if !pdf_config.extract_images {
            tracing::debug!("extract_images is disabled; skipping image extraction");
            return Ok((markdown.trim().to_string(), Vec::new()));
        }

        // Copy image files from temp dir to paper_data_dir/images/
        let img_dest = paper_data_dir.join("images");
        let out_format = crate::util::image::parse_image_format(&pdf_config.output_format);
        let mut updated_figures = Vec::new();
        for fig in figures {
            if let Some(ref rel) = fig.image_path {
                let src = temp_path.join(rel);
                if src.exists() {
                    std::fs::create_dir_all(&img_dest).map_err(|e| {
                        PaperedError::pdf_parse_with_source(
                            format!("Failed to create images dir: {e}"),
                            e,
                        )
                    })?;
                    // Flatten path: use just the filename to avoid nested subdirs
                    let stem = src
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let tmp_path = img_dest.join(format!("{stem}.tmp"));
                    let image_path = crate::util::image::place_optimized_image(
                        crate::util::image::optimize_image(
                            &src,
                            &tmp_path,
                            pdf_config.output_max_long_side,
                            pdf_config.output_quality,
                            out_format,
                        ),
                        &src,
                        &img_dest,
                        "images",
                        &stem,
                        out_format,
                        true,
                    )?
                    .1;
                    updated_figures.push(MinerUFigure {
                        caption: fig.caption,
                        image_path: Some(image_path),
                        page_number: fig.page_number,
                    });
                }
            } else {
                // No image path, keep as-is
                updated_figures.push(fig);
            }
        }

        // temp_dir is dropped here, cleaning up all intermediate files
        Ok((markdown.trim().to_string(), updated_figures))
    }

    /// Parse figures from markdown where images have local relative paths.
    fn parse_figures_from_markdown_local(
        paper_data_dir: &Path,
        markdown: &str,
    ) -> Vec<MinerUFigure> {
        let mut figures = Vec::new();
        let re = &*crate::util::MARKDOWN_IMAGE_RE;

        for cap in re.captures_iter(markdown) {
            let caption = cap.get(1).map(|m| m.as_str().to_string());
            let rel_path = cap.get(2).map(|m| m.as_str().to_string());

            // Verify the image file exists locally
            let exists = rel_path
                .as_ref()
                .is_some_and(|p| paper_data_dir.join(p).exists());

            if exists {
                figures.push(MinerUFigure {
                    caption,
                    image_path: rel_path,
                    page_number: None,
                });
            }
        }

        figures
    }

    /// Download/copy images referenced in markdown to internal storage.
    /// Returns updated markdown with local relative paths and figure metadata.
    async fn persist_images_from_markdown(
        &self,
        markdown: &str,
        base_url: Option<&str>,
        paper_data_dir: &Path,
        pdf_config: &PdfExtractionConfig,
    ) -> Result<(String, Vec<MinerUFigure>)> {
        let figures_dir = paper_data_dir.join("figures");
        std::fs::create_dir_all(&figures_dir).map_err(|e| {
            PaperedError::pdf_parse_with_source(
                format!(
                    "Failed to create figures dir {}: {}",
                    figures_dir.display(),
                    e
                ),
                e,
            )
        })?;

        if !pdf_config.extract_images {
            tracing::debug!("extract_images is disabled; skipping markdown image persistence");
            return Ok((markdown.to_string(), Vec::new()));
        }

        let mut updated_markdown = markdown.to_string();
        let mut figures = Vec::new();
        let out_format = crate::util::image::parse_image_format(&pdf_config.output_format);

        let re = &*crate::util::MARKDOWN_IMAGE_RE;

        for (i, cap) in re.captures_iter(markdown).enumerate() {
            let alt_text = cap.get(1).map_or("", |m| m.as_str());
            let img_ref = cap.get(2).map_or("", |m| m.as_str());

            if img_ref.is_empty() {
                continue;
            }

            let fig_stem = format!("fig{}", i + 1);
            let tmp_path = figures_dir.join(format!("{fig_stem}.tmp"));

            // Resolve source URL/path
            let source = if img_ref.starts_with("http://")
                || img_ref.starts_with("https://")
                || img_ref.starts_with('/')
            {
                img_ref.to_string()
            } else if let Some(base) = base_url {
                if base.ends_with('/') {
                    format!("{base}{img_ref}")
                } else {
                    format!("{base}/{img_ref}")
                }
            } else {
                img_ref.to_string()
            };

            // Download or copy to a temporary staging path.
            let stage_ext = Self::guess_extension(img_ref);
            let stage_path = figures_dir.join(format!("{fig_stem}_stage.{stage_ext}"));
            if source.starts_with("http://") || source.starts_with("https://") {
                match self.client.get(&source).send().await {
                    Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                        Ok(bytes) => {
                            if let Err(e) = std::fs::write(&stage_path, &bytes) {
                                tracing::warn!(
                                    "Failed to write image {}: {}",
                                    stage_path.display(),
                                    e
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to read image bytes from {}: {}", source, e)
                        }
                    },
                    Ok(resp) => tracing::warn!(
                        "Failed to download image {}: HTTP {}",
                        source,
                        resp.status()
                    ),
                    Err(e) => {
                        tracing::warn!("Failed to download image {}: {}", source, e)
                    }
                }
            } else {
                let src_path = Path::new(&source);
                if src_path.exists()
                    && let Err(e) = std::fs::copy(src_path, &stage_path)
                {
                    tracing::warn!(
                        "Failed to copy image {} to {}: {}",
                        source,
                        stage_path.display(),
                        e
                    );
                }
            }

            // Optimize the staged image to the configured format/size.
            let (final_filename, local_path) = if stage_path.exists() {
                let src = stage_path.clone();
                let dst = tmp_path.clone();
                let max_side = pdf_config.output_max_long_side;
                let quality = pdf_config.output_quality;
                let format = out_format;
                let optimize_result = tokio::task::spawn_blocking(move || {
                    crate::util::image::optimize_image(&src, &dst, max_side, quality, format)
                })
                .await
                .map_err(|e| {
                    PaperedError::Indexing(format!("Image optimization task failed: {e}"))
                })?;
                let placed = crate::util::image::place_optimized_image(
                    optimize_result,
                    &stage_path,
                    &figures_dir,
                    "figures",
                    &fig_stem,
                    out_format,
                    false,
                )?;
                // Remove the staging file only after placement, so the fallback
                // copy above can still read it when optimization failed.
                let _ = std::fs::remove_file(&stage_path);
                placed
            } else {
                let ext = out_format.default_extension();
                let fname = format!("{fig_stem}.{ext}");
                (fname.clone(), format!("figures/{fname}"))
            };

            // Replace in markdown
            let old_ref = format!("![{alt_text}]({img_ref})");
            let new_ref = format!("![{alt_text}]({local_path})");
            updated_markdown = updated_markdown.replace(&old_ref, &new_ref);

            let dest = figures_dir.join(&final_filename);
            if dest.exists() {
                figures.push(MinerUFigure {
                    caption: Some(alt_text.to_string()),
                    image_path: Some(local_path),
                    page_number: None,
                });
            }
        }

        Ok((updated_markdown, figures))
    }

    fn guess_extension(path: &str) -> &'static str {
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|e| match e.to_lowercase().as_str() {
                "jpg" | "jpeg" => Some("jpg"),
                "png" => Some("png"),
                "gif" => Some("gif"),
                "svg" => Some("svg"),
                "webp" => Some("webp"),
                _ => None,
            })
            .unwrap_or("png")
    }

    // ------------------------------------------------------------------
    // Response parsing (local mode only)
    // ------------------------------------------------------------------

    /// Parse MinerU API response, handling multiple common JSON schemas.
    fn parse_response(body: &str) -> Result<MinerUResult> {
        // Try JSON first
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
            return Self::parse_json_response(json, body);
        }

        // Plain text / markdown fallback
        Ok(MinerUResult {
            markdown: body.trim().to_string(),
            title: None,
            authors: Vec::new(),
            affiliations: Vec::new(),
            emails: Vec::new(),
            keywords: Vec::new(),
            urls: Vec::new(),
            abstract_text: None,
            extra: None,
            figures: Vec::new(),
        })
    }

    fn parse_json_response(json: serde_json::Value, fallback_body: &str) -> Result<MinerUResult> {
        // Common response formats from MinerU and community wrappers:
        // 1. { "markdown": "..." }
        // 2. { "data": { "markdown": "...", "title": "...", "authors": [...], "affiliations": [...] } }
        // 3. { "result": { "markdown": "..." } }
        // 4. { "text": "..." }

        let data = json.get("data").or_else(|| json.get("result"));

        let markdown = json
            .get("markdown")
            .or_else(|| data.and_then(|d| d.get("markdown")))
            .or_else(|| json.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or(fallback_body)
            .trim()
            .to_string();

        if markdown.is_empty() {
            return Err(PaperedError::pdf_parse("MinerU returned empty markdown"));
        }

        let title = json
            .get("title")
            .or_else(|| data.and_then(|d| d.get("title")))
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);

        /// Extract a string array from JSON, checking both the root and `data` nested object.
        fn extract_string_array(
            json: &serde_json::Value,
            data: Option<&serde_json::Value>,
            key: &str,
        ) -> Vec<String> {
            json.get(key)
                .or_else(|| data.and_then(|d| d.get(key)))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                        .collect()
                })
                .unwrap_or_default()
        }

        let authors = extract_string_array(&json, data, "authors");
        let affiliations = extract_string_array(&json, data, "affiliations");

        let abstract_text = json
            .get("abstract")
            .or_else(|| data.and_then(|d| d.get("abstract")))
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);

        let emails = extract_string_array(&json, data, "emails");
        let keywords = extract_string_array(&json, data, "keywords");
        let urls = extract_string_array(&json, data, "urls");

        // Parse extra metadata from JSON if available — enforce object type
        let extra = json
            .get("extra")
            .or_else(|| data.and_then(|d| d.get("extra")))
            .and_then(|v| {
                if v.is_null() {
                    None
                } else if let Some(obj) = v.as_object() {
                    if obj.is_empty() {
                        None
                    } else {
                        serde_json::to_string(obj).ok()
                    }
                } else {
                    tracing::warn!("MinerU returned extra as non-object: {:?}. Ignoring.", v);
                    None
                }
            });

        // Parse figures from JSON if available
        let figures = json
            .get("figures")
            .or_else(|| data.and_then(|d| d.get("figures")))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|fig| MinerUFigure {
                        caption: fig
                            .get("caption")
                            .and_then(|v| v.as_str())
                            .map(std::string::ToString::to_string),
                        image_path: fig
                            .get("image_path")
                            .or_else(|| fig.get("path"))
                            .and_then(|v| v.as_str())
                            .map(std::string::ToString::to_string),
                        page_number: fig
                            .get("page_number")
                            .and_then(serde_json::Value::as_u64)
                            .map(|p| p as u32),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(MinerUResult {
            markdown,
            title,
            authors,
            affiliations,
            emails,
            keywords,
            urls,
            abstract_text,
            extra,
            figures,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::str_enum::StrLabel;

    #[test]
    fn mode_serializes_to_lowercase() {
        assert_eq!(
            serde_json::to_string(&MinerUMode::Precision).unwrap(),
            "\"precision\""
        );
        assert_eq!(
            serde_json::to_string(&MinerUMode::Lightweight).unwrap(),
            "\"lightweight\""
        );
    }

    #[test]
    fn mode_round_trips() {
        for mode in [MinerUMode::Precision, MinerUMode::Lightweight] {
            let json = serde_json::to_string(&mode).unwrap();
            let parsed: MinerUMode = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, mode);
            assert_eq!(mode.as_str().parse::<MinerUMode>().unwrap(), mode);
        }
    }

    #[test]
    fn mode_from_str_is_case_insensitive() {
        assert_eq!(
            "Lightweight".parse::<MinerUMode>().unwrap(),
            MinerUMode::Lightweight
        );
        assert!("unknown".parse::<MinerUMode>().is_err());
    }

    #[test]
    fn mode_default_is_precision() {
        assert_eq!(MinerUMode::default(), MinerUMode::Precision);
    }
}
