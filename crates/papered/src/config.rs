use crate::error::{PaperedError, Result};
use crate::paper::mineru::MinerUConfig;
use crate::search::SearchMethod;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ==========================================================================
// Config types — persisted to config.toml
// ==========================================================================

/// API credential registry entry. Referenced by `ModelConfig.provider`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    pub api_base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// Model definition persisted in config. References a provider key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelConfig {
    /// Key into `AppConfig.providers`.
    pub provider: String,
    /// Model name sent to the API.
    pub model: String,
    #[serde(default)]
    pub concurrency: usize,
    #[serde(default)]
    pub rpm: usize,
    #[serde(default)]
    pub tpm: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
}

// ==========================================================================
// Runtime resolved type — constructed from ProviderConfig + ModelConfig
// ==========================================================================

/// Fully resolved model endpoint combining provider credentials and model config.
/// Constructed by `AppConfig::resolve_model()` at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEndpoint {
    pub api_base: String,
    pub api_key: Option<String>,
    pub model: String,
    pub concurrency: usize,
    pub rpm: usize,
    pub tpm: usize,
    pub extra_body: Option<serde_json::Value>,
    pub reasoning_effort: Option<String>,
    pub context_window: Option<usize>,
    pub max_output_tokens: Option<usize>,
}

impl ModelEndpoint {
    /// Placeholder endpoint targeting a discard port (127.0.0.1:9).
    /// Used when no real model is configured yet (e.g. fresh install).
    pub fn placeholder() -> Self {
        Self {
            api_base: "http://127.0.0.1:9".to_string(),
            api_key: None,
            model: String::new(),
            concurrency: 0,
            rpm: 0,
            tpm: 0,
            extra_body: None,
            reasoning_effort: None,
            context_window: None,
            max_output_tokens: None,
        }
    }
}

impl From<(&ProviderConfig, &ModelConfig)> for ModelEndpoint {
    fn from((pc, mc): (&ProviderConfig, &ModelConfig)) -> Self {
        Self {
            api_base: pc.api_base.clone(),
            api_key: pc.api_key.clone(),
            model: mc.model.clone(),
            concurrency: mc.concurrency,
            rpm: mc.rpm,
            tpm: mc.tpm,
            extra_body: mc.extra_body.clone(),
            reasoning_effort: mc.reasoning_effort.clone(),
            context_window: mc.context_window,
            max_output_tokens: mc.max_output_tokens,
        }
    }
}

impl ModelEndpoint {
    pub fn has_valid_api_key(&self) -> bool {
        self.api_key.as_ref().is_some_and(|k| !k.trim().is_empty())
    }
}

// ==========================================================================
// Purposes
// ==========================================================================

/// Purpose-to-model bindings.
///
/// Each field maps a feature purpose to a model key in `AppConfig.models`.
/// Only **one embedding model** is allowed — `embedding` is a single string
/// key, not a list. The embedding client auto-handles both text and
/// image/figure embedding (multimodal fallback is transparent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PurposesConfig {
    /// Single embedding model for text search and image/figure embedding.
    /// Must reference exactly one key in `AppConfig.models`.
    pub embedding: String,
    pub reranker: String,
    pub section: String,
    pub rag: String,
    /// Optional vision model for image semantic descriptions.
    /// When `None` or empty, image description is skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<String>,
    /// Unified query enhancement purpose (rewriting + HyDE).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhancement: Option<String>,
    /// Translation purpose (optional — defaults to the `rag` model when `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<String>,
}

// ==========================================================================
// Translation config
// ==========================================================================

/// Translation feature settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranslationConfig {
    /// Target language for translations (e.g. "zh-CN", "en", "ja").
    #[serde(default = "default_target_language")]
    pub target_language: String,
}

fn default_target_language() -> String {
    "zh-CN".to_string()
}

impl Default for TranslationConfig {
    fn default() -> Self {
        Self {
            target_language: default_target_language(),
        }
    }
}

