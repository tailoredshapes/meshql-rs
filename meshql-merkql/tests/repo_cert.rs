use cucumber::World as _;
use merkql::broker::{Broker, BrokerConfig};
#[allow(unused_imports)]
use meshql_cert::steps::repo;
use meshql_cert::CertWorld;
use meshql_merkql::MerkqlRepository;
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
                let repo = MerkqlRepository::new(broker, &topic);
                world.set_repo(Arc::new(repo));
            })
        }) // A scenario whose steps do not match is *skipped*, and cucumber
        // exits 0 on a skip. Without this, a suite where nothing ran at all
        // reports success — which is how a diverged feature file went
        // unnoticed for months.
        .fail_on_skipped()
        .run_and_exit("../meshql-cert/tests/features/repository.feature")
        .await;
}
