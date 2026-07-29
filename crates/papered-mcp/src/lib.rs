//! MCP Server implementation for Papered, backed by the official [rmcp] SDK.
//!
//! Provides tools (search, RAG, paper retrieval), resources (`paper://` URIs),
//! and prompts for MCP clients. Supports streamable HTTP (via Axum) and stdio
//! transports.

mod prompts;
mod resources;
mod server;
mod tools;
mod util;

pub use server::{PaperedMcpServer, build_mcp_service, run_stdio_server};
