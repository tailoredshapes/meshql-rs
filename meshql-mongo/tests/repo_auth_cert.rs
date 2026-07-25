mod common;

use common::{fresh_collection, shared_mongo};
use meshql_core::testing as cert;
use meshql_core::NoAuth;
use meshql_mongo::MongoRepository;
use std::sync::Arc;

async fn create_repo() -> (MongoRepository, impl std::any::Any) {
    let node = shared_mongo().await;
    let collection_name = fresh_collection();
    let repo = MongoRepository::new(&node.uri, "test_db", &collection_name, Arc::new(NoAuth))
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
