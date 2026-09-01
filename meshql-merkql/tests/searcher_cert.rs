use cucumber::World as _;
use merkql::broker::{Broker, BrokerConfig};
#[allow(unused_imports)]
use meshql_cert::steps::searcher;
use meshql_cert::CertWorld;
use meshql_merkql::{MerkqlRepository, MerkqlSearcher};
use std::sync::Arc;

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
                let repo = Arc::new(MerkqlRepository::new(broker.clone(), &topic));
                let searcher = Arc::new(MerkqlSearcher::new(broker, &topic));
                world.set_repo(repo);
                world.set_searcher(searcher);
            })
        }) // A scenario whose steps do not match is *skipped*, and cucumber
        // exits 0 on a skip. Without this, a suite where nothing ran at all
        // reports success — which is how a diverged feature file went
        // unnoticed for months.
        .fail_on_skipped()
        .run_and_exit("../meshql-cert/tests/features/searcher.feature")
        .await;
}
