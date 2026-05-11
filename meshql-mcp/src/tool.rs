//! MCP tool primitives: `Tool`, `ToolHandler`, `ToolFuture`, and the
//! `wrap_text_result` helper used to produce the `tools/call` response shape
//! that MCP clients expect.

use crate::client::MeshqlClient;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type ToolFuture = Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>>;
pub type ToolHandler = Arc<dyn Fn(Arc<MeshqlClient>, Value) -> ToolFuture + Send + Sync>;

#[derive(Clone)]
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub handler: ToolHandler,
}

/// Helper: wrap a `Value` result so MCP `tools/call` returns the
/// `{ content: [{ type: "text", text: "..." }] }` shape clients expect.
///
/// MCP requires `structuredContent` to be an object — wrap arrays and scalars
/// in `{ "result": <value> }` so clients that validate the wire format don't
/// reject the response.
pub fn wrap_text_result(value: &Value) -> Value {
    let structured = if value.is_object() {
        value.clone()
    } else {
        serde_json::json!({ "result": value })
    };
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_default(),
        }],
        "structuredContent": structured,
    })
}
