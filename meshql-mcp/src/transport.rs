//! Stdio JSON-RPC transport for the meshql MCP server.
//!
//! `MeshqlMcpServer::serve_stdio` runs the request loop (one JSON-RPC request
//! per line on stdin, one response per line on stdout). Configuration —
//! server name/version, REST client, catalogue entity list, and any custom
//! tools — is supplied via [`McpServerConfig`].
//!
//! Methods supported:
//!   - `initialize`            — server info + capabilities
//!   - `notifications/initialized` — no-op acknowledgement
//!   - `tools/list`            — tool catalogue
//!   - `tools/call`            — { name, arguments } → result
//!   - `ping`                  — health check
//!
//! Use stderr for logs; stdout is reserved for JSON-RPC frames.

use crate::capability::Capability;
use crate::client::MeshqlClient;
use crate::tool::{wrap_text_result, Tool};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Configuration for [`MeshqlMcpServer`]. Each app's `<app>-mcp` bin builds
/// a `capabilities` vec (usually via `CapabilitiesBuilder`) and passes it
/// here along with the HTTP client and server identity.
pub struct McpServerConfig {
    pub server_name: String,
    pub server_version: String,
    pub client: Arc<MeshqlClient>,
    /// All operations the server exposes through `tools/list`. Each capability
    /// is converted to a low-level [`Tool`] at server-construction time.
    pub capabilities: Vec<Capability>,
}

pub struct MeshqlMcpServer {
    config: McpServerConfig,
    tools: Vec<Tool>,
}

impl MeshqlMcpServer {
    /// Build a server, eagerly converting each [`Capability`] into a
    /// dispatch-ready [`Tool`].
    pub fn new(mut config: McpServerConfig) -> Self {
        let capabilities = std::mem::take(&mut config.capabilities);
        let client = config.client.clone();
        let tools = capabilities
            .into_iter()
            .map(|cap| cap.into_tool(client.clone()))
            .collect();
        Self { config, tools }
    }

    pub fn server_name(&self) -> &str {
        &self.config.server_name
    }

    pub fn server_version(&self) -> &str {
        &self.config.server_version
    }

    pub fn client(&self) -> &Arc<MeshqlClient> {
        &self.config.client
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Run the stdio JSON-RPC request loop until stdin closes.
    pub async fn serve_stdio(&self) -> anyhow::Result<()> {
        eprintln!(
            "{} v{} → {} ({} tools)",
            self.config.server_name,
            self.config.server_version,
            self.config.client.base_url(),
            self.tools.len()
        );

        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin).lines();
        let mut stdout = tokio::io::stdout();

        while let Some(line) = reader.next_line().await? {
            if let Some(frame) = self.handle_line(&line).await {
                write_line(&mut stdout, &frame).await?;
            }
        }
        Ok(())
    }

    /// Parse a single input line and produce the response frame (or `None`
    /// for notifications/blank lines). Pulled out so it can be unit-tested
    /// without spinning up real stdio.
    pub(crate) async fn handle_line(&self, line: &str) -> Option<Value> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                return Some(error_frame(
                    Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                ));
            }
        };
        self.handle_request(req).await
    }

    /// Dispatch a parsed JSON-RPC request. Returns `Some(response_frame)` for
    /// requests and `None` for notifications.
    pub(crate) async fn handle_request(&self, req: Value) -> Option<Value> {
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let is_notification = req.get("id").is_none();

        if method.starts_with("notifications/") {
            return None;
        }

        let result = match method.as_str() {
            "initialize" => Ok(self.initialize_response()),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.list_tools_response()),
            "tools/call" => self.call_tool(params).await,
            other => Err((-32601i64, format!("method not found: {other}"))),
        };

        if is_notification {
            return None;
        }

        Some(match result {
            Ok(value) => result_frame(id, value),
            Err((code, msg)) => error_frame(id, code, &msg),
        })
    }

    fn initialize_response(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": self.config.server_name,
                "version": self.config.server_version
            }
        })
    }

    fn list_tools_response(&self) -> Value {
        let entries: Vec<Value> = self
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                })
            })
            .collect();
        json!({ "tools": entries })
    }

    async fn call_tool(&self, params: Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing 'name' in tools/call params".to_string()))?;
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let tool = self
            .tools
            .iter()
            .find(|t| t.name == name)
            .ok_or((-32601, format!("unknown tool: {name}")))?;

        match (tool.handler)(self.config.client.clone(), args).await {
            Ok(value) => Ok(wrap_text_result(&value)),
            Err(e) => Ok(json!({
                "content": [{ "type": "text", "text": format!("error: {e}") }],
                "isError": true,
            })),
        }
    }
}

