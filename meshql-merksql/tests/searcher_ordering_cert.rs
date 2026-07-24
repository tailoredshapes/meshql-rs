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
    cert::seed_searcher_ordering_data(&repo).await;
    (repo, searcher, dir)
}

#[tokio::test]
async fn ordering_limit_truncates_in_insertion_order() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_ordering_limit_truncates_in_insertion_order(&searcher).await;
}

#[tokio::test]
async fn ordering_is_stable_across_repeated_queries() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_ordering_is_stable_across_repeated_queries(&searcher).await;
}

#[tokio::test]
async fn ordering_uses_resolved_version_position() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_ordering_uses_resolved_version_position(&searcher).await;
}

#[tokio::test]
async fn ordering_breaks_millisecond_ties_by_id() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_ordering_breaks_millisecond_ties_by_id(&searcher).await;
}

#[tokio::test]
async fn ordering_as_of_uses_version_resolved_at_cutoff() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_ordering_as_of_uses_version_resolved_at_cutoff(&searcher).await;
}
