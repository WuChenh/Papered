//! Papered MCP server — the top-level struct and `ServerHandler` implementation.
//!
//! Provides factory functions to create a streamable HTTP service and a stdio
//! server, both backed by rmcp.

use papered::VectorStore;
use papered::llm::rag::RagEngine;
use papered::search::SearchEngine;
use rmcp::model::{
    GetPromptRequestParams, GetPromptResult, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, session::local::LocalSessionManager, tower::StreamableHttpService,
};
use rmcp::{ErrorData, serve_server};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::prompts;
use crate::resources;
use crate::util::McpResultExt;

/// The Papered MCP server — holds all shared state needed by tools, resources,
/// and prompts.
pub struct PaperedMcpServer {
    pub store: Arc<dyn VectorStore>,
    pub search_engine: Arc<RwLock<SearchEngine>>,
    pub rag_engine: Arc<RwLock<RagEngine>>,
}

// =========================================================================
// ServerHandler — resources and prompts override the defaults; tool dispatch
// is filled in by `#[tool_handler(router = Self::tool_router())]`.
// =========================================================================

#[rmcp::tool_handler(
    router = Self::tool_router(),
    name = "papered-mcp",
    instructions = "Papered is a local-first academic paper knowledge engine. \
Canonical research flow: (1) search_papers to discover relevant papers by metadata, \
(2) get_paper_details to read extracted sections and bio-entities, \
(3) get_paper_passages or get_passage to fetch the exact original text behind a citation. \
Use ask_rag for a single cited answer grounded in the library. \
All tools are read-only and return JSON."
)]
impl rmcp::ServerHandler for PaperedMcpServer {
    // ------------------------------------------------------------------
    // Resources
    // ------------------------------------------------------------------

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = resources::list_resources(self).await?;
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(
            resources::resource_templates(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        resources::read_resource(self, &request.uri).await
    }

    // ------------------------------------------------------------------
    // Prompts
    // ------------------------------------------------------------------

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let mut prompts = prompts::list_builtin_prompts();

        // Add user-defined prompts from the store
        let user_prompts = self.store.list_prompts().await.mcp()?;

        for p in user_prompts {
            prompts.push(rmcp::model::Prompt::new(
                p.name,
                p.description,
                None::<Vec<rmcp::model::PromptArgument>>,
            ));
        }

        Ok(ListPromptsResult::with_all_items(prompts))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        let args = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or_default();

        // Check built-in prompts first
        if let Some(builtin) = prompts::get_builtin_prompt(&request.name, &args) {
            return Ok(builtin);
        }

        // Fall back to user-defined prompts from the store
        let prompts = self.store.list_prompts().await.mcp()?;

        let prompt = prompts
            .into_iter()
            .find(|p| p.name == request.name)
            .ok_or_else(|| {
                ErrorData::invalid_params(format!("Prompt not found: {}", request.name), None)
            })?;

        Ok(
            GetPromptResult::new(vec![rmcp::model::PromptMessage::new_text(
                rmcp::model::Role::User,
                prompt.system_prompt,
            )])
            .with_description(prompt.description.unwrap_or_else(|| prompt.name.clone())),
        )
    }

    // ------------------------------------------------------------------
    // Server info — override to include resources + prompts capabilities
    // (the `#[tool_handler]` attribute already enables tools capability).
    // ------------------------------------------------------------------

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "papered-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "Papered is a local-first academic paper knowledge engine. \
             Canonical research flow: (1) search_papers to discover relevant papers by metadata, \
             (2) get_paper_details to read extracted sections and bio-entities, \
             (3) get_paper_passages or get_passage to fetch the exact original text behind a citation. \
             Use ask_rag for a single cited answer grounded in the library. \
             All tools are read-only and return JSON.",
        )
    }
}

// =========================================================================
// Factory functions
// =========================================================================

/// Build a `StreamableHttpService` that can be nested into an Axum router.
///
/// # Example
///
/// ```ignore
/// let mcp_svc = build_mcp_service(store, search_engine, rag_engine);
/// let app = axum::Router::new().nest_service("/mcp", mcp_svc);
/// ```
pub fn build_mcp_service(
    store: Arc<dyn VectorStore>,
    search_engine: Arc<RwLock<SearchEngine>>,
    rag_engine: Arc<RwLock<RagEngine>>,
) -> StreamableHttpService<PaperedMcpServer, LocalSessionManager> {
    let session_manager = Arc::new(LocalSessionManager::default());
    let config = StreamableHttpServerConfig::default();

    StreamableHttpService::new(
        move || {
            Ok(PaperedMcpServer {
                store: store.clone(),
                search_engine: search_engine.clone(),
                rag_engine: rag_engine.clone(),
            })
        },
        session_manager,
        config,
    )
}

/// Run the MCP server over stdio (stdin/stdout).
///
/// This function blocks until stdin is closed or the transport encounters an
/// unrecoverable error.
pub async fn run_stdio_server(
    store: Arc<dyn VectorStore>,
    search_engine: Arc<RwLock<SearchEngine>>,
    rag_engine: Arc<RwLock<RagEngine>>,
) {
    let server = PaperedMcpServer {
        store,
        search_engine,
        rag_engine,
    };

    let transport = rmcp::transport::io::stdio();

    match serve_server(server, transport).await {
        Ok(running) => {
            tracing::info!("MCP stdio server started");
            if let Err(e) = running.waiting().await {
                tracing::error!("MCP stdio server error: {e}");
            }
        }
        Err(e) => {
            tracing::error!("MCP stdio server failed to initialize: {e}");
        }
    }
}
