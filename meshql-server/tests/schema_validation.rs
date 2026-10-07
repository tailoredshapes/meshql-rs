//! `build_app` enforces each restlette's `schema_json`. It used to mount every
//! restlette without it, so a deployment's JSON Schemas were read and ignored;
//! meshql-java and meshobj refuse a document that does not conform.

use meshql_core::{RestletteConfig, ServerConfig};
use meshql_server::build_app;
use meshql_sqlite::SqliteRepository;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

async fn repo() -> Arc<SqliteRepository> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    Arc::new(SqliteRepository::new_with_pool(pool).await.unwrap())
}

fn config(schema: serde_json::Value, repository: Arc<SqliteRepository>) -> ServerConfig {
    ServerConfig {
        port: 0,
        graphlettes: vec![],
        restlettes: vec![RestletteConfig {
            path: "/farm/api".into(),
            schema_json: schema,
            repository,
        }],
    }
}

#[tokio::test]
async fn a_deployment_enforces_its_restlettes_schemas() {
    let schema =
        json!({"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}});
    let app = build_app(config(schema, repo().await))
        .await
        .expect("the app builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();

    let bad = client
        .post(format!("{addr}/farm/api"))
        .json(&json!({"acres": 40}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        bad.status().as_u16(),
        400,
        "a farm without a name must be refused"
    );
    let good = client
        .post(format!("{addr}/farm/api"))
        .json(&json!({"name": "Emmerdale"}))
        .send()
        .await
        .unwrap();
    assert_eq!(good.status().as_u16(), 201);
    let listed: serde_json::Value = client
        .get(format!("{addr}/farm/api"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        listed.as_array().unwrap().len(),
        1,
        "only the conforming farm was stored"
    );
}

#[tokio::test]
async fn a_schema_that_does_not_compile_stops_the_build() {
    let err = match build_app(config(json!({"type": 5}), repo().await)).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a restlette whose schema does not compile must not be served"),
    };
    assert!(
        err.contains("/farm/api"),
        "the error names the restlette: {err}"
    );
}
