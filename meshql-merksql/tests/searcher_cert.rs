use cucumber::World as _;
use merkql::broker::{Broker, BrokerConfig};
use merksql::MerkSql;
#[allow(unused_imports)]
use meshql_cert::steps::searcher;
use meshql_cert::CertWorld;
use meshql_merksql::{MerksqlRepository, MerksqlSearcher};
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(|_feature, _rule, _scenario, world| {
            Box::pin(async move {
                let dir = tempfile::tempdir().unwrap();
                let dir = Box::new(dir);
                let dir_ref = Box::leak(dir);
                let config = BrokerConfig::new(dir_ref.path());
                let broker = Broker::open(config).unwrap();
                let topic = format!("cert_{}", uuid::Uuid::new_v4().simple());
                let merksql = Arc::new(Mutex::new(MerkSql::new(broker.clone())));
                let repo = Arc::new(MerksqlRepository::new(
                    broker.clone(),
                    &topic,
                    merksql.clone(),
                ));
                let searcher = Arc::new(MerksqlSearcher::new(broker, &topic, merksql));
                world.set_repo(repo);
                world.set_searcher(searcher);
            })
        })
        .run_and_exit("../meshql-cert/tests/features/searcher.feature")
        .await;
}
