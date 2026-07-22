//! Proves create/update/delete each pass their own verb as the
//! authorize_action string, not one shared "write" — the change the
//! farm-event-sourcing-retrofit spec's Auth section requires so a
//! Casbin policy can express "create allowed, update/delete denied"
//! (lay_report's new create-only contract).

use meshql_core::{Auth, Envelope, Stash};
use meshql_restlette::build_restlette_router;
use meshql_sqlite::SqliteRepository;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

/// Denies every action except "create" — if create/update/delete all
/// passed the same "write" string, this would make create ALSO fail
/// (since "write" != "create"), so a passing create + failing
/// update/delete proves the three handlers pass distinct strings.
struct CreateOnlyAuth;
impl Auth for CreateOnlyAuth {
    fn get_auth_token(&self, _context: &Stash) -> Vec<String> {
        vec![]
    }
    fn is_authorized(&self, _credentials: &[String], _envelope: &Envelope) -> bool {
        true
    }
    fn authorize_action(&self, _credentials: &[String], action: &str) -> bool {
        action == "create"
    }
}

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
    let router = build_restlette_router("/widgets", repo, Arc::new(CreateOnlyAuth));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn create_succeeds_update_and_delete_are_denied() {
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

    let update_resp = client
        .put(format!("{addr}/widgets/{id}"))
        .json(&serde_json::json!({"name": "sprocket-v2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        update_resp.status(),
        403,
        "update must pass \"update\", not \"create\""
    );

    let delete_resp = client
        .delete(format!("{addr}/widgets/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        delete_resp.status(),
        403,
        "delete must pass \"delete\", not \"create\""
    );
}
