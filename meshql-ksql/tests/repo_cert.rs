use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::repo;
use meshql_cert::CertWorld;
use meshql_core::testing as cert;
use meshql_core::Repository;
use meshql_ksql::{ConfluentClient, KsqlConfig, KsqlRepository};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // A skip is a failure. This binary used to `return` when Confluent Cloud
    // credentials were absent, which exits 0 — so the whole ksql adapter
    // reported success on every machine that had never configured it, and
    // `.fail_on_skipped()` below never got the chance to see a thing. Every
    // other adapter fails when its backend is missing (Postgres, MySQL and
    // Mongo need Docker; DynamoDB needs DynamoDB Local); ksql now says so too.
    if std::env::var("CONFLUENT_KAFKA_REST_URL").is_err() {
        panic!(
            "ksql repository certification cannot run: CONFLUENT_KAFKA_REST_URL is not set.\n\
             This is a FAILURE, not a skip. The adapter is uncertified until a \n\
             Confluent Cloud cluster is configured — set CONFLUENT_KAFKA_REST_URL, \n\
             CONFLUENT_KAFKA_CLUSTER_ID, CONFLUENT_KAFKA_API_KEY, \n\
             CONFLUENT_KAFKA_API_SECRET, CONFLUENT_KSQLDB_URL, \n\
             CONFLUENT_KSQLDB_API_KEY and CONFLUENT_KSQLDB_API_SECRET."
        );
    }

    let config = KsqlConfig::from_env().expect("missing Confluent Cloud env vars");

    run_auth_cert(&config).await;

    CertWorld::cucumber()
        .max_concurrent_scenarios(1)
        .before(move |_feature, _rule, _scenario, world| {
            let config = config.clone();
            Box::pin(async move {
                let client = Arc::new(ConfluentClient::new(&config));
                let topic = format!("cert_{}", uuid::Uuid::new_v4().simple());
                let repo = KsqlRepository::new(client, &topic, &config);
                repo.initialize()
                    .await
                    .expect("failed to initialize ksqlDB DDL");
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

/// Shared repository authorization certification (meshql-core/src/testing.rs),
/// run against a fresh topic. Any assertion failure panics and fails the test
/// binary before the cucumber scenarios run.
async fn run_auth_cert(config: &KsqlConfig) {
    let client = Arc::new(ConfluentClient::new(config));
    let topic = format!("cert_{}", uuid::Uuid::new_v4().simple());
    let repo = KsqlRepository::new(client, &topic, config);
    repo.initialize()
        .await
        .expect("failed to initialize ksqlDB DDL");

    cert::seed_repository_auth_data(&repo).await;
    await_auth_seed_materialized(&repo).await;

    cert::test_repository_auth_wildcard_caller_sees_all(&repo).await;
    cert::test_repository_auth_restricted_caller_sees_only_intersecting(&repo).await;
    cert::test_repository_auth_denies_non_intersecting(&repo).await;
    cert::test_repository_auth_empty_tokens_are_public(&repo).await;
    cert::test_repository_auth_star_token_visible_to_all(&repo).await;
    cert::test_repository_auth_latest_version_controls_visibility(&repo).await;
    eprintln!("ksql repository auth cert: 6 tests passed");
}

/// KsqlRepository.create is fire-and-forget to Kafka; the ksqlDB TABLE
/// materializes asynchronously. Poll (as a "*" caller) until every seeded
/// envelope — including the latest auth-versioned version — is queryable.
async fn await_auth_seed_materialized(repo: &KsqlRepository) {
    let star = vec!["*".to_string()];
    let expected = [
        "repo-auth-public",
        "repo-auth-star",
        "repo-auth-alice",
        "repo-auth-bob",
        "repo-auth-versioned",
    ];

    for _ in 0..150 {
        let listed = repo
            .list(&meshql_core::TokenSession::new(star.clone()))
            .await
            .unwrap_or_default();
        let versioned_is_v2 = listed
            .iter()
            .find(|e| e.id == "repo-auth-versioned")
            .and_then(|e| e.payload.get("name"))
            .map(|n| n == "versioned-v2")
            .unwrap_or(false);
        if expected.iter().all(|id| listed.iter().any(|e| e.id == *id)) && versioned_is_v2 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("auth seed data did not materialize in the ksqlDB table within 30s");
}