// ==========================================================================
// Feature configs (no endpoint_id — bound via purposes)
// ==========================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddingConfig {
    #[serde(default = "default_embedding_max_batch_size")]
    pub max_batch_size: usize,
    /// HTTP timeout in seconds for embedding API calls (default 60).
    #[serde(default = "default_embedding_timeout_secs")]
    pub timeout_secs: u64,
    /// Truncate direction for long inputs: "right", "left", etc.
    /// Only meaningful for providers that support multimodal embedding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncate: Option<String>,
    /// Encoding format for the response: "float", "base64", etc.
    /// Only meaningful for providers that support multimodal embedding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<String>,
    /// Whether this model supports multimodal (image+text) embedding.
    /// When `false`, image embedding is skipped entirely — no upload is
    /// attempted. Set to `true` for Qwen-VL, CLIP, and similar models.
    #[serde(default)]
    pub supports_multimodal: bool,
}

const fn default_embedding_max_batch_size() -> usize {
    8
}

const fn default_embedding_timeout_secs() -> u64 {
    60
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            max_batch_size: default_embedding_max_batch_size(),
            timeout_secs: default_embedding_timeout_secs(),
            truncate: None,
            encoding_format: None,
            supports_multimodal: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SectionConfig {
    #[serde(default = "default_section_max_input_chars")]
    pub max_input_chars: usize,
    #[serde(default = "default_section_max_output_tokens")]
    pub max_output_tokens: usize,
}

/// Controls whether a separate, focused LLM call extracts figure metadata
/// (labels + captions) from the paper text. Enabled by default — disable to
/// save one LLM call per import and fall back to MinerU figures only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FigureExtractionConfig {
    #[serde(default = "default_figure_extraction_enabled")]
    pub enabled: bool,
}

impl Default for FigureExtractionConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

const fn default_figure_extraction_enabled() -> bool {
    true
}

const fn default_section_max_input_chars() -> usize {
    800000
}
const fn default_section_max_output_tokens() -> usize {
    131072
}

