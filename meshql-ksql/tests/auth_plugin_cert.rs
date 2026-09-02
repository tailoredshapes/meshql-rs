//! The auth plugin decides, and nothing else does — ksqlDB adapter.
//! See `meshql-cert/tests/features/auth_plugin.feature`.
//!
//! The before-hook stands up storage only. The server comes up in the `Given`
//! step, because each scenario names its own plugin — and they are plugins no
//! adapter can second-guess, which is the whole point of this suite: an adapter
//! passes only if every surface actually asks.

use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::authz;
use meshql_cert::CertWorld;
use meshql_ksql::{ConfluentClient, KsqlConfig, KsqlRepository, KsqlSearcher};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // A skip is a failure. See the note in `repo_cert.rs`: this adapter used to
    // return early when no cluster was configured, which exits 0, so it
    // reported success on every machine that had never configured one.
    if std::env::var("CONFLUENT_KAFKA_REST_URL").is_err() {
        panic!(
            "ksql auth-plugin certification cannot run: CONFLUENT_KAFKA_REST_URL is not set.\n\
             This is a FAILURE, not a skip. The adapter is uncertified until a \n\
             Kafka REST endpoint and ksqlDB are reachable. For a local stack:\n\
             \n\
                 eval \"$(scripts/ksql-local.sh)\"\n\
             \n\
             or set the CONFLUENT_* variables at a Confluent Cloud cluster."
        );
    }

    let config = KsqlConfig::from_env().expect("missing ksqlDB env vars");

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
