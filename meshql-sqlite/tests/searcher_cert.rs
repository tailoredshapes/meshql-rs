use meshql_core::testing as cert;
use meshql_sqlite::{SqliteRepository, SqliteSearcher};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

async fn create_searcher() -> (SqliteRepository, SqliteSearcher) {
    // Use a single pool shared by both repo and searcher so they see the same in-memory DB
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();

    let repo = SqliteRepository::new_with_pool(pool.clone()).await.unwrap();
    let searcher = SqliteSearcher::new_with_pool(pool).await.unwrap();
    cert::seed_searcher_data(&repo).await;
    (repo, searcher)
}

#[tokio::test]
async fn should_return_empty_for_nonexistent_id() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_empty_result_for_nonexistent(&searcher).await;
}

#[tokio::test]
async fn should_find_by_id() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_find_by_id(&searcher).await;
}

#[tokio::test]
async fn should_find_by_name() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_find_by_name(&searcher).await;
}

#[tokio::test]
async fn should_find_all_by_type() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_find_all_by_type(&searcher).await;
}

#[tokio::test]
async fn should_find_all_by_type_and_name() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_find_all_by_type_and_name(&searcher).await;
}

#[tokio::test]
async fn should_return_empty_for_nonexistent_type() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_empty_array_for_nonexistent_type(&searcher).await;
}

#[tokio::test]
async fn should_respect_limit() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_respects_limit(&searcher).await;
}

#[tokio::test]
async fn should_handle_empty_query() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_empty_query(&searcher).await;
}
