//! Read-modify-write against `hen_productivity`, entirely over REST/GraphQL
//! — never a direct database call, per the "single writer" invariant (the
//! worker is just another authorized REST caller). Mirrors the Java
//! `ProjectionUpdater` reference pattern: GraphQL exposes ids, REST doesn't,
//! so a fresh create is discovered afterward via the same GraphQL query used
//! to read current state.

use crate::config::WorkerConfig;
use crate::graphql::graphql_query;
use crate::productivity::HenProductivity;
use anyhow::{anyhow, Context};

fn auth_header(cfg: &WorkerConfig) -> Option<(&str, &str)> {
    cfg.auth_header
        .as_deref()
        .map(|h| (h, cfg.auth_value.as_str()))
}

/// GET the current hen_productivity for `hen_id` via GraphQL, discovering
/// its MeshQL id in the same call (GraphQL exposes ids; REST deliberately
/// doesn't — see meshql-patterns' REST ID model). `None` means this is the
/// hen's first lay_report. Query name is dialect-aware — Rust's farm
/// exposes `getHenProductivityByHen`, Java's and TS's both expose
/// `getByHen` — see the reconciliation note at the top of this plan. Its
/// `RootConfig` query template filters on `"payload.henId"` on every
/// landed backend (Mongo, SQLite both require the `payload.` prefix — see
/// "Facts to respect" at the top of this plan), which is an implementation
/// detail of the target deployment's config, invisible from here.
pub async fn get_current(
    client: &reqwest::Client,
    cfg: &WorkerConfig,
    hen_id: &str,
) -> anyhow::Result<Option<HenProductivity>> {
    let url = format!(
        "{}/hen_productivity/graph",
        cfg.target_graphql_base.trim_end_matches('/')
    );
    let now_ms = chrono::Utc::now().timestamp_millis();
    let query_name = cfg.query_dialect.hen_productivity_by_hen();
    let query = format!(
        r#"{{ {query_name}(id: "{hen_id}", at: {now_ms}) {{ id henId totalEggs lastLaidAt }} }}"#
    );
    let data = graphql_query(client, &url, &query, auth_header(cfg)).await?;
    let list = data
        .get(query_name)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    match list.into_iter().next() {
        None => Ok(None),
        Some(v) => Ok(Some(serde_json::from_value(v).context(
            "hen_productivity-by-hen result did not match the assumed HenProductivity shape",
        )?)),
    }
}

