use merkql::broker::{Broker, BrokerConfig};
use meshql_core::testing as cert;
use meshql_merkql::{MerkqlRepository, MerkqlSearcher};

async fn create_searcher() -> (MerkqlRepository, MerkqlSearcher, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = BrokerConfig::new(dir.path());
    let broker = Broker::open(config).unwrap();
    let topic = format!("cert_{}", uuid::Uuid::new_v4().simple());
    let repo = MerkqlRepository::new(broker.clone(), &topic);
    let searcher = MerkqlSearcher::new(broker, &topic);
    cert::seed_searcher_result_shape_data(&repo).await;
    (repo, searcher, dir)
}

#[tokio::test]
async fn result_carries_id_and_created_at() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_result_carries_id_and_created_at(&searcher).await;
}
