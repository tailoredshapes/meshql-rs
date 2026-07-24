use merkql::broker::{Broker, BrokerConfig};
use merksql::MerkSql;
use meshql_core::testing as cert;
use meshql_merksql::MerksqlRepository;
use std::sync::{Arc, Mutex};

async fn create_repo() -> (MerksqlRepository, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = BrokerConfig::new(dir.path());
    let broker = Broker::open(config).unwrap();
    let topic = format!("cert_{}", uuid::Uuid::new_v4().simple());
    let merksql = Arc::new(Mutex::new(MerkSql::new(broker.clone())));
    let repo = MerksqlRepository::new(broker, &topic, merksql);
    cert::seed_repository_auth_data(&repo).await;
    (repo, dir)
}

#[tokio::test]
async fn auth_wildcard_caller_sees_all() {
    let (repo, _dir) = create_repo().await;
    cert::test_repository_auth_wildcard_caller_sees_all(&repo).await;
}

#[tokio::test]
async fn auth_restricted_caller_sees_only_intersecting() {
    let (repo, _dir) = create_repo().await;
    cert::test_repository_auth_restricted_caller_sees_only_intersecting(&repo).await;
}

#[tokio::test]
async fn auth_denies_non_intersecting() {
    let (repo, _dir) = create_repo().await;
    cert::test_repository_auth_denies_non_intersecting(&repo).await;
}

#[tokio::test]
async fn auth_empty_tokens_are_public() {
    let (repo, _dir) = create_repo().await;
    cert::test_repository_auth_empty_tokens_are_public(&repo).await;
}

#[tokio::test]
async fn auth_star_token_visible_to_all() {
    let (repo, _dir) = create_repo().await;
    cert::test_repository_auth_star_token_visible_to_all(&repo).await;
}

#[tokio::test]
async fn auth_latest_version_controls_visibility() {
    let (repo, _dir) = create_repo().await;
    cert::test_repository_auth_latest_version_controls_visibility(&repo).await;
}
