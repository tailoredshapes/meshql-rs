use meshql_core::testing as cert;
use meshql_postgres::{PostgresRepository, PostgresSearcher};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

async fn create_searcher() -> (PostgresSearcher, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let table = format!("env_{}", uuid::Uuid::new_v4().simple());
    let repo = PostgresRepository::new_with_table(&url, &table).await.unwrap();
    cert::seed_searcher_data(&repo).await;
    let searcher = PostgresSearcher::new_with_table(&url, &table).await.unwrap();
    (searcher, container)
}

#[tokio::test]
async fn should_return_empty_for_nonexistent_id() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_empty_result_for_nonexistent(&searcher).await;
}

#[tokio::test]
async fn should_find_by_id() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_find_by_id(&searcher).await;
}

#[tokio::test]
async fn should_find_by_name() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_find_by_name(&searcher).await;
}

#[tokio::test]
async fn should_find_all_by_type() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_find_all_by_type(&searcher).await;
}

#[tokio::test]
async fn should_find_all_by_type_and_name() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_find_all_by_type_and_name(&searcher).await;
}

#[tokio::test]
async fn should_return_empty_for_nonexistent_type() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_empty_array_for_nonexistent_type(&searcher).await;
}

#[tokio::test]
async fn should_respect_limit() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_respects_limit(&searcher).await;
}

#[tokio::test]
async fn should_handle_empty_query() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_empty_query(&searcher).await;
}
