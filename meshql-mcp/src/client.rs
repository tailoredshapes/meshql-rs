//! HTTP client for a meshql-rs deployment.
//!
//! Three methods cover the typical capability needs:
//!
//! - [`gql`](MeshqlClient::gql) — POST a GraphQL query to a graphlette endpoint.
//!   Use this for reads (per the project CQRS rule: reads via `/graph`, writes
//!   via `/api`). The `CapabilityHandler::GraphQuery` variant dispatches here.
//! - [`get_path`](MeshqlClient::get_path) — GET an arbitrary REST path. Use
//!   this for computed/aggregated endpoints not yet in the graph schema
//!   (e.g. `/test_environment/:id/history`). The `CapabilityHandler::RestGet`
//!   variant dispatches here.
//! - [`post_path`](MeshqlClient::post_path) — POST an arbitrary REST path.
//!   Use this for writes and commands (e.g. `/change_request/:id/plan`). The
//!   `CapabilityHandler::RestPost` variant dispatches here.
//!
//! The legacy [`list`](MeshqlClient::list) and [`get`](MeshqlClient::get)
//! REST entity-fetch methods are retained for callers (like groundwork's
//! in-memory dependency-graph snapshot loader) that haven't migrated to
//! GraphQL yet. New code should prefer `gql` for reads.
//!
//! Construct with [`MeshqlClient::new`] when the base URL is known, or
//! [`MeshqlClient::from_env`] to read it from an environment variable.

use anyhow::Context;
use serde_json::Value;

/// Trusted-header identity attached to outbound requests. When `Some`, every
/// HTTP call this client makes carries `X-Manifold-User-Id` (and optional
/// `X-Manifold-User-Groups`) so the receiving meshql deployment can resolve
/// roles via its configured `Auth`. The MCP server passes this through
/// verbatim from its environment — it does not negotiate the identity with
/// the MCP client.
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub user_id: Option<String>,
    pub groups: Option<String>,
}

impl Identity {
    pub fn is_set(&self) -> bool {
        self.user_id
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }
}

pub struct MeshqlClient {
    base_url: String,
    http: reqwest::Client,
    identity: Identity,
}

