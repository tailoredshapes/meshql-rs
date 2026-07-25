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
    cert::seed_searcher_result_shape_data(&repo).await;
    (repo, searcher, dir)
}

#[tokio::test]
async fn result_carries_id_and_created_at() {
    let (_repo, searcher, _dir) = create_searcher().await;
    cert::test_searcher_result_carries_id_and_created_at(&searcher).await;
}