/// Write `next` via REST: `PUT` to its known id (an update-as-new-version)
/// if one exists, or `POST` if this is the hen's first productivity record.
/// A fresh `POST` does not need to re-discover its id here — the caller's
/// next `get_current` call (the next time an event for this hen is
/// processed) will find it, and within a single batch `write` always
/// `await`s the REST call to completion before returning, so a same-batch
/// second event for the same hen sees the first write's result.
pub async fn write(
    client: &reqwest::Client,
    cfg: &WorkerConfig,
    next: &HenProductivity,
) -> anyhow::Result<()> {
    let rest_base = format!(
        "{}/hen_productivity/api",
        cfg.target_rest_base.trim_end_matches('/')
    );
    let body = serde_json::to_value(next)?;

    if let Some(id) = &next.id {
        let url = format!("{rest_base}/{id}");
        let mut req = client.put(&url).json(&body);
        if let Some((name, value)) = auth_header(cfg) {
            req = req.header(name, value);
        }
        let resp = req.send().await.context("PUT hen_productivity failed")?;
        if !resp.status().is_success() {
            return Err(anyhow!("PUT {url} failed: {}", resp.status()));
        }
        return Ok(());
    }

    let mut req = client.post(&rest_base).json(&body);
    if let Some((name, value)) = auth_header(cfg) {
        req = req.header(name, value);
    }
    let resp = req.send().await.context("POST hen_productivity failed")?;
    if !resp.status().is_success() {
        return Err(anyhow!("POST {rest_base} failed: {}", resp.status()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{QueryDialect, WorkerConfig};
    use axum::extract::State;
    use axum::routing::{post, put};
    use axum::{Json, Router};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeStore(Arc<Mutex<Option<HenProductivity>>>);

    async fn graph_handler(State(store): State<FakeStore>, Json(body): Json<Value>) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(
            query.contains("getHenProductivityByHen"),
            "unexpected query: {query}"
        );
        let current = store.0.lock().unwrap().clone();
        let list = match current {
            Some(hp) => vec![serde_json::to_value(&hp).unwrap()],
            None => vec![],
        };
        Json(json!({ "data": { "getHenProductivityByHen": list } }))
    }

    async fn graph_handler_generic(
        State(store): State<FakeStore>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(query.contains("getByHen"), "unexpected query: {query}");
        let current = store.0.lock().unwrap().clone();
        let list = match current {
            Some(hp) => vec![serde_json::to_value(&hp).unwrap()],
            None => vec![],
        };
        Json(json!({ "data": { "getByHen": list } }))
    }

    async fn post_handler(State(store): State<FakeStore>, Json(body): Json<Value>) -> Json<Value> {
        let mut hp: HenProductivity = serde_json::from_value(body).unwrap();
        hp.id = Some("hp-generated".to_string());
        *store.0.lock().unwrap() = Some(hp.clone());
        Json(serde_json::to_value(&hp).unwrap())
    }

    async fn put_handler(
        axum::extract::Path(id): axum::extract::Path<String>,
        State(store): State<FakeStore>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut hp: HenProductivity = serde_json::from_value(body).unwrap();
        hp.id = Some(id);
        *store.0.lock().unwrap() = Some(hp.clone());
        Json(serde_json::to_value(&hp).unwrap())
    }

    async fn start() -> (String, FakeStore) {
        let store = FakeStore::default();
        let router = Router::new()
            .route("/hen_productivity/graph", post(graph_handler))
            .route("/hen_productivity/api", post(post_handler))
            .route("/hen_productivity/api/:id", put(put_handler))
            .with_state(store.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{addr}"), store)
    }

    async fn start_generic() -> (String, FakeStore) {
        let store = FakeStore::default();
        let router = Router::new()
            .route("/hen_productivity/graph", post(graph_handler_generic))
            .route("/hen_productivity/api", post(post_handler))
            .route("/hen_productivity/api/:id", put(put_handler))
            .with_state(store.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{addr}"), store)
    }

    fn cfg(base: &str) -> WorkerConfig {
        cfg_dialect(base, QueryDialect::EntityNamed)
    }

    fn cfg_dialect(base: &str, dialect: QueryDialect) -> WorkerConfig {
        let base = base.to_string();
        let mut c = WorkerConfig::from_lookup(move |k| match k {
            "SOURCE_GRAPHQL_URL" | "TARGET_REST_URL" | "TARGET_GRAPHQL_URL" => {
                Some(base.to_string())
            }
            _ => None,
        });
        c.query_dialect = dialect;
        c
    }

    #[tokio::test]
    async fn get_current_returns_none_when_the_hen_has_no_record_yet() {
        let (base, _store) = start().await;
        let client = reqwest::Client::new();
        let result = get_current(&client, &cfg(&base), "hen-1").await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn get_current_uses_the_generic_dialect_query_name_when_configured() {
        // Java's and TS's farm retrofits expose getByHen, not
        // getHenProductivityByHen — see the reconciliation note at the top
        // of this plan.
        let (base, _store) = start_generic().await;
        let client = reqwest::Client::new();
        let c = cfg_dialect(&base, QueryDialect::Generic);
        let result = get_current(&client, &c, "hen-1").await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn write_posts_when_the_hen_has_no_known_id_then_a_later_write_puts() {
        let (base, store) = start().await;
        let client = reqwest::Client::new();
        let c = cfg(&base);

        let first = HenProductivity {
            id: None,
            hen_id: "hen-1".to_string(),
            total_eggs: 3,
            last_laid_at: "2026-07-22T08:00:00Z".to_string(),
        };
        write(&client, &c, &first).await.unwrap();
        let stored = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(stored.id, Some("hp-generated".to_string()));
        assert_eq!(stored.total_eggs, 3);

        // Discover the id the way the worker's own loop would, then PUT.
        let discovered = get_current(&client, &c, "hen-1").await.unwrap().unwrap();
        let second = HenProductivity {
            total_eggs: 5,
            ..discovered
        };
        write(&client, &c, &second).await.unwrap();
        let stored = store.0.lock().unwrap().clone().unwrap();
        assert_eq!(
            stored.id,
            Some("hp-generated".to_string()),
            "PUT must keep the same id"
        );
        assert_eq!(stored.total_eggs, 5);
    }
}
