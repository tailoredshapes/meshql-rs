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
    cert::seed_searcher_auth_data(&repo).await;
    cert::seed_searcher_ordering_data(&repo).await;
    cert::seed_searcher_result_shape_data(&repo).await;
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

#[tokio::test]
async fn auth_wildcard_caller_sees_all() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_auth_wildcard_caller_sees_all(&searcher).await;
}

#[tokio::test]
async fn auth_restricted_caller_sees_only_intersecting() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_auth_restricted_caller_sees_only_intersecting(&searcher).await;
}

#[tokio::test]
async fn auth_denies_non_intersecting() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_auth_denies_non_intersecting(&searcher).await;
}

#[tokio::test]
async fn auth_empty_tokens_are_public() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_auth_empty_tokens_are_public(&searcher).await;
}

#[tokio::test]
async fn auth_star_token_visible_to_all() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_auth_star_token_visible_to_all(&searcher).await;
}

#[tokio::test]
async fn auth_latest_version_controls_visibility() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_auth_latest_version_controls_visibility(&searcher).await;
}

#[tokio::test]
async fn ordering_limit_truncates_in_insertion_order() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_ordering_limit_truncates_in_insertion_order(&searcher).await;
}

#[tokio::test]
async fn ordering_is_stable_across_repeated_queries() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_ordering_is_stable_across_repeated_queries(&searcher).await;
}

#[tokio::test]
async fn ordering_uses_resolved_version_position() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_ordering_uses_resolved_version_position(&searcher).await;
}

#[tokio::test]
async fn ordering_breaks_millisecond_ties_by_id() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_ordering_breaks_millisecond_ties_by_id(&searcher).await;
}

#[tokio::test]
async fn ordering_as_of_uses_version_resolved_at_cutoff() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_ordering_as_of_uses_version_resolved_at_cutoff(&searcher).await;
}

#[tokio::test]
async fn result_carries_id_and_created_at() {
    let (_repo, searcher) = create_searcher().await;
    cert::test_searcher_result_carries_id_and_created_at(&searcher).await;
}
