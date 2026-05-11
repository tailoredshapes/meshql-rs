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

pub struct MeshqlClient {
    base_url: String,
    http: reqwest::Client,
}

impl MeshqlClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Build a client from `env_var`, falling back to `default_url` when the
    /// variable is unset.
    pub fn from_env(env_var: &str, default_url: &str) -> Self {
        let base_url = std::env::var(env_var).unwrap_or_else(|_| default_url.to_string());
        Self::new(base_url)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `GET /<entity>/api` — returns the array of envelopes (id + payload fields).
    pub async fn list(&self, entity: &str) -> anyhow::Result<Value> {
        let url = format!("{}/{entity}/api", self.base_url);
        let resp = self
            .http
            .get(&url)
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
            .http
            .get(&url)
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
            .http
            .get(&url)
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
            .http
            .post(&url)
            .json(body)
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
            .http
            .post(&url)
            .json(&body)
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
