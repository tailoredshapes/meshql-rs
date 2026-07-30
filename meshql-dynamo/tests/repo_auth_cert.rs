//! Repository *authorization* certification, DynamoDB adapter.
//!
//! The rest of the repository suite creates every envelope with `"*"` tokens, so
//! an adapter that ignored `tokens` entirely would pass it. These close that gap.

mod common;

use common::RepoFixture;
use meshql_core::testing as cert;

#[tokio::test]
async fn auth_wildcard_caller_sees_all() {
    let f = RepoFixture::seeded_repo_auth().await;
    cert::test_repository_auth_wildcard_caller_sees_all(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_restricted_caller_sees_only_intersecting() {
    let f = RepoFixture::seeded_repo_auth().await;
    cert::test_repository_auth_restricted_caller_sees_only_intersecting(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_denies_non_intersecting() {
    let f = RepoFixture::seeded_repo_auth().await;
    cert::test_repository_auth_denies_non_intersecting(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_empty_tokens_are_public() {
    let f = RepoFixture::seeded_repo_auth().await;
    cert::test_repository_auth_empty_tokens_are_public(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_star_token_visible_to_all() {
    let f = RepoFixture::seeded_repo_auth().await;
    cert::test_repository_auth_star_token_visible_to_all(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_latest_version_controls_visibility() {
    let f = RepoFixture::seeded_repo_auth().await;
    cert::test_repository_auth_latest_version_controls_visibility(&f.repo).await;
    f.cleanup().await;
}