fn result_frame(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_frame(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

async fn write_line<W: AsyncWriteExt + Unpin>(out: &mut W, frame: &Value) -> anyhow::Result<()> {
    let line = serde_json::to_string(frame)?;
    out.write_all(line.as_bytes()).await?;
    out.write_all(b"\n").await?;
    out.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Capability, CapabilityHandler};
    use crate::tool::ToolFuture;
    use serde_json::json;

    fn test_server(extras: Vec<Capability>) -> MeshqlMcpServer {
        let mut capabilities = vec![
            Capability {
                name: "list_deployables",
                description: "List every deployable.",
                input_schema: json!({ "type": "object" }),
                handler: CapabilityHandler::GraphQuery {
                    path: "/deployable/graph".into(),
                    query_template: "{ getAll { id name } }".into(),
                },
            },
            Capability {
                name: "list_services",
                description: "List every service.",
                input_schema: json!({ "type": "object" }),
                handler: CapabilityHandler::GraphQuery {
                    path: "/service/graph".into(),
                    query_template: "{ getAll { id name } }".into(),
                },
            },
        ];
        capabilities.extend(extras);
        MeshqlMcpServer::new(McpServerConfig {
            server_name: "test-mcp".to_string(),
            server_version: "9.9.9".to_string(),
            client: Arc::new(MeshqlClient::new("http://127.0.0.1:1")),
            capabilities,
        })
    }

    fn dummy_capability(name: &'static str) -> Capability {
        Capability {
            name,
            description: "dummy",
            input_schema: json!({ "type": "object" }),
            handler: CapabilityHandler::Custom(Arc::new(|_client, _args| -> ToolFuture {
                Box::pin(async move { Ok(json!({ "ok": true })) })
            })),
        }
    }

    #[tokio::test]
    async fn initialize_returns_capabilities() {
        let server = test_server(vec![]);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        let resp = server.handle_request(req).await.expect("response present");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        let result = &resp["result"];
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "test-mcp");
        assert_eq!(result["serverInfo"]["version"], "9.9.9");
    }

    #[tokio::test]
    async fn tools_list_returns_configured_tools() {
        let server = test_server(vec![dummy_capability("custom.thing")]);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        });
        let resp = server.handle_request(req).await.expect("response present");
        let names: Vec<String> = resp["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "list_deployables".to_string(),
                "list_services".to_string(),
                "custom.thing".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_returns_is_error() {
        let server = test_server(vec![]);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "nope", "arguments": {} }
        });
        let resp = server.handle_request(req).await.expect("response present");
        // Unknown tool surfaces as a JSON-RPC error (method-not-found code).
        // The plan also accepts an isError envelope; we follow the
        // groundwork-mcp behavior of returning -32601 from call_tool.
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn tools_call_handler_error_returns_is_error_envelope() {
        // A handler that returns an error gets wrapped into an isError content
        // envelope (rather than a JSON-RPC error), so the LLM client can read
        // the message.
        let mut custom = vec![Capability {
            name: "boom",
            description: "always errors",
            input_schema: json!({ "type": "object" }),
            handler: CapabilityHandler::Custom(Arc::new(|_client, _args| -> ToolFuture {
                Box::pin(async move { Err(anyhow::anyhow!("kaboom")) })
            })),
        }];
        custom.push(dummy_capability("noop"));
        let server = test_server(custom);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "boom", "arguments": {} }
        });
        let resp = server.handle_request(req).await.expect("response present");
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            text.contains("kaboom"),
            "expected error message, got: {text}"
        );
    }

    #[tokio::test]
    async fn parse_error_returns_neg_32700() {
        let server = test_server(vec![]);
        let resp = server
            .handle_line("this is not json")
            .await
            .expect("response present");
        assert_eq!(resp["error"]["code"], -32700);
        assert_eq!(resp["id"], Value::Null);
    }

    #[tokio::test]
    async fn unknown_method_returns_neg_32601() {
        let server = test_server(vec![]);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "frob"
        });
        let resp = server.handle_request(req).await.expect("response present");
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn notifications_initialized_returns_none() {
        let server = test_server(vec![]);
        let req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let resp = server.handle_request(req).await;
        assert!(resp.is_none(), "notifications must not produce a response");
    }

    #[tokio::test]
    async fn blank_line_returns_none() {
        let server = test_server(vec![]);
        assert!(server.handle_line("").await.is_none());
        assert!(server.handle_line("   ").await.is_none());
    }
}
