//! End-to-end authorization certification, merkql adapter.
//! See `meshql-cert/tests/features/authz.feature`.

use cucumber::World as _;
use merkql::broker::{Broker, BrokerConfig};
#[allow(unused_imports)]
use meshql_cert::steps::authz;
use meshql_cert::CertWorld;
use meshql_merkql::{MerkqlRepository, MerkqlSearcher};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(|_feature, _rule, _scenario, world| {
            Box::pin(async move {
                // Leaked on purpose: the TempDir has to outlive the before-hook,
                // or the broker's data directory vanishes under the scenario.
                let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
                let broker = Broker::open(BrokerConfig::new(dir.path())).unwrap();
                let topic = format!("authz_{}", uuid::Uuid::new_v4().simple());

                let repo = Arc::new(MerkqlRepository::new(broker.clone(), &topic));
                let searcher = Arc::new(MerkqlSearcher::new(broker, &topic));

                let addr = meshql_cert::authz::start_server(repo.clone(), searcher).await;
                world.server_addr = Some(addr);
                world.set_repo(repo);
                world.reset_authz();
            })
        })
        .run_and_exit("../meshql-cert/tests/features/authz.feature")
        .await;
}
