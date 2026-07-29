use papered::error::PaperedError;
use rmcp::ErrorData;

pub(crate) fn json_text(v: &impl serde::Serialize, label: &str) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| {
        tracing::error!("Failed to serialize MCP {label}: {e}");
        "{}".to_string()
    })
}

/// Map a `PaperedError` to the most appropriate MCP error code.
///
/// - `NotFound` / `InvalidArgument` → `invalid_params` (the caller supplied a
///   bad reference or malformed input — recoverable by fixing the request).
/// - Everything else → `internal_error` (server-side failure).
pub(crate) fn papered_error_to_mcp(e: PaperedError) -> ErrorData {
    match &e {
        PaperedError::NotFound(_, _) | PaperedError::InvalidArgument(_) => {
            ErrorData::invalid_params(e.to_string(), None)
        }
        _ => ErrorData::internal_error(e.to_string(), None),
    }
}

/// Extension trait to convert `papered::Result<T>` into `Result<T, ErrorData>`
/// for MCP tool handlers, eliminating repetitive `.map_err(...)` boilerplate.
pub(crate) trait McpResultExt<T> {
    fn mcp(self) -> Result<T, ErrorData>;
}

impl<T> McpResultExt<T> for papered::Result<T> {
    fn mcp(self) -> Result<T, ErrorData> {
        self.map_err(papered_error_to_mcp)
    }
}

impl<T> McpResultExt<T> for Result<T, String> {
    fn mcp(self) -> Result<T, ErrorData> {
        self.map_err(|e| ErrorData::internal_error(e, None))
    }
}
