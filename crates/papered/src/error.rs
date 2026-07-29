use http::StatusCode;
use serde::Serialize;
use thiserror::Error;

// ------------------------------------------------------------------
// Error code constants
// ------------------------------------------------------------------

pub const ERR_NOT_FOUND: &str = "not_found";
pub const ERR_INVALID_ARGUMENT: &str = "invalid_argument";
pub const ERR_CONFIG: &str = "config_error";
pub const ERR_DUPLICATE: &str = "duplicate";
pub const ERR_DATABASE: &str = "database_error";
pub const ERR_IO: &str = "io_error";
pub const ERR_EMBEDDING_API: &str = "embedding_api_error";
pub const ERR_PDF_PARSE: &str = "pdf_parse_error";
pub const ERR_CANCELLED: &str = "cancelled";
pub const ERR_INTERNAL: &str = "internal_error";
pub const ERR_CONFLICT: &str = "conflict";
pub const ERR_PAYLOAD_TOO_LARGE: &str = "file_too_large";

// ------------------------------------------------------------------
// Ergonomic constructor macro
// ------------------------------------------------------------------

macro_rules! error_with_source {
    ($($name:ident),* $(,)?) => {
        $(
            paste::paste! {
                pub fn [<$name:snake>](msg: impl Into<String>) -> Self {
                    Self::$name(msg.into(), None)
                }
                pub fn [<$name:snake _with_source>](
                    msg: impl Into<String>,
                    source: impl std::error::Error + Send + Sync + 'static,
                ) -> Self {
                    Self::$name(msg.into(), Some(Box::new(source)))
                }
            }
        )*
    };
}

// ------------------------------------------------------------------
// PaperedError
// ------------------------------------------------------------------

#[derive(Error, Debug)]
pub enum PaperedError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    Database(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("PDF parse error: {0}")]
    PdfParse(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),

    #[error("Embedding API error: {status} - {message}")]
    EmbeddingApi { status: u16, message: String },

    #[error("Section extraction error: {0}")]
    SectionExtraction(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),

    #[error("Configuration error: {0}")]
    Config(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),

    #[error("Not found: {0}")]
    NotFound(
        String,
        #[source] Option<Box<dyn std::error::Error + Send + Sync>>,
    ),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("Indexing error: {0}")]
    Indexing(String),

    #[error("JSON repair error: {0}")]
    JsonRepair(String),

    #[error("Duplicate file: already indexed as '{title}' (id: {id})")]
    Duplicate { title: String, id: String },

    #[error("Search error: {0}")]
    Search(String),

    #[error("Reranker error: {0}")]
    Reranker(String),

    #[error("LLM generation error: {0}")]
    LlmGeneration(String),

    #[error("Unknown error: {0}")]
    Unknown(String),

    #[error("Cancelled: {0}")]
    Cancelled(String),
}

impl PaperedError {
    error_with_source!(PdfParse, SectionExtraction, Config, NotFound);

    pub fn invalid_argument(msg: impl Into<String>) -> Self {
        Self::InvalidArgument(msg.into())
    }

    pub fn database(msg: impl Into<String>) -> Self {
        Self::Database(msg.into())
    }

    pub fn indexing(msg: impl Into<String>) -> Self {
        Self::Indexing(msg.into())
    }

    pub fn search(msg: impl Into<String>) -> Self {
        Self::Search(msg.into())
    }

    pub fn llm_generation(msg: impl Into<String>) -> Self {
        Self::LlmGeneration(msg.into())
    }

    pub fn json_repair(msg: impl Into<String>) -> Self {
        Self::JsonRepair(msg.into())
    }

    pub fn reranker(msg: impl Into<String>) -> Self {
        Self::Reranker(msg.into())
    }

    pub fn unknown(msg: impl Into<String>) -> Self {
        Self::Unknown(msg.into())
    }

    pub fn cancelled(msg: impl Into<String>) -> Self {
        Self::Cancelled(msg.into())
    }

    pub fn embedding_api(status: u16, message: impl Into<String>) -> Self {
        Self::EmbeddingApi {
            status,
            message: message.into(),
        }
    }

    /// Convenience constructor for `Io(std::io::Error::other(...))` —
    /// replaces 19 call sites of `map_err(|e| Io(io::Error::other(...)))`.
    pub fn io_other(msg: impl Into<String>) -> Self {
        Self::Io(std::io::Error::other(msg.into()))
    }

    /// The machine-readable error code for API responses.
    pub fn api_code(&self) -> &'static str {
        match self {
            Self::NotFound(_, _) => ERR_NOT_FOUND,
            Self::InvalidArgument(_) => ERR_INVALID_ARGUMENT,
            Self::Config(_, _) => ERR_CONFIG,
            Self::Duplicate { .. } => ERR_DUPLICATE,
            Self::Database(_) => ERR_DATABASE,
            Self::Io(_) => ERR_IO,
            Self::EmbeddingApi { .. } => ERR_EMBEDDING_API,
            Self::PdfParse(_, _) => ERR_PDF_PARSE,
            Self::Cancelled(_) => ERR_CANCELLED,
            _ => ERR_INTERNAL,
        }
    }

    /// The HTTP status code for API error responses.
    pub fn api_status(&self) -> StatusCode {
        match self {
            Self::NotFound(_, _) => StatusCode::NOT_FOUND,
            Self::InvalidArgument(_) | Self::Config(_, _) => StatusCode::BAD_REQUEST,
            Self::Duplicate { .. } | Self::Cancelled(_) => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Convert into the standard API error envelope: `(status, ApiError)`.
    pub fn into_api_error(self) -> (StatusCode, ApiError) {
        let status = self.api_status();
        let code = self.api_code();
        let message = self.to_string();
        (
            status,
            ApiError {
                code: code.to_string(),
                message,
                details: None,
            },
        )
    }
}

// ------------------------------------------------------------------
// ApiError
// ------------------------------------------------------------------

/// Standardised API error envelope suitable for JSON serialisation in HTTP
/// responses.  The `status` field is carried separately so that the daemon
/// layer can set the HTTP response code.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl From<PaperedError> for ApiError {
    fn from(e: PaperedError) -> Self {
        e.into_api_error().1
    }
}

// ------------------------------------------------------------------
// Conversions
// ------------------------------------------------------------------

impl From<turso::Error> for PaperedError {
    fn from(e: turso::Error) -> Self {
        PaperedError::Database(format!("turso: {e}"))
    }
}

// ------------------------------------------------------------------
// Type alias
// ------------------------------------------------------------------

pub type Result<T> = std::result::Result<T, PaperedError>;