impl Default for SectionConfig {
    fn default() -> Self {
        Self {
            max_input_chars: default_section_max_input_chars(),
            max_output_tokens: default_section_max_output_tokens(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexingConfig {
    #[serde(default = "default_indexing_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_indexing_queue_size")]
    pub queue_size: usize,
}

const fn default_indexing_concurrency() -> usize {
    8
}
const fn default_indexing_queue_size() -> usize {
    2048
}

impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            concurrency: default_indexing_concurrency(),
            queue_size: default_indexing_queue_size(),
        }
    }
}

fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("papered")
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RagConfig {
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: usize,
    #[serde(default = "default_rag_top_k")]
    pub top_k: usize,
    #[serde(default)]
    pub search_method: SearchMethod,
    #[serde(default = "default_rag_system_prompt")]
    pub system_prompt: String,
    #[serde(default = "default_rag_context_chars")]
    pub max_context_chars: usize,
    #[serde(default = "default_rag_paper_chars")]
    pub max_paper_context_chars: usize,
    #[serde(default = "default_rag_paper_scoped_chars")]
    pub max_paper_scoped_chars: usize,
    #[serde(default = "default_rag_temperature")]
    pub temperature: f32,
    #[serde(default = "default_use_compact_context")]
    pub use_compact_context: bool,
    #[serde(default = "default_include_meta_fields")]
    pub include_meta_fields: Vec<String>,
    /// Unified query enhancement configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhancement: Option<crate::llm::query_enhancer::QueryEnhancerConfig>,
    #[serde(default = "default_true")]
    pub adaptive_enabled: bool,
}

const fn default_max_output_tokens() -> usize {
    131072
}
const fn default_rag_top_k() -> usize {
    12
}
fn default_rag_system_prompt() -> String {
    "You are a cognitive mirror reflecting the user's thought space. Answer based ONLY on the provided research papers.\n\nCitations: the context is organized into numbered sources (\"### Source 1:\", \"### Source 2:\", ...). Cite every non-trivial claim inline with the matching source number in square brackets, e.g. [1] or [1][3]. When the context shows a \"Section path:\" line, you may also name the section. If the context does not contain enough information, say so clearly instead of guessing. Be concise but thorough.".to_string()
}
const fn default_rag_context_chars() -> usize {
    24_000
}
const fn default_rag_paper_chars() -> usize {
    4_000
}
const fn default_rag_paper_scoped_chars() -> usize {
    8_000
}
const fn default_rag_temperature() -> f32 {
    0.2
}
const fn default_use_compact_context() -> bool {
    true
}
fn default_include_meta_fields() -> Vec<String> {
    crate::paper::default_include_meta_fields()
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            max_output_tokens: default_max_output_tokens(),
            top_k: default_rag_top_k(),
            search_method: SearchMethod::default(),
            system_prompt: default_rag_system_prompt(),
            max_context_chars: default_rag_context_chars(),
            max_paper_context_chars: default_rag_paper_chars(),
            max_paper_scoped_chars: default_rag_paper_scoped_chars(),
            temperature: default_rag_temperature(),
            use_compact_context: default_use_compact_context(),
            include_meta_fields: crate::paper::default_include_meta_fields(),
            enhancement: None,
            adaptive_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PdfExtractionConfig {
    #[serde(default = "default_artifact_threshold")]
    pub artifact_threshold: f32,
    #[serde(default = "default_min_image_short_side")]
    pub min_image_short_side: u32,
    #[serde(default = "default_max_image_long_side")]
    pub max_image_long_side: u32,
    #[serde(default = "default_min_image_file_size_bytes")]
    pub min_image_file_size_bytes: u64,
    #[serde(default = "default_max_image_file_size_bytes")]
    pub max_image_file_size_bytes: u64,
    #[serde(default = "default_output_max_long_side")]
    pub output_max_long_side: u32,
    #[serde(default = "default_output_quality")]
    pub output_quality: u8,
    #[serde(default = "default_output_format")]
    pub output_format: String,
    #[serde(default = "default_extract_images")]
    pub extract_images: bool,
    #[serde(default = "default_min_title_chars")]
    pub min_title_chars: usize,
    #[serde(default = "default_quality_min_chars")]
    pub quality_min_chars: usize,
    #[serde(default = "default_quality_min_words")]
    pub quality_min_words: usize,
    #[serde(default = "default_quality_min_lines")]
    pub quality_min_lines: usize,
    #[serde(default = "default_quality_min_alphanumeric_ratio")]
    pub quality_min_alphanumeric_ratio: f32,
    #[serde(default = "default_quality_warn_threshold")]
    pub quality_warn_threshold: u8,
    #[serde(default = "default_quality_reject_threshold")]
    pub quality_reject_threshold: u8,
    #[serde(default = "default_enable_layout_quality_signals")]
    pub enable_layout_quality_signals: bool,
    #[serde(default = "default_min_chars_per_page")]
    pub min_chars_per_page: usize,
    #[serde(default = "default_max_pages_warning")]
    pub max_pages_warning: usize,
    /// Repair PDF text-extraction artifacts (line-break hyphenation and
    /// intra-word spaces) for the pdf_oxide path only. A deterministic regex
    /// pre-pass runs free; a pre-filter then sends only suspicious paragraph
    /// chunks to the LLM, so token cost stays low. MinerU output is clean and
    /// never touched. Default true.
    #[serde(default = "default_fix_text_artifacts")]
    pub fix_text_artifacts: bool,
}

const fn default_artifact_threshold() -> f32 {
    0.85
}
const fn default_min_image_short_side() -> u32 {
    220
}
const fn default_max_image_long_side() -> u32 {
    9000
}
const fn default_min_image_file_size_bytes() -> u64 {
    16384
}
const fn default_max_image_file_size_bytes() -> u64 {
    20 * 1024 * 1024
}
const fn default_output_max_long_side() -> u32 {
    1600
}
const fn default_output_quality() -> u8 {
    85
}
fn default_output_format() -> String {
    "auto".to_string()
}
const fn default_extract_images() -> bool {
    true
}
const fn default_min_title_chars() -> usize {
    3
}
const fn default_quality_min_chars() -> usize {
    50
}
const fn default_quality_min_words() -> usize {
    10
}
const fn default_quality_min_lines() -> usize {
    2
}
const fn default_quality_min_alphanumeric_ratio() -> f32 {
    0.40
}
const fn default_quality_warn_threshold() -> u8 {
    70
}
const fn default_quality_reject_threshold() -> u8 {
    30
}
const fn default_enable_layout_quality_signals() -> bool {
    true
}
const fn default_fix_text_artifacts() -> bool {
    true
}
const fn default_min_chars_per_page() -> usize {
    20
}
const fn default_max_pages_warning() -> usize {
    5000
}

impl Default for PdfExtractionConfig {
    fn default() -> Self {
        Self {
            artifact_threshold: default_artifact_threshold(),
            min_image_short_side: default_min_image_short_side(),
            max_image_long_side: default_max_image_long_side(),
            min_image_file_size_bytes: default_min_image_file_size_bytes(),
            max_image_file_size_bytes: default_max_image_file_size_bytes(),
            output_max_long_side: default_output_max_long_side(),
            output_quality: default_output_quality(),
            output_format: default_output_format(),
            extract_images: default_extract_images(),
            min_title_chars: default_min_title_chars(),
            quality_min_chars: default_quality_min_chars(),
            quality_min_words: default_quality_min_words(),
            quality_min_lines: default_quality_min_lines(),
            quality_min_alphanumeric_ratio: default_quality_min_alphanumeric_ratio(),
            quality_warn_threshold: default_quality_warn_threshold(),
            quality_reject_threshold: default_quality_reject_threshold(),
            enable_layout_quality_signals: default_enable_layout_quality_signals(),
            min_chars_per_page: default_min_chars_per_page(),
            max_pages_warning: default_max_pages_warning(),
            fix_text_artifacts: default_fix_text_artifacts(),
        }
    }
}

// ==========================================================================
// AppConfig
// ==========================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub models: HashMap<String, ModelConfig>,
    #[serde(default)]
    pub purposes: PurposesConfig,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub section: SectionConfig,
    #[serde(default)]
    pub mineru: MinerUConfig,
    #[serde(default)]
    pub reranker: crate::llm::reranker::RerankerConfig,
    #[serde(default)]
    pub rag: RagConfig,
    #[serde(default)]
    pub lattice_sync: LatticeSyncConfig,
    #[serde(default)]
    pub zotero_sync: ZoteroSyncConfig,
    #[serde(default)]
    pub indexing: IndexingConfig,
    #[serde(default)]
    pub pdf_extraction: PdfExtractionConfig,
    #[serde(default)]
    pub figure_extraction: FigureExtractionConfig,
    #[serde(default)]
    pub translation: TranslationConfig,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

// ==========================================================================
// Base sync config (shared between lattice + zotero)
// ==========================================================================

/// Common configuration knobs shared by all library-sync providers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaseSyncConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sync_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_sync_batch")]
    pub batch_limit: u32,
    #[serde(default)]
    pub pdf_search_paths: Vec<std::path::PathBuf>,
}

impl Default for BaseSyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_sync_interval(),
            batch_limit: default_sync_batch(),
            pdf_search_paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LatticeSyncConfig {
    #[serde(flatten)]
    pub base: BaseSyncConfig,
    /// Sync only these named collections. Empty = sync all collections.
    /// Names are matched case-sensitively against `LatticeCollection::name`.
    #[serde(default)]
    pub collections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZoteroSyncConfig {
    #[serde(flatten)]
    pub base: BaseSyncConfig,
    #[serde(default)]
    pub collection_keys: Vec<String>,
    #[serde(default = "default_download_pdf")]
    pub download_pdf: bool,
    #[serde(default)]
    pub last_sync_version: u64,
    #[serde(default = "default_recursive_collections")]
    pub recursive_collections: bool,
}

const fn default_sync_interval() -> u64 {
    300
}
const fn default_sync_batch() -> u32 {
    50
}
const fn default_download_pdf() -> bool {
    true
}
const fn default_recursive_collections() -> bool {
    true
}

impl Default for ZoteroSyncConfig {
    fn default() -> Self {
        Self {
            base: BaseSyncConfig::default(),
            collection_keys: Vec::new(),
            download_pdf: true,
            last_sync_version: 0,
            recursive_collections: true,
        }
    }
}

// ==========================================================================
// AppConfig impls
// ==========================================================================

impl Default for AppConfig {
    fn default() -> Self {
        // A fresh install carries no providers, models, or purpose bindings:
        // the setup wizard (`papered ui`) collects them from the user. Only
        // the non-model feature configs keep their built-in defaults.
        Self {
            data_dir: default_data_dir(),
            providers: HashMap::new(),
            models: HashMap::new(),
            purposes: PurposesConfig::default(),
            embedding: EmbeddingConfig::default(),
            section: SectionConfig::default(),
            mineru: MinerUConfig::default(),
            reranker: crate::llm::reranker::RerankerConfig::default(),
            rag: RagConfig::default(),
            lattice_sync: LatticeSyncConfig::default(),
            zotero_sync: ZoteroSyncConfig::default(),
            indexing: IndexingConfig::default(),
            pdf_extraction: PdfExtractionConfig::default(),
            figure_extraction: FigureExtractionConfig::default(),
            translation: TranslationConfig::default(),
            log_level: default_log_level(),
        }
    }
}

// ==========================================================================
// Methods
// ==========================================================================

impl AppConfig {
    pub fn load() -> Result<Self> {
        let config_path = Self::find_config_path()?;
        if !config_path.exists() {
            Self::save_default()?;
        }
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| PaperedError::config_with_source("Failed to read config", e))?;
        let mut config: Self = toml::from_str(&content)
            .map_err(|e| PaperedError::config_with_source("Failed to parse config", e))?;
        config.ensure_dirs()?;
        Ok(config)
    }

    pub fn save_default() -> Result<()> {
        let config_path = Self::find_config_path()?;
        let template = toml::to_string_pretty(&Self::default()).map_err(|e| {
            PaperedError::config_with_source("Failed to serialize default config", e)
        })?;
        Self::write_config_file(&config_path, &template)
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::find_config_path()?;
        let content = toml::to_string_pretty(self)
            .map_err(|e| PaperedError::config_with_source("Failed to serialize config", e))?;
        Self::write_config_file(&config_path, &content)
    }

    /// Create the parent dir, write `content`, and restrict permissions to
    /// 0o600 (the file may contain API keys). Permission failures only warn.
    fn write_config_file(config_path: &std::path::Path, content: &str) -> Result<()> {
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| PaperedError::config_with_source("Failed to create config dir", e))?;
        }
        std::fs::write(config_path, content)
            .map_err(|e| PaperedError::config_with_source("Failed to write config", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600))
            {
                tracing::warn!("Failed to set config file permissions to 0o600: {}", e);
            }
        }
        Ok(())
    }

    pub fn config_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".config")
            })
            .join("papered");
        Ok(config_dir.join("config.toml"))
    }

    pub fn find_config_path() -> Result<PathBuf> {
        let primary = Self::config_path()?;
        if primary.exists() {
            return Ok(primary);
        }
        let fallback = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("config.toml")))
            .filter(|p| p.exists());
        Ok(fallback.unwrap_or(primary))
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("papered.db")
    }

    /// Resolve a model key → ProviderConfig + ModelConfig → ModelEndpoint (runtime).
    pub fn resolve_model(&self, model_key: &str) -> Result<ModelEndpoint> {
        let mc = self.models.get(model_key).ok_or_else(|| {
            PaperedError::config(format!("Model '{model_key}' not found in models registry"))
        })?;
        let pc = self.providers.get(&mc.provider).ok_or_else(|| {
            PaperedError::config(format!(
                "Provider '{}' (referenced by model '{model_key}') not found in providers registry",
                mc.provider
            ))
        })?;
        Ok(ModelEndpoint::from((pc, mc)))
    }

    /// Compute embedding model fingerprint: "{provider}/{model}".
    pub fn embedding_fingerprint(&self) -> Option<String> {
        let mc = self.models.get(&self.purposes.embedding)?;
        Some(format!("{}/{}", mc.provider, mc.model))
    }

    fn ensure_dirs(&mut self) -> Result<()> {
        // Users naturally write `~/...` in config.toml; expand it on load.
        // Without this a literal `~` directory is created relative to the
        // process working directory.
        self.data_dir = crate::util::paths::normalize_input_path(&self.data_dir.to_string_lossy());
        std::fs::create_dir_all(&self.data_dir)
            .map_err(|e| PaperedError::config_with_source("Failed to create data dir", e))?;
        Ok(())
    }

    /// Resolve the translation model endpoint.
    /// Falls back to the `rag` model when `purposes.translation` is not set.
    pub fn resolve_translation_model(&self) -> Result<ModelEndpoint> {
        let key = self
            .purposes
            .translation
            .as_deref()
            .filter(|k| !k.is_empty())
            .unwrap_or(&self.purposes.rag);
        self.resolve_model(key)
    }

    /// Validate that every non-empty purpose references a known model, and
    /// every model references a known provider. An empty purpose key means
    /// "not configured yet" (fresh install before the setup wizard runs) and
    /// is allowed; the daemon starts in degraded mode until it is filled in.
    /// Deterministic content hash used as an ETag for optimistic concurrency on
    /// the config REST endpoints.
    pub fn version_hash(&self) -> String {
        use std::hash::{Hash, Hasher};
        let canonical = serde_json::to_vec(self).unwrap_or_default();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        canonical.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    pub fn validate(&self) -> Result<()> {
        let purposes: [(&str, Option<&str>); 7] = [
            ("embedding", Some(&self.purposes.embedding)),
            ("reranker", Some(&self.purposes.reranker)),
            ("section", Some(&self.purposes.section)),
            ("rag", Some(&self.purposes.rag)),
            ("enhancement", self.purposes.enhancement.as_deref()),
            ("vision", self.purposes.vision.as_deref()),
            ("translation", self.purposes.translation.as_deref()),
        ];

        for (purpose, model_key) in purposes {
            if let Some(key) = model_key
                && !key.is_empty()
                && !self.models.contains_key(key)
            {
                return Err(PaperedError::config(format!(
                    "purpose '{purpose}' references model '{key}', which does not exist"
                )));
            }
        }

        for (model_key, mc) in &self.models {
            if !self.providers.contains_key(&mc.provider) {
                return Err(PaperedError::config(format!(
                    "model '{model_key}' references provider '{}', which does not exist in providers registry",
                    mc.provider
                )));
            }
        }

        Ok(())
    }

    /// Strict validation: runs [`validate`](Self::validate) plus numeric
    /// sanity checks that catch misconfigurations at load time rather than
    /// at first use.
    pub fn validate_strict(&self) -> Result<()> {
        self.validate()?;

        if self.embedding.max_batch_size == 0 {
            return Err(PaperedError::config("embedding.max_batch_size must be > 0"));
        }
        if self.indexing.concurrency == 0 {
            return Err(PaperedError::config("indexing.concurrency must be > 0"));
        }
        if self.lattice_sync.base.enabled && self.lattice_sync.base.interval_secs < 60 {
            return Err(PaperedError::config(
                "lattice_sync.interval_secs must be >= 60",
            ));
        }
        if self.zotero_sync.base.enabled && self.zotero_sync.base.interval_secs < 60 {
            return Err(PaperedError::config(
                "zotero_sync.interval_secs must be >= 60",
            ));
        }

        Ok(())
    }
}

