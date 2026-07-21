//! SearcherTail certification against in-memory sqlite. The repo and
//! searcher MUST share one single-connection pool — each `sqlite::memory:`
//! connection is its own private database.

use meshql_changes::testing as cert;
use meshql_changes::SearcherTail;
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use std::str::FromStr;
use std::sync::Arc;

async fn setup() -> (SearcherTail, Arc<SqliteRepository>) {
    // max_connections(1): each sqlite::memory: connection is its own DB.
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    let repo = Arc::new(SqliteRepository::new_with_pool(pool.clone()).await.unwrap());
    let searcher = Arc::new(SqliteSearcher::new_with_pool(pool).await.unwrap());
    let tail = SearcherTail::new("hen", searcher, repo.clone());
    (tail, repo)
}

#[tokio::test]
async fn detects_create() {
    let (tail, repo) = setup().await;
    cert::test_detects_create(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn detects_update() {
    let (tail, repo) = setup().await;
    cert::test_detects_update(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn ignores_identical_rewrite() {
    let (tail, repo) = setup().await;
    cert::test_ignores_identical_rewrite(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn detects_delete() {
    let (tail, repo) = setup().await;
    cert::test_detects_delete(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn update_then_delete_between_polls() {
    let (tail, repo) = setup().await;
    cert::test_update_then_delete_between_polls(&tail, repo.as_ref()).await;
}

#[tokio::test]
async fn quiet_store_emits_nothing() {
    let (tail, repo) = setup().await;
    cert::test_quiet_store_emits_nothing(&tail, repo.as_ref()).await;
}
