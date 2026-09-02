//! The auth plugin decides, and nothing else does — DynamoDB adapter.
//! See `meshql-cert/tests/features/auth_plugin.feature`.
//!
//! Unlike `authz_cert`, the before-hook stands up storage only. The server
//! comes up in the `Given` step, because each scenario names its own plugin —
//! and they are plugins no adapter can second-guess, which is the whole point
//! of this suite: an adapter passes only if every surface actually asks.
//!
//! Run twice, unindexed and with the secondary indexes derived from the same
//! `RootConfig` the certified server serves, for the reason `authz_cert` gives:
//! one config produces both the queries and the indexes that answer them, and
//! the answers have to come out the same either way.

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
                // result set, so leftovers from the previous one would fail them.
                let client = meshql_dynamo::make_client(Some(&endpoint())).await;
                let client_for_cleanup = client.clone();
                let table = format!("meshql_dynamo_plugin_{}", uuid::Uuid::new_v4().simple());

                let (repo, searcher): (Arc<_>, Arc<_>) = match indexing {
                    Indexing::Off => (
                        Arc::new(
                            DynamoRepository::new_with_client(client.clone(), &table)
                                .await
                                .expect("create the auth-plugin cert table"),
                        ),
                        Arc::new(
                            DynamoSearcher::new_with_client(client, &table)
                                .await
                                .expect("searcher over the same table"),
                        ),
                    ),
                    Indexing::On => {
                        // The indexes come from the same RootConfig the server
                        // serves. Nothing is declared twice.
                        let (repo, searcher) = DynamoCollection::open_with_client(
                            client,
                            &table,
                            &meshql_cert::authz::root_config(),
                        )
                        .await
                        .expect("create the indexed auth-plugin cert table")
                        .split();
                        (Arc::new(repo), Arc::new(searcher))
                    }
                };

                world.set_repo(repo);
                world.set_searcher(searcher);
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
        .run_and_exit("../meshql-cert/tests/features/auth_plugin.feature")
        .await;
}

#[tokio::main]
async fn main() {
    println!("== auth plugin certification, unindexed ==");
    run(Indexing::Off).await;
    println!("== auth plugin certification, indexes derived from the served RootConfig ==");
    run(Indexing::On).await;
}
