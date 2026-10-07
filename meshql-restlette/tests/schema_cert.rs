//! A restlette enforces its JSON Schema on every write, over HTTP, as
//! meshql-java and meshobj do: a document that does not conform is refused
//! with 400 and nothing is stored; an update is checked as the document it
//! would store.

use meshql_core::NoAuth;
use meshql_restlette::{build_restlette_router_ext, schema_validator};
use meshql_sqlite::SqliteRepository;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

fn hen_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name"],
        "properties": {
            "name": {"type": "string", "minLength": 1},
            "eggs": {"type": "integer", "minimum": 0},
            "laid": {"type": "string", "format": "date"}
        }
    })
}

async fn spawn_server(schema: Value) -> String {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    let repo = Arc::new(SqliteRepository::new_with_pool(pool).await.unwrap());
    let validator = schema_validator(&schema).expect("the schema compiles");
    let router =
        build_restlette_router_ext("/hens", repo, Arc::new(NoAuth), None, validator, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

async fn send(method: reqwest::Method, url: String, body: Option<Value>) -> (u16, Value) {
    let mut req = reqwest::Client::new().request(method, url);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn count(addr: &str) -> usize {
    send(reqwest::Method::GET, format!("{addr}/hens"), None)
        .await
        .1
        .as_array()
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_conforming_document_is_created() {
    let addr = spawn_server(hen_schema()).await;
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{addr}/hens"),
        Some(json!({"name": "chuck", "eggs": 2, "laid": "2026-10-07"})),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(count(&addr).await, 1);
}

#[tokio::test]
async fn a_create_that_does_not_conform_is_refused_and_nothing_is_stored() {
    let addr = spawn_server(hen_schema()).await;
    for (doc, named) in [
        (json!({"eggs": 2}), "name"),
        (json!({"name": "chuck", "eggs": "three"}), "/eggs"),
        (json!({"name": "chuck", "eggs": -1}), "/eggs"),
        (json!({"name": "chuck", "laid": "yesterday"}), "/laid"),
    ] {
        let (status, body) = send(
            reqwest::Method::POST,
            format!("{addr}/hens"),
            Some(doc.clone()),
        )
        .await;
        assert_eq!(status, 400, "{doc} must be refused: {body}");
        let error = body["error"].as_str().unwrap_or_default();
        assert!(error.starts_with("Invalid payload"), "{doc}: {body}");
        assert!(
            error.contains(named),
            "{doc}: the violation is named ({named}): {body}"
        );
    }
    assert_eq!(
        count(&addr).await,
        0,
        "a refused document must not be stored"
    );
}

#[tokio::test]
async fn an_update_is_checked_as_the_document_it_would_store() {
    let addr = spawn_server(hen_schema()).await;
    let (_, created) = send(
        reqwest::Method::POST,
        format!("{addr}/hens"),
        Some(json!({"name": "chuck", "eggs": 2})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    // A partial body that keeps the merged document valid is accepted.
    let (status, body) = send(
        reqwest::Method::PUT,
        format!("{addr}/hens/{id}"),
        Some(json!({"eggs": 5})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["name"], "chuck");

    // One that would break it is refused, and the stored version is untouched.
    let (status, body) = send(
        reqwest::Method::PUT,
        format!("{addr}/hens/{id}"),
        Some(json!({"eggs": -3})),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    let (_, current) = send(reqwest::Method::GET, format!("{addr}/hens/{id}"), None).await;
    assert_eq!(current["eggs"], 5);
    let (_, versions) = send(
        reqwest::Method::GET,
        format!("{addr}/hens/{id}/versions"),
        None,
    )
    .await;
    assert_eq!(
        versions["versions"].as_array().unwrap().len(),
        2,
        "the refused update wrote no version"
    );
}

#[tokio::test]
async fn an_empty_schema_accepts_any_document() {
    let addr = spawn_server(json!({})).await;
    let (status, _) = send(
        reqwest::Method::POST,
        format!("{addr}/hens"),
        Some(json!({"anything": [1, {"at": "all"}]})),
    )
    .await;
    assert_eq!(status, 201);
}
