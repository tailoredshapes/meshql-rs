//! The auth plugin decides, and nothing else does — MongoDB adapter.
//! See `meshql-cert/tests/features/auth_plugin.feature`.
//!
//! Unlike `authz_cert`, the before-hook stands up storage only. The server
//! comes up in the `Given` step, because each scenario names its own plugin —
//! and they are plugins no adapter can second-guess, which is the whole point
//! of this suite: an adapter passes only if every surface actually asks.

use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::authz;
use meshql_cert::CertWorld;
use meshql_core::NoAuth;
use meshql_mongo::{MongoRepository, MongoSearcher};
use std::sync::Arc;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mongo::Mongo;

#[tokio::main]
async fn main() {
    let container = Mongo::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(27017).await.unwrap();
    let uri = format!("mongodb://127.0.0.1:{port}");

    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(move |_feature, _rule, _scenario, world| {
            let uri = uri.clone();
            Box::pin(async move {
                // The `Auth` these constructors still take is vestigial: the
                // adapter holds no credentials and answers no authorization
                // question, it asks the session it is handed per call. Passing
                // `NoAuth` here is the proof — the plugin that decides is the
                // one the scenario names, and it reaches storage another way.
                let db = format!("plugin_{}", uuid::Uuid::new_v4().simple());

                let repo = Arc::new(
                    MongoRepository::new(&uri, &db, "widgets", Arc::new(NoAuth))
                        .await
                        .unwrap(),
                );
                let searcher = Arc::new(
                    MongoSearcher::new(&uri, &db, "widgets", Arc::new(NoAuth))
                        .await
                        .unwrap(),
                );

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

    drop(container);
}
