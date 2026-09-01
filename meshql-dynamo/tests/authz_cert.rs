//! End-to-end authorization certification, DynamoDB adapter.
//! See `meshql-cert/tests/features/authz.feature`.
//!
//! Requires DynamoDB Local at `MESHQL_DYNAMO_ENDPOINT` (default
//! `http://localhost:8123`).
//!
//! # The whole feature, twice
//!
//! Once unindexed and once with the secondary indexes **derived from
//! `meshql_cert::authz::root_config()`** — the very config the certified server
//! serves, not a copy of it. So the indexed run is the deployment shape end to
//! end: one `RootConfig` produces both the queries and the indexes that answer
//! them, and the authorization feature has to come out the same either way.
//!
//! `run_and_exit` panics on failure rather than exiting, so two sequential runs
//! is all "twice" costs.

use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::authz;
use meshql_cert::CertWorld;
use meshql_dynamo::{DynamoCollection, DynamoRepository, DynamoSearcher};
use std::sync::{Arc, Mutex};

fn endpoint() -> String {
    std::env::var("MESHQL_DYNAMO_ENDPOINT").unwrap_or_else(|_| "http://localhost:8123".into())
}

/// The table the current scenario is using, so the `after` hook can drop it.
/// `max_concurrent_scenarios(1)` is what makes a single slot enough.
static CURRENT: Mutex<Option<(aws_sdk_dynamodb::Client, String)>> = Mutex::new(None);

#[derive(Clone, Copy)]
enum Indexing {
    Off,
    On,
}

async fn run(indexing: Indexing) {
    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(move |_feature, _rule, _scenario, world| {
            Box::pin(async move {
                // A fresh table per scenario: several scenarios assert an exact
                // result set ("the result should be exactly \"alpha\""), so
                // leftovers from the previous scenario would fail them.
                let client = meshql_dynamo::make_client(Some(&endpoint())).await;
                let client_for_cleanup = client.clone();
                let table = format!("meshql_dynamo_authz_{}", uuid::Uuid::new_v4().simple());

                let (repo, searcher): (Arc<_>, Arc<_>) = match indexing {
                    Indexing::Off => (
                        Arc::new(
                            DynamoRepository::new_with_client(client.clone(), &table)
                                .await
                                .expect("create the authz cert table"),
                        ),
                        Arc::new(
                            DynamoSearcher::new_with_client(client, &table)
                                .await
                                .expect("searcher over the same table"),
                        ),
                    ),
                    Indexing::On => {
                        // The indexes come from the same RootConfig the server
                        // below serves. Nothing is declared twice.
                        let (repo, searcher) = DynamoCollection::open_with_client(
                            client,
                            &table,
                            &meshql_cert::authz::root_config(),
                        )
                        .await
                        .expect("create the indexed authz cert table")
                        .split();
                        (Arc::new(repo), Arc::new(searcher))
                    }
                };

                let addr = meshql_cert::authz::start_server(repo.clone(), searcher).await;
                world.server_addr = Some(addr);
                world.set_repo(repo);
                world.reset_authz();

                *CURRENT.lock().unwrap() = Some((client_for_cleanup, table));
            })
        })
        .after(|_feature, _rule, _scenario, _ev, _world| {
            Box::pin(async move {
                // `run_and_exit` never returns normally on failure, so
                // per-scenario teardown is the only place a table can be
                // dropped. Failures are swallowed: a noisy teardown would bury
                // the assertion that actually failed.
                let current = CURRENT.lock().unwrap().take();
                if let Some((client, table)) = current {
                    let _ = meshql_dynamo::drop_table(&client, &table).await;
                }
            })
        }) // A scenario whose steps do not match is *skipped*, and cucumber
        // exits 0 on a skip. Without this, a suite where nothing ran at all
        // reports success — which is how a diverged feature file went
        // unnoticed for months.
        .fail_on_skipped()
        .run_and_exit("../meshql-cert/tests/features/authz.feature")
        .await;
}

#[tokio::main]
async fn main() {
    println!("== authz certification, unindexed ==");
    run(Indexing::Off).await;
    println!("== authz certification, indexes derived from the served RootConfig ==");
    run(Indexing::On).await;
}
