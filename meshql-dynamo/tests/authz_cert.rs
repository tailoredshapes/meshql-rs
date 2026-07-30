//! End-to-end authorization certification, DynamoDB adapter.
//! See `meshql-cert/tests/features/authz.feature`.
//!
//! Requires DynamoDB Local at `MESHQL_DYNAMO_ENDPOINT` (default
//! `http://localhost:8123`).

use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::authz;
use meshql_cert::CertWorld;
use meshql_dynamo::{DynamoRepository, DynamoSearcher};
use std::sync::{Arc, Mutex};

fn endpoint() -> String {
    std::env::var("MESHQL_DYNAMO_ENDPOINT").unwrap_or_else(|_| "http://localhost:8123".into())
}

/// The table the current scenario is using, so the `after` hook can drop it.
/// `max_concurrent_scenarios(1)` is what makes a single slot enough.
static CURRENT: Mutex<Option<(aws_sdk_dynamodb::Client, String)>> = Mutex::new(None);

#[tokio::main]
async fn main() {
    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(|_feature, _rule, _scenario, world| {
            Box::pin(async move {
                // A fresh table per scenario: several scenarios assert an exact
                // result set ("the result should be exactly \"alpha\""), so
                // leftovers from the previous scenario would fail them.
                let client = meshql_dynamo::make_client(Some(&endpoint())).await;
                let client_for_cleanup = client.clone();
                let table = format!("meshql_dynamo_authz_{}", uuid::Uuid::new_v4().simple());

                let repo = Arc::new(
                    DynamoRepository::new_with_client(client.clone(), &table)
                        .await
                        .expect("create the authz cert table"),
                );
                let searcher = Arc::new(
                    DynamoSearcher::new_with_client(client, &table)
                        .await
                        .expect("searcher over the same table"),
                );

                let addr = meshql_cert::authz::start_server(repo.clone(), searcher).await;
                world.server_addr = Some(addr);
                world.set_repo(repo);
                world.reset_authz();

                *CURRENT.lock().unwrap() = Some((client_for_cleanup, table));
            })
        })
        .after(|_feature, _rule, _scenario, _ev, _world| {
            Box::pin(async move {
                // `run_and_exit` never returns, so per-scenario teardown is the
                // only place a table can be dropped. Failures are swallowed:
                // a noisy teardown would bury the assertion that actually failed.
                let current = CURRENT.lock().unwrap().take();
                if let Some((client, table)) = current {
                    let _ = meshql_dynamo::drop_table(&client, &table).await;
                }
            })
        })
        .run_and_exit("../meshql-cert/tests/features/authz.feature")
        .await;
}
