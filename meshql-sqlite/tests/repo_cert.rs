use meshql_core::testing as cert;
use meshql_sqlite::SqliteRepository;

async fn create_repo() -> SqliteRepository {
    SqliteRepository::new("sqlite::memory:").await.unwrap()
}

#[tokio::test]
async fn create_should_store_and_return_envelope() {
    let repo = create_repo().await;
    cert::test_create_should_store_and_return_envelope(&repo).await;
}

#[tokio::test]
async fn read_should_retrieve_existing_envelope() {
    let repo = create_repo().await;
    cert::test_read_should_retrieve_existing_envelope(&repo).await;
}

#[tokio::test]
async fn list_should_retrieve_all_created_envelopes() {
    let repo = create_repo().await;
    cert::test_list_should_retrieve_all_created_envelopes(&repo).await;
}

#[tokio::test]
async fn remove_should_delete_envelope() {
    let repo = create_repo().await;
    cert::test_remove_should_delete_envelope(&repo).await;
}

#[tokio::test]
async fn create_many_should_store_multiple_envelopes() {
    let repo = create_repo().await;
    cert::test_create_many_should_store_multiple_envelopes(&repo).await;
}

#[tokio::test]
async fn read_many_should_retrieve_multiple_envelopes() {
    let repo = create_repo().await;
    cert::test_read_many_should_retrieve_multiple_envelopes(&repo).await;
}

#[tokio::test]
async fn remove_many_should_delete_multiple_envelopes() {
    let repo = create_repo().await;
    cert::test_remove_many_should_delete_multiple_envelopes(&repo).await;
}

#[tokio::test]
async fn should_allow_multiple_versions_and_temporal_reads() {
    let repo = create_repo().await;
    cert::test_temporal_versioning(&repo).await;
}

#[tokio::test]
async fn should_only_list_latest_version() {
    let repo = create_repo().await;
    cert::test_list_shows_only_latest_version(&repo).await;
}

#[tokio::test]
async fn should_not_list_deleted_envelope_with_prior_version() {
    let repo = create_repo().await;
    cert::test_list_excludes_deleted_envelope_with_prior_version(&repo).await;
}

// ---- Versions ----

#[tokio::test]
async fn lists_every_version_oldest_first() {
    let repo = create_repo().await;
    cert::test_lists_every_version_oldest_first(&repo).await;
}

#[tokio::test]
async fn versions_in_one_millisecond_are_distinct() {
    let repo = create_repo().await;
    cert::test_versions_in_one_millisecond_are_distinct(&repo).await;
}

#[tokio::test]
async fn version_listing_is_stable() {
    let repo = create_repo().await;
    cert::test_version_listing_is_stable(&repo).await;
}

#[tokio::test]
async fn a_deletion_appears_in_the_history() {
    let repo = create_repo().await;
    cert::test_a_deletion_appears_in_the_history(&repo).await;
}

#[tokio::test]
async fn an_unreadable_version_is_a_tombstone() {
    let repo = create_repo().await;
    cert::test_an_unreadable_version_is_a_tombstone(&repo).await;
}

#[tokio::test]
async fn unknown_token_absent_unreadable_refused() {
    let repo = create_repo().await;
    cert::test_unknown_token_absent_unreadable_refused(&repo).await;
}
