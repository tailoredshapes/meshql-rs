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
//! - [`capability`] — high-level `Capability` + `CapabilityHandler` for
//!   declaring named, templated MCP operations.
//! - [`catalog`] — legacy generic `catalog.list / catalog.get / catalog.search`
//!   tools. Slated for removal once all apps migrate to capabilities.
//! - [`schema`] — hand-rolled parser for the meshql GraphQL subset, used
//!   by the capability builder to auto-derive baseline tools.
//! - [`transport`] — `MeshqlMcpServer` + `McpServerConfig`, the stdio
//!   JSON-RPC transport.

pub mod capability;
pub mod catalog;
pub mod client;
pub mod schema;
pub mod tool;
pub mod transport;

pub use capability::{CapabilitiesBuilder, Capability, CapabilityHandler};
pub use client::MeshqlClient;
pub use schema::{
    parse_meshql_schema, render_entity_field_selection, EntityField, ParsedSchema, QueryOp,
};
pub use tool::{wrap_text_result, Tool, ToolFuture, ToolHandler};
pub use transport::{McpServerConfig, MeshqlMcpServer};
