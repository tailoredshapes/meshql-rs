//! End-to-end authorization certification, MongoDB adapter.
//! See `meshql-cert/tests/features/authz.feature`.

use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::authz;
use meshql_cert::CertWorld;
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
                // The adapter takes the `Auth` too — hand it the same one the
                // server runs on, not `NoAuth`, so nothing downstream quietly
                // resolves every caller to the wildcard token.
                let auth = meshql_cert::authz::edge_auth();
                let db = format!("authz_{}", uuid::Uuid::new_v4().simple());

                let repo = Arc::new(
                    MongoRepository::new(&uri, &db, "widgets", Arc::clone(&auth))
                        .await
                        .unwrap(),
                );
                let searcher = Arc::new(
                    MongoSearcher::new(&uri, &db, "widgets", auth)
                        .await
                        .unwrap(),
                );

                let addr = meshql_cert::authz::start_server(repo.clone(), searcher).await;
                world.server_addr = Some(addr);
                world.set_repo(repo);
                world.reset_authz();
            })
        }) // A scenario whose steps do not match is *skipped*, and cucumber
        // exits 0 on a skip. Without this, a suite where nothing ran at all
        // reports success — which is how a diverged feature file went
        // unnoticed for months.
        .fail_on_skipped()
        .run_and_exit("../meshql-cert/tests/features/authz.feature")
        .await;

    drop(container);
}
