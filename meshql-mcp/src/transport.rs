//! MCP 2025-06-18 tools profile over bounded newline-delimited stdio only.
use crate::{
    capability::Capability,
    client::MeshqlClient,
    frame::{self, Frame},
    tool::{wrap_text_result, Tool},
};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub struct McpServerConfig {
    pub server_name: String,
    pub server_version: String,
    pub client: Arc<MeshqlClient>,
    pub capabilities: Vec<Capability>,
}
#[derive(PartialEq, Eq)]
enum Stage {
    New,
    Initializing,
    Ready,
}
pub struct MeshqlMcpServer {
    config: McpServerConfig,
    tools: Vec<Tool>,
    validators: Vec<jsonschema::Validator>,
    stage: Mutex<Stage>,
    frame_limit: usize,
}
impl MeshqlMcpServer {
    pub fn new(config: McpServerConfig) -> Self {
        Self::try_new(config).expect("invalid MCP configuration")
    }
    pub fn try_new(mut config: McpServerConfig) -> anyhow::Result<Self> {
        let backend = reqwest::Url::parse(config.client.base_url())?;
        anyhow::ensure!(
            ["http", "https"].contains(&backend.scheme())
                && backend.host_str().is_some()
                && backend.query().is_none()
                && backend.fragment().is_none(),
            "invalid MCP backend URL"
        );
        let mut tools: Vec<_> = std::mem::take(&mut config.capabilities)
            .into_iter()
            .map(|c| c.into_tool(config.client.clone()))
            .collect();
        tools.sort_by_key(|t| t.name);
        let mut names = HashSet::new();
        let mut validators = Vec::new();
        for tool in &tools {
            anyhow::ensure!(names.insert(tool.name), "duplicate MCP tool");
            anyhow::ensure!(
                !tool.name.is_empty()
                    && tool.name.len() <= 128
                    && tool
                        .name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)),
                "invalid MCP tool name"
            );
            let mut options = jsonschema::options();
            if tool.input_schema.get("$schema").is_none() {
                options.with_draft(jsonschema::Draft::Draft7);
            }
            validators.push(
                options
                    .should_validate_formats(true)
                    .build(&tool.input_schema)
                    .map_err(|_| anyhow::anyhow!("invalid MCP tool input schema"))?,
            );
        }
        let frame_limit = match std::env::var("MESHQL_MCP_MAX_FRAME_BYTES") {
            Ok(value) => value.parse::<usize>()?,
            Err(_) => 1048576,
        };
        anyhow::ensure!(
            (1..=16777216).contains(&frame_limit),
            "invalid MCP frame limit"
        );
        Ok(Self {
            config,
            tools,
            validators,
            stage: Mutex::new(Stage::New),
            frame_limit,
        })
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
    pub async fn serve_stdio(&self) -> anyhow::Result<()> {
        let mut input = BufReader::new(tokio::io::stdin());
        let mut output = tokio::io::stdout();
        loop {
            let response = match frame::read(&mut input, self.frame_limit).await? {
                Frame::Eof => break,
                Frame::Oversized => Some(error_frame(Value::Null, -32600, "frame limit exceeded")),
                Frame::Bytes(bytes) => match std::str::from_utf8(&bytes) {
                    Ok(line) => self.handle_line(line).await,
                    Err(_) => Some(error_frame(Value::Null, -32700, "invalid UTF-8 JSON")),
                },
            };
            if let Some(response) = response {
                write_line(&mut output, &response).await?;
            }
        }
        Ok(())
    }
    pub(crate) async fn handle_line(&self, line: &str) -> Option<Value> {
        if line.trim().is_empty() {
            return None;
        }
        match serde_json::from_str(line) {
            Ok(request) => self.handle_request(request).await,
            Err(_) => Some(error_frame(Value::Null, -32700, "parse error")),
        }
    }
    pub(crate) async fn handle_request(&self, request: Value) -> Option<Value> {
        if !valid_envelope(&request) {
            let id = request
                .get("id")
                .filter(|id| id.is_string() || id.as_i64().is_some())
                .cloned()
                .unwrap_or(Value::Null);
            return Some(error_frame(id, -32600, "invalid request"));
        }
        let method = request["method"].as_str().unwrap();
        if request.get("id").is_none() {
            if method == "notifications/initialized" {
                let mut stage = self.stage.lock().unwrap();
                if *stage == Stage::Initializing {
                    *stage = Stage::Ready;
                }
            }
            return None;
        }
        let id = request["id"].clone();
        let params = request.get("params").cloned().unwrap_or(json!({}));
        let result = if !params.is_object() {
            Err((-32602, "params must be an object".into()))
        } else {
            self.dispatch(method, params).await
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err((code, message)) => error_frame(id, code, &message),
        })
    }
    async fn dispatch(&self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        if method == "initialize" {
            return self.initialize(params);
        }
        if method == "ping" {
            return Ok(json!({}));
        }
        if *self.stage.lock().unwrap() != Stage::Ready {
            return Err((-32600, "client is not initialized".into()));
        }
        match method {
            "tools/list" => Ok(
                json!({"tools":self.tools.iter().map(|t|json!({"name":t.name,"description":t.description,"inputSchema":t.input_schema})).collect::<Vec<_>>()}),
            ),
            "tools/call" => self.call_tool(params).await,
            _ => Err((-32601, "method not found".into())),
        }
    }
    fn initialize(&self, params: Value) -> Result<Value, (i64, String)> {
        let valid = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .is_some()
            && params.get("capabilities").is_some_and(Value::is_object)
            && params.get("clientInfo").is_some_and(|v| {
                v.get("name").is_some_and(Value::is_string)
                    && v.get("version").is_some_and(Value::is_string)
            });
        if !valid {
            return Err((-32602, "invalid initialize params".into()));
        }
        let mut stage = self.stage.lock().unwrap();
        if *stage != Stage::New {
            return Err((-32600, "already initialized".into()));
        }
        *stage = Stage::Initializing;
        Ok(
            json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{"listChanged":false}},
            "serverInfo":{"name":self.config.server_name,"version":self.config.server_version}}),
        )
    }
    async fn call_tool(&self, params: Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "tool name required".into()))?;
        let index = self
            .tools
            .iter()
            .position(|t| t.name == name)
            .ok_or((-32602, "unknown tool".into()))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        if !args.is_object() || !self.validators[index].is_valid(&args) {
            return Err((-32602, "invalid tool arguments".into()));
        }
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            (self.tools[index].handler)(self.config.client.clone(), args),
        )
        .await;
        Ok(match result {
            Ok(Ok(value)) => wrap_text_result(&value),
            Ok(Err(error)) => {
                json!({"content":[{"type":"text","text":format!("error: {error}")}],"isError":true})
            }
            Err(_) => {
                json!({"content":[{"type":"text","text":"tool deadline exceeded"}],"isError":true})
            }
        })
    }
}
fn valid_envelope(v: &Value) -> bool {
    v.is_object()
        && v.get("jsonrpc") == Some(&json!("2.0"))
        && v.get("method").is_some_and(Value::is_string)
        && v.get("id")
            .is_none_or(|id| id.is_string() || id.as_i64().is_some())
}
fn error_frame(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
async fn write_line<W: AsyncWrite + Unpin>(out: &mut W, frame: &Value) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(frame)?;
    out.write_all(&bytes).await?;
    out.write_all(b"\n").await?;
    out.flush().await?;
    Ok(())
}
#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
