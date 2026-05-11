//! `meshql-mcp` — Model Context Protocol server building blocks for
//! meshql-rs deployments.
//!
//! This crate ports the stdio JSON-RPC transport, REST client, and tool
//! registry that originated in `groundwork-mcp` (in the manifold repo) into
//! the meshql-rs workspace so other meshql-based services can reuse them.
//!
//! Modules:
//! - [`client`] — `MeshqlClient`, a thin reqwest wrapper around the REST API.
//! - [`tool`] — `Tool`, `ToolHandler`, `ToolFuture`, and `wrap_text_result`.
//! - [`catalog`] — generic `catalog.list / catalog.get / catalog.search` tools
//!   parameterized over an entity-name list.
//! - [`transport`] — `MeshqlMcpServer` + `McpServerConfig`, the stdio
//!   JSON-RPC transport.

pub mod catalog;
pub mod client;
pub mod tool;
pub mod transport;
