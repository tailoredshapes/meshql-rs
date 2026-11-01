//! Bounded HTTP dispatch to one trusted deployment. Identity is launch context,
//! not tool arguments; downstream Auth remains the only authorization decision.
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::time::Duration;
#[derive(Debug)]
pub(crate) struct BackendStatus(pub u16);
impl std::fmt::Display for BackendStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "backend HTTP {}", self.0)
    }
}
impl std::error::Error for BackendStatus {}
#[derive(Clone, Debug, Default)]
pub struct Identity {
    pub user_id: Option<String>,
    pub groups: Option<String>,
}
impl Identity {
    pub fn is_set(&self) -> bool {
        self.user_id.as_ref().is_some_and(|s| !s.is_empty())
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
            base_url: base_url.into().trim_end_matches('/').into(),
            identity: Identity::default(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .connect_timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("HTTP client"),
        }
    }
    pub fn from_env(name: &str, default: &str) -> Self {
        Self::new(std::env::var(name).unwrap_or_else(|_| default.into())).with_identity_from_env()
    }
    pub fn with_identity_from_env(mut self) -> Self {
        self.identity = Identity {
            user_id: std::env::var("MANIFOLD_USER_ID").ok(),
            groups: std::env::var("MANIFOLD_USER_GROUPS").ok(),
        };
        self
    }
    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.identity = identity;
        self
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    /// Compatibility helper only. Standard tools do not impose an extra gate.
    pub fn require_identity(&self) -> Result<()> {
        ensure!(self.identity.is_set(), "identity is not configured");
        Ok(())
    }
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
    fn identity_request(&self, mut r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        for (key, value) in [
            ("X-Manifold-User-Id", &self.identity.user_id),
            ("X-Manifold-User-Groups", &self.identity.groups),
        ] {
            if let Some(value) = value.as_ref().filter(|s| !s.is_empty()) {
                r = r.header(key, value);
            }
        }
        r
    }
    async fn request_json(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        ensure!(
            path.starts_with('/') && !path.starts_with("//"),
            "invalid deployment path"
        );
        let mut request = self.identity_request(
            self.http
                .request(method.clone(), format!("{}{path}", self.base_url)),
        );
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("backend request failed"))?;
        if !response.status().is_success() {
            return Err(BackendStatus(response.status().as_u16()).into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("backend response failed"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= 1048576,
                "backend response limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() && method == reqwest::Method::DELETE {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub async fn list(&self, entity: &str) -> Result<Value> {
        self.get_path(&format!("/{}/api", crate::template::segment(entity)?))
            .await
    }
    pub async fn get(&self, entity: &str, id: &str) -> Result<Option<Value>> {
        let path = format!(
            "/{}/api/{}",
            crate::template::segment(entity)?,
            crate::template::segment(id)?
        );
        match self.get_path(&path).await {
            Ok(v) => Ok(Some(v)),
            Err(e)
                if e.downcast_ref::<BackendStatus>()
                    .is_some_and(|status| status.0 == 404) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
    pub async fn get_path(&self, path: &str) -> Result<Value> {
        self.request_json(reqwest::Method::GET, path, None).await
    }
    pub async fn post_path(&self, path: &str, body: &Value) -> Result<Value> {
        self.request_json(reqwest::Method::POST, path, Some(body))
            .await
    }
    pub async fn put_path(&self, path: &str, body: &Value) -> Result<Value> {
        self.request_json(reqwest::Method::PUT, path, Some(body))
            .await
    }
    pub async fn delete_path(&self, path: &str) -> Result<Value> {
        self.request_json(reqwest::Method::DELETE, path, None).await
    }
    pub async fn gql(&self, path: &str, query: &str) -> Result<Value> {
        self.gql_variables(path, query, json!({})).await
    }
    pub async fn gql_variables(&self, path: &str, query: &str, variables: Value) -> Result<Value> {
        let mut payload = self
            .post_path(path, &json!({"query":query,"variables":variables}))
            .await?;
        if let Some(errors) = payload
            .get("errors")
            .and_then(Value::as_array)
            .filter(|es| !es.is_empty())
        {
            anyhow::bail!(
                "graphql: {}",
                errors
                    .iter()
                    .map(|e| e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("GraphQL error"))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        Ok(payload
            .get_mut("data")
            .map(std::mem::take)
            .unwrap_or(Value::Null))
    }
}
#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
