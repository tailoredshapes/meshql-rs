//! The auth plugin decides, and nothing else does — merkql adapter.
//! See `meshql-cert/tests/features/auth_plugin.feature`.
//!
//! Unlike `authz_cert`, the before-hook stands up storage only. The server
//! comes up in the `Given` step, because each scenario names its own plugin —
//! and they are plugins no adapter can second-guess, which is the whole point
//! of this suite: an adapter passes only if every surface actually asks.

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
                let topic = format!("plugin_{}", uuid::Uuid::new_v4().simple());

                let repo = Arc::new(MerkqlRepository::new(broker.clone(), &topic));
                let searcher = Arc::new(MerkqlSearcher::new(broker, &topic));

                world.set_repo(repo);
                world.set_searcher(searcher);
                world.reset_authz();
            })
        }) // A scenario whose steps do not match is *skipped*, and cucumber
        // exits 0 on a skip. Without this, a suite where nothing ran at all
        // reports success — which is how a diverged feature file went
        // unnoticed for months.
        .fail_on_skipped()
        .run_and_exit("../meshql-cert/tests/features/auth_plugin.feature")
        .await;
}