/// Human-readable diagnostic for a purpose whose model hasn't been configured
/// yet. Kept neutral so it works for both the web UI and the native Swift app.
pub fn unconfigured_model_message(purpose: &str) -> String {
    format!("No {purpose} model configured yet. Open Settings and add a model for {purpose}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_hash_is_deterministic_and_content_sensitive() {
        let a = AppConfig::default();
        let b = AppConfig::default();
        assert_eq!(a.version_hash(), b.version_hash());

        let mut c = b;
        c.rag.top_k += 1;
        assert_ne!(a.version_hash(), c.version_hash());
    }

    #[test]
    fn default_config_is_self_consistent() {
        // Fresh installs start from `AppConfig::default()` (via `save_default`):
        // it carries no providers, models, or purpose bindings (the setup
        // wizard collects those), and the empty registry must still pass
        // validation so the daemon can start on first launch.
        let config = AppConfig::default();
        assert!(config.providers.is_empty());
        assert!(config.models.is_empty());
        assert!(config.purposes.embedding.is_empty());
        assert!(config.purposes.reranker.is_empty());
        assert!(config.purposes.section.is_empty());
        assert!(config.purposes.rag.is_empty());
        assert!(config.purposes.vision.is_none());
        assert!(config.purposes.enhancement.is_none());
        assert!(config.purposes.translation.is_none());
        assert_eq!(config.translation.target_language, "zh-CN");
        config.validate().expect("default config must validate");
    }

    #[test]
    fn non_empty_purpose_must_reference_existing_model() {
        let mut config = AppConfig::default();
        config.purposes.embedding = "missing-model".to_string();
        let err = config
            .validate()
            .expect_err("non-empty purpose referencing a missing model must fail");
        assert!(
            err.to_string()
                .contains("purpose 'embedding' references model 'missing-model'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn default_include_meta_fields_uses_published_date() {
        let config = AppConfig::default();
        assert_eq!(
            config.rag.include_meta_fields,
            vec!["title", "authors", "published_date", "venue"]
        );
    }

    #[test]
    fn resolve_model_combines_provider_and_model() {
        let mut config = AppConfig::default();
        config.providers.insert(
            "deepseek".to_string(),
            ProviderConfig {
                api_base: "https://api.deepseek.com".to_string(),
                api_key: None,
            },
        );
        config.models.insert(
            "deepseek-v4-flash".to_string(),
            ModelConfig {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-flash".to_string(),
                concurrency: 0,
                rpm: 0,
                tpm: 0,
                extra_body: None,
                reasoning_effort: None,
                context_window: None,
                max_output_tokens: None,
            },
        );
        let ep = config.resolve_model("deepseek-v4-flash").unwrap();
        assert_eq!(ep.api_base, "https://api.deepseek.com");
        assert_eq!(ep.model, "deepseek-v4-flash");
    }

    #[test]
    fn ensure_dirs_expands_tilde_in_data_dir() {
        let home = dirs::home_dir().unwrap();
        let mut config = AppConfig {
            data_dir: PathBuf::from("~/.papered-ensure-dirs-test"),
            ..AppConfig::default()
        };
        let expanded = home.join(".papered-ensure-dirs-test");
        let _ = std::fs::remove_dir_all(&expanded);
        config.ensure_dirs().unwrap();
        assert_eq!(config.data_dir, expanded);
        assert!(expanded.is_dir());
        std::fs::remove_dir_all(&expanded).unwrap();
    }

    #[test]
    fn example_configs_parse_and_validate() {
        for file in ["config.toml", "config-omlx.toml", "config-minimal.toml"] {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/");
            let content = std::fs::read_to_string(format!("{path}{file}"))
                .unwrap_or_else(|e| panic!("{file}: {e}"));
            let config: AppConfig =
                toml::from_str(&content).unwrap_or_else(|e| panic!("{file} should parse: {e}"));
            config
                .validate()
                .unwrap_or_else(|e| panic!("{file} should validate: {e}"));
        }
    }
}
