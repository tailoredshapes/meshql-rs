//! Every version of a document has an address — ksqlDB adapter.
//! See `meshql-cert/tests/features/versions.feature`.
//!
//! Runs against the same widget restlette the authorization certification uses,
//! so what is certified is the REST surface a caller actually gets.

use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::{authz, versions};
use meshql_cert::CertWorld;
use meshql_ksql::{ConfluentClient, KsqlConfig, KsqlRepository, KsqlSearcher};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // A skip is a failure. See the note in `repo_cert.rs`: this adapter used to
    // return early when no cluster was configured, which exits 0, so it
    // reported success on every machine that had never configured one.
    // A skip is a failure. This used to `return` when no cluster was
    // configured, which exits 0 — so the adapter reported success on every
    // machine that had never configured one, and hid two real defects.
    //
    // `for_certification` defaults to the local stack and refuses any
    // endpoint that is not loopback, so a developer with CONFLUENT_* already
    // exported for a real cluster cannot have `cargo test` quietly create and
    // drop topics on it.
    let config = KsqlConfig::for_certification().unwrap_or_else(|e| {
        panic!(
            "ksql versions certification cannot run.\n\
             This is a FAILURE, not a skip; the adapter is uncertified until it \n\
             passes.\n\
             \n\
             {e}"
        )
    });

    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(move |_feature, _rule, _scenario, world| {
            let config = config.clone();
            Box::pin(async move {
                let client = Arc::new(ConfluentClient::new(&config));
                let topic = format!("cert_{}", uuid::Uuid::new_v4().simple());
                let repo = Arc::new(KsqlRepository::new(client.clone(), &topic, &config));
                let searcher = Arc::new(KsqlSearcher::new(client, &topic));
                repo.initialize()
                    .await
                    .expect("failed to initialize ksqlDB DDL");
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
        .run_and_exit("../meshql-cert/tests/features/versions.feature")
        .await;
}
