mod common;

use common::{fresh_table, shared_mysql};
use meshql_core::testing as cert;
use meshql_mysql::MysqlRepository;

async fn create_repo() -> (MysqlRepository, impl std::any::Any) {
    let node = shared_mysql().await;
    let table = fresh_table();
    let repo = MysqlRepository::new_with_table(&node.url, &table)
        .await
        .unwrap();
    cert::seed_repository_auth_data(&repo).await;
    (repo, node)
}

#[tokio::test]
async fn auth_wildcard_caller_sees_all() {
    let (repo, _c) = create_repo().await;
    cert::test_repository_auth_wildcard_caller_sees_all(&repo).await;
}

#[tokio::test]
async fn auth_restricted_caller_sees_only_intersecting() {
    let (repo, _c) = create_repo().await;
    cert::test_repository_auth_restricted_caller_sees_only_intersecting(&repo).await;
}

#[tokio::test]
async fn auth_denies_non_intersecting() {
    let (repo, _c) = create_repo().await;
    cert::test_repository_auth_denies_non_intersecting(&repo).await;
}

#[tokio::test]
async fn auth_empty_tokens_are_public() {
    let (repo, _c) = create_repo().await;
    cert::test_repository_auth_empty_tokens_are_public(&repo).await;
}

#[tokio::test]
async fn auth_star_token_visible_to_all() {
    let (repo, _c) = create_repo().await;
    cert::test_repository_auth_star_token_visible_to_all(&repo).await;
}

#[tokio::test]
async fn auth_latest_version_controls_visibility() {
    let (repo, _c) = create_repo().await;
    cert::test_repository_auth_latest_version_controls_visibility(&repo).await;
}
