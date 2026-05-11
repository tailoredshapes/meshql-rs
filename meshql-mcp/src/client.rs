//! Thin HTTP client wrapping a meshql REST deployment.
//!
//! Used by the catalog tools and by custom tools that need to hit
//! arbitrary deployment-specific endpoints. Construct with [`MeshqlClient::new`]
//! when the base URL is known, or [`MeshqlClient::from_env`] to read it from
//! an environment variable (with a fallback default).

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
}
