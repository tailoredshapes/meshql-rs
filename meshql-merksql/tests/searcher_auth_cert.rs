//! Searcher-level authorization certification, merksql adapter.
//!
//! Every other adapter with a Searcher already ran this; merksql was the one
//! that did not, and that is precisely where the token filter had been left
//! out of `find`/`find_all` entirely. Wired in so the gap cannot reopen.

use merkql::broker::{Broker, BrokerConfig};
use merksql::MerkSql;
use meshql_core::testing as cert;
use meshql_merksql::{MerksqlRepository, MerksqlSearcher};
use std::sync::{Arc, Mutex};

async fn create_searcher() -> (MerksqlRepository, MerksqlSearcher, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = BrokerConfig::new(dir.path());
    let broker = Broker::open(config).unwrap();
    let topic = format!("cert_{}", uuid::Uuid::new_v4().simple());
    let merksql = Arc::new(Mutex::new(MerkSql::new(broker.clone())));
    let repo = MerksqlRepository::new(broker.clone(), &topic, merksql.clone());
    let searcher = MerksqlSearcher::new(broker, &topic, merksql);
    cert::seed_searcher_auth_data(&repo).await;
    (repo, searcher, dir)
}

#[tokio::test]
async fn auth_wildcard_caller_sees_all() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_auth_wildcard_caller_sees_all(&searcher).await;
}

#[tokio::test]
async fn auth_restricted_caller_sees_only_intersecting() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_auth_restricted_caller_sees_only_intersecting(&searcher).await;
}

#[tokio::test]
async fn auth_denies_non_intersecting() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_auth_denies_non_intersecting(&searcher).await;
}

#[tokio::test]
async fn auth_empty_tokens_are_public() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_auth_empty_tokens_are_public(&searcher).await;
}

#[tokio::test]
async fn auth_star_token_visible_to_all() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_auth_star_token_visible_to_all(&searcher).await;
}

#[tokio::test]
async fn auth_latest_version_controls_visibility() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_auth_latest_version_controls_visibility(&searcher).await;
}