impl MeshqlClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::new(),
            identity: Identity::default(),
        }
    }

    /// Build a client from `env_var`, falling back to `default_url` when the
    /// variable is unset. Also picks up `MANIFOLD_USER_ID` and
    /// `MANIFOLD_USER_GROUPS` if set so write capabilities work out of the box.
    pub fn from_env(env_var: &str, default_url: &str) -> Self {
        let base_url = std::env::var(env_var).unwrap_or_else(|_| default_url.to_string());
        Self::new(base_url).with_identity_from_env()
    }

    /// Read `MANIFOLD_USER_ID` and `MANIFOLD_USER_GROUPS` from the environment
    /// and attach as the trusted identity carried on every outbound request.
    /// Empty / unset env vars leave the identity empty, in which case writes
    /// will fail with a clear error (see [`Self::require_identity`]).
    pub fn with_identity_from_env(mut self) -> Self {
        self.identity.user_id = std::env::var("MANIFOLD_USER_ID").ok();
        self.identity.groups = std::env::var("MANIFOLD_USER_GROUPS").ok();
        self
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Used by write capabilities before issuing a request — returns a
    /// human-readable error suitable for surfacing to the MCP client when
    /// the configured identity is missing.
    pub fn require_identity(&self) -> anyhow::Result<()> {
        if self.identity.is_set() {
            Ok(())
        } else {
            anyhow::bail!(
                "this write requires an identity — set MANIFOLD_USER_ID in this MCP server's env (e.g. in your client's .mcp.json) to enable writes"
            )
        }
    }

    fn apply_identity(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(id) = self.identity.user_id.as_deref() {
            if !id.is_empty() {
                req = req.header("X-Manifold-User-Id", id);
            }
        }
        if let Some(groups) = self.identity.groups.as_deref() {
            if !groups.is_empty() {
                req = req.header("X-Manifold-User-Groups", groups);
            }
        }
        req
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `GET /<entity>/api` — returns the array of envelopes (id + payload fields).
    pub async fn list(&self, entity: &str) -> anyhow::Result<Value> {
        let url = format!("{}/{entity}/api", self.base_url);
        let resp = self
            .apply_identity(self.http.get(&url))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("GET {url} -> {}", resp.status());
        }
        resp.json::<Value>()
            .await
            .with_context(|| format!("decode {url}"))
    }

    /// `GET /<entity>/api/<id>` — returns the envelope, or `None` on 404.
    pub async fn get(&self, entity: &str, id: &str) -> anyhow::Result<Option<Value>> {
        let url = format!("{}/{entity}/api/{id}", self.base_url);
        let resp = self
            .apply_identity(self.http.get(&url))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            anyhow::bail!("GET {url} -> {}", resp.status());
        }
        let v = resp
            .json::<Value>()
            .await
            .with_context(|| format!("decode {url}"))?;
        Ok(Some(v))
    }

    /// `GET <base_url><path>` — for custom-endpoint paths that aren't
    /// covered by the entity-shaped helpers. `path` should start with `/`.
    pub async fn get_path(&self, path: &str) -> anyhow::Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .apply_identity(self.http.get(&url))
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("GET {url} -> {}", resp.status());
        }
        resp.json::<Value>()
            .await
            .with_context(|| format!("decode {url}"))
    }

    /// `POST <base_url><path>` with a JSON body. `path` should start with `/`.
    pub async fn post_path(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .apply_identity(self.http.post(&url).json(body))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("POST {url} -> {}", resp.status());
        }
        resp.json::<Value>()
            .await
            .with_context(|| format!("decode {url}"))
    }

    /// `PUT <base_url><path>` with a JSON body.
    pub async fn put_path(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .apply_identity(self.http.put(&url).json(body))
            .send()
            .await
            .with_context(|| format!("PUT {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("PUT {url} -> {}", resp.status());
        }
        resp.json::<Value>()
            .await
            .with_context(|| format!("decode {url}"))
    }

    /// `DELETE <base_url><path>`. Returns the JSON envelope of the deletion
    /// confirmation, or `null` for empty bodies.
    pub async fn delete_path(&self, path: &str) -> anyhow::Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .apply_identity(self.http.delete(&url))
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("DELETE {url} -> {}", resp.status());
        }
        // Some servers return an empty body on DELETE; tolerate that.
        let text = resp.text().await.with_context(|| format!("decode {url}"))?;
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).with_context(|| format!("parse {url}"))
    }

    /// POST a GraphQL query to a meshql graphlette endpoint.
    ///
    /// `path` is the entity-relative graph route (e.g. `/deployable/graph`).
    /// The body sent is `{ "query": "<query>" }` — no `variables` map for
    /// v1 (the catalog tools build self-contained query strings). The
    /// response is parsed as `{ "data": ..., "errors": [...] }`, matching
    /// the meshql graphlette wire format. When `errors` is present and
    /// non-empty, the joined messages are returned as an `Err`. Otherwise
    /// the `data` field (which may be any JSON value) is returned.
    ///
    /// TODO: thread GraphQL `variables` through so callers can parameterize
    /// queries without string-escaping ids themselves.
    pub async fn gql(&self, path: &str, query: &str) -> anyhow::Result<Value> {
        let url = format!("{}{path}", self.base_url);
        let body = serde_json::json!({ "query": query });
        let resp = self
            .apply_identity(self.http.post(&url).json(&body))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("POST {url} -> {}", resp.status());
        }
        let mut payload: Value = resp.json().await.with_context(|| format!("decode {url}"))?;
        if let Some(errors) = payload.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                let joined = errors
                    .iter()
                    .map(|e| {
                        e.get("message")
                            .and_then(|m| m.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| e.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                anyhow::bail!("graphql {url}: {joined}");
            }
        }
        Ok(payload
            .get_mut("data")
            .map(std::mem::take)
            .unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spin up a one-shot HTTP/1.1 responder on `127.0.0.1:0` that returns
    /// `response_body` (already-serialized JSON) for the first request,
    /// captures the request body, and shuts down. Returns the base URL and
    /// a JoinHandle yielding the request body bytes.
    async fn one_shot_server(response_body: String) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut total = Vec::new();
            // Read until we have the full request body. We rely on a
            // Content-Length header from reqwest's json() call.
            let mut content_length: Option<usize> = None;
            let mut header_end: Option<usize> = None;
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                total.extend_from_slice(&buf[..n]);
                if header_end.is_none() {
                    if let Some(idx) = total.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(idx + 4);
                        let header_str = std::str::from_utf8(&total[..idx]).unwrap_or("");
                        for line in header_str.split("\r\n") {
                            if let Some(rest) = line
                                .strip_prefix("Content-Length:")
                                .or_else(|| line.strip_prefix("content-length:"))
                            {
                                content_length = rest.trim().parse().ok();
                            }
                        }
                    }
                }
                if let (Some(end), Some(cl)) = (header_end, content_length) {
                    if total.len() >= end + cl {
                        break;
                    }
                }
            }
            let body_start = header_end.unwrap_or(total.len());
            let body = String::from_utf8_lossy(&total[body_start..]).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
            body
        });
        (url, handle)
    }

    #[tokio::test]
    async fn gql_posts_query_and_returns_data() {
        let response = r#"{"data":{"getAll":[{"id":"d1","name":"first"}]}}"#;
        let (url, handle) = one_shot_server(response.to_string()).await;
        let client = MeshqlClient::new(url);
        let data = client
            .gql("/deployable/graph", "{ getAll { id name } }")
            .await
            .expect("gql ok");
        let body = handle.await.unwrap();
        // Body should be `{"query":"{ getAll { id name } }"}` — verify it
        // round-trips through serde_json so we don't lock down whitespace.
        let parsed: Value = serde_json::from_str(&body).expect("request was JSON");
        assert_eq!(parsed["query"], "{ getAll { id name } }");
        assert_eq!(data["getAll"][0]["id"], "d1");
        assert_eq!(data["getAll"][0]["name"], "first");
    }

    #[tokio::test]
    async fn gql_returns_err_when_errors_array_present() {
        let response = r#"{"data":null,"errors":[{"message":"boom"},{"message":"again"}]}"#;
        let (url, _handle) = one_shot_server(response.to_string()).await;
        let client = MeshqlClient::new(url);
        let err = client
            .gql("/deployable/graph", "{ getAll { id } }")
            .await
            .expect_err("should fail");
        let msg = format!("{err}");
        assert!(msg.contains("boom"), "got: {msg}");
        assert!(msg.contains("again"), "got: {msg}");
    }
}
