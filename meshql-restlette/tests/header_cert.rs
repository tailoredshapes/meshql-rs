use meshql_core::NoAuth;
use meshql_restlette::build_restlette_router;
use meshql_sqlite::SqliteRepository;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

async fn spawn_server() -> String {
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
    let router = build_restlette_router("/widgets", repo, Arc::new(NoAuth));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

fn assert_honesty_headers(resp: &reqwest::Response, expect_deleted: &str) {
    let created_at = resp
        .headers()
        .get("x-meshql-created-at")
        .expect("x-meshql-created-at header must be present")
        .to_str()
        .unwrap();
    chrono::DateTime::parse_from_rfc3339(created_at)
        .expect("x-meshql-created-at must be a valid RFC3339 timestamp");

    let deleted = resp
        .headers()
        .get("x-meshql-deleted")
        .expect("x-meshql-deleted header must be present")
        .to_str()
        .unwrap();
    assert_eq!(deleted, expect_deleted);
}

#[tokio::test]
async fn create_response_carries_honesty_headers() {
    let addr = spawn_server().await;
    let resp = reqwest::Client::new()
        .post(format!("{addr}/widgets"))
        .json(&serde_json::json!({"name": "sprocket"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    assert_honesty_headers(&resp, "false");
}

#[tokio::test]
async fn read_response_carries_honesty_headers() {
    let addr = spawn_server().await;
    let client = reqwest::Client::new();
    let created: serde_json::Value = client
        .post(format!("{addr}/widgets"))
        .json(&serde_json::json!({"name": "sprocket"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client
        .get(format!("{addr}/widgets/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_honesty_headers(&resp, "false");
}

#[tokio::test]
async fn update_response_carries_a_fresh_created_at() {
    let addr = spawn_server().await;
    let client = reqwest::Client::new();
    let created: serde_json::Value = client
        .post(format!("{addr}/widgets"))
        .json(&serde_json::json!({"name": "sprocket"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let create_resp = client
        .get(format!("{addr}/widgets/{id}"))
        .send()
        .await
        .unwrap();
    let original_created_at = create_resp
        .headers()
        .get("x-meshql-created-at")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let update_resp = client
        .put(format!("{addr}/widgets/{id}"))
        .json(&serde_json::json!({"name": "sprocket-v2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(update_resp.status(), 200);
    assert_honesty_headers(&update_resp, "false");
    let updated_created_at = update_resp
        .headers()
        .get("x-meshql-created-at")
        .unwrap()
        .to_str()
        .unwrap();
    assert_ne!(
        original_created_at, updated_created_at,
        "an update creates a new version, so its as-of timestamp must move forward"
    );
}
