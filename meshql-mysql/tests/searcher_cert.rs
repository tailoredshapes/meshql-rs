use meshql_core::testing as cert;
use meshql_mysql::{MysqlRepository, MysqlSearcher};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mysql::Mysql;

async fn create_searcher() -> (MysqlSearcher, impl std::any::Any) {
    let container = Mysql::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(3306).await.unwrap();
    // testcontainers-modules mysql defaults: root with empty password, db = "test"
    let url = format!("mysql://root:@127.0.0.1:{port}/test");
    let table = format!("env_{}", uuid::Uuid::new_v4().simple());

    let repo = MysqlRepository::new_with_table(&url, &table).await.unwrap();
    cert::seed_searcher_data(&repo).await;
    cert::seed_searcher_auth_data(&repo).await;
    cert::seed_searcher_ordering_data(&repo).await;

    let searcher = MysqlSearcher::new_with_table(&url, &table).await.unwrap();
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

#[tokio::test]
async fn auth_wildcard_caller_sees_all() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_auth_wildcard_caller_sees_all(&searcher).await;
}

#[tokio::test]
async fn auth_restricted_caller_sees_only_intersecting() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_auth_restricted_caller_sees_only_intersecting(&searcher).await;
}

#[tokio::test]
async fn auth_denies_non_intersecting() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_auth_denies_non_intersecting(&searcher).await;
}

#[tokio::test]
async fn auth_empty_tokens_are_public() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_auth_empty_tokens_are_public(&searcher).await;
}

#[tokio::test]
async fn auth_star_token_visible_to_all() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_auth_star_token_visible_to_all(&searcher).await;
}

#[tokio::test]
async fn auth_latest_version_controls_visibility() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_auth_latest_version_controls_visibility(&searcher).await;
}

#[tokio::test]
async fn ordering_limit_truncates_in_insertion_order() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_ordering_limit_truncates_in_insertion_order(&searcher).await;
}

#[tokio::test]
async fn ordering_is_stable_across_repeated_queries() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_ordering_is_stable_across_repeated_queries(&searcher).await;
}

#[tokio::test]
async fn ordering_uses_resolved_version_position() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_ordering_uses_resolved_version_position(&searcher).await;
}

#[tokio::test]
async fn ordering_breaks_millisecond_ties_by_id() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_ordering_breaks_millisecond_ties_by_id(&searcher).await;
}

#[tokio::test]
async fn ordering_as_of_uses_version_resolved_at_cutoff() {
    let (searcher, _c) = create_searcher().await;
    cert::test_searcher_ordering_as_of_uses_version_resolved_at_cutoff(&searcher).await;
}
