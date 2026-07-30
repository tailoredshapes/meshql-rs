//! Repository certification, DynamoDB adapter.
//! Requires DynamoDB Local at `MESHQL_DYNAMO_ENDPOINT` (default
//! `http://localhost:8123`).

mod common;

use common::RepoFixture;
use meshql_core::testing as cert;

#[tokio::test]
async fn create_should_store_and_return_envelope() {
    let f = RepoFixture::new().await;
    cert::test_create_should_store_and_return_envelope(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn read_should_retrieve_existing_envelope() {
    let f = RepoFixture::new().await;
    cert::test_read_should_retrieve_existing_envelope(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn list_should_retrieve_all_created_envelopes() {
    let f = RepoFixture::new().await;
    cert::test_list_should_retrieve_all_created_envelopes(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn remove_should_delete_envelope() {
    let f = RepoFixture::new().await;
    cert::test_remove_should_delete_envelope(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn create_many_should_store_multiple_envelopes() {
    let f = RepoFixture::new().await;
    cert::test_create_many_should_store_multiple_envelopes(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn read_many_should_retrieve_multiple_envelopes() {
    let f = RepoFixture::new().await;
    cert::test_read_many_should_retrieve_multiple_envelopes(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn remove_many_should_delete_multiple_envelopes() {
    let f = RepoFixture::new().await;
    cert::test_remove_many_should_delete_multiple_envelopes(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_allow_multiple_versions_and_temporal_reads() {
    let f = RepoFixture::new().await;
    cert::test_temporal_versioning(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_only_list_latest_version() {
    let f = RepoFixture::new().await;
    cert::test_list_shows_only_latest_version(&f.repo).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_not_list_deleted_envelope_with_prior_version() {
    let f = RepoFixture::new().await;
    cert::test_list_excludes_deleted_envelope_with_prior_version(&f.repo).await;
    f.cleanup().await;
}
