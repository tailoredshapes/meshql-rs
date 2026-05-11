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
//! - [`schema`] — hand-rolled parser for the meshql GraphQL subset, used
//!   by the capability builder to auto-derive baseline tools.
//! - [`transport`] — `MeshqlMcpServer` + `McpServerConfig`, the stdio
//!   JSON-RPC transport.

pub mod catalog;
pub mod client;
pub mod schema;
pub mod tool;
pub mod transport;

pub use client::MeshqlClient;
pub use schema::{
    parse_meshql_schema, render_entity_field_selection, EntityField, ParsedSchema, QueryOp,
};
pub use tool::{wrap_text_result, Tool, ToolFuture, ToolHandler};
pub use transport::{EntityConfig, McpServerConfig, MeshqlMcpServer};
