use meshql_core::testing as cert;
use meshql_sqlite::{SqliteRepository, SqliteSearcher};

async fn create_searcher() -> (SqliteRepository, SqliteSearcher) {
    let name = uuid::Uuid::new_v4().simple().to_string();
    let url = format!("sqlite:file:{}?mode=memory&cache=shared&uri=true", name);
    let repo = SqliteRepository::new(&url).await.unwrap();
    let searcher = SqliteSearcher::new(&url).await.unwrap();
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
