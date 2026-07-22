//! Minimal GraphQL-over-HTTP client. No GraphQL client library — this
//! workspace's convention (see meshql-restlette's ProjectionUpdater-style
//! callers and the Java reference `ProjectionUpdater`) is plain
//! `{"query": "..."}"` POSTs, parsed by hand.

use anyhow::{anyhow, Context};
use serde_json::Value;

/// POST a GraphQL query and return its `data` object. Errors on a non-2xx
/// response or a non-empty `errors` array — this worker never treats a
/// partial/error GraphQL response as usable detail.
pub async fn graphql_query(
    client: &reqwest::Client,
    url: &str,
    query: &str,
    auth: Option<(&str, &str)>,
) -> anyhow::Result<Value> {
    let mut req = client
        .post(url)
        .json(&serde_json::json!({ "query": query }));
    if let Some((name, value)) = auth {
        req = req.header(name, value);
    }
    let resp = req.send().await.context("GraphQL request failed")?;
    let status = resp.status();
    let body: Value = resp.json().await.context("GraphQL response was not JSON")?;
    if !status.is_success() {
        return Err(anyhow!("GraphQL request to {url} failed: {status} {body}"));
    }
    if let Some(errors) = body.get("errors") {
        return Err(anyhow!("GraphQL errors from {url}: {errors}"));
    }
    body.get("data")
        .cloned()
        .ok_or_else(|| anyhow!("GraphQL response from {url} had no 'data': {body}"))
}
