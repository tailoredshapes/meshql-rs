use chrono::Utc;
use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::searcher;
use meshql_cert::CertWorld;
use meshql_core::testing as cert;
use meshql_core::{Searcher, Stash};
use meshql_ksql::{ConfluentClient, KsqlConfig, KsqlRepository, KsqlSearcher};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // Only run cert tests if Confluent Cloud credentials are set
    if std::env::var("CONFLUENT_KAFKA_REST_URL").is_err() {
        eprintln!("Skipping ksql searcher cert tests: CONFLUENT_KAFKA_REST_URL not set");
        return;
    }

    let config = KsqlConfig::from_env().expect("missing Confluent Cloud env vars");

    run_auth_cert(&config).await;
    run_ordering_cert(&config).await;

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
            })
        })
        .run_and_exit("../meshql-cert/tests/features/searcher.feature")
        .await;
}

/// Shared searcher authorization certification (meshql-core/src/testing.rs),
/// run against a fresh topic. Any assertion failure panics and fails the test
/// binary before the cucumber scenarios run.
async fn run_auth_cert(config: &KsqlConfig) {
    let client = Arc::new(ConfluentClient::new(config));
    let topic = format!("cert_{}", uuid::Uuid::new_v4().simple());
    let repo = KsqlRepository::new(client.clone(), &topic, config);
    repo.initialize()
        .await
        .expect("failed to initialize ksqlDB DDL");
    let searcher = KsqlSearcher::new(client, &topic);

    cert::seed_searcher_auth_data(&repo).await;
    await_auth_seed_materialized(&searcher).await;

    cert::test_searcher_auth_wildcard_caller_sees_all(&searcher).await;
    cert::test_searcher_auth_restricted_caller_sees_only_intersecting(&searcher).await;
    cert::test_searcher_auth_denies_non_intersecting(&searcher).await;
    cert::test_searcher_auth_empty_tokens_are_public(&searcher).await;
    cert::test_searcher_auth_star_token_visible_to_all(&searcher).await;
    cert::test_searcher_auth_latest_version_controls_visibility(&searcher).await;
    eprintln!("ksql searcher auth cert: 6 tests passed");
}

/// Shared searcher ordering certification (meshql-core/src/testing.rs), run
/// against a fresh topic. Certifies that a result set comes back in the
/// insertion order of the envelopes it contains and that `limit` truncates that
/// order. ksqlDB pull queries accept no ORDER BY, so `KsqlSearcher` sorts
/// client-side; these certs are what hold it to the same order every other
/// adapter produces.
async fn run_ordering_cert(config: &KsqlConfig) {
    let client = Arc::new(ConfluentClient::new(config));
    let topic = format!("cert_{}", uuid::Uuid::new_v4().simple());
    let repo = KsqlRepository::new(client.clone(), &topic, config);
    repo.initialize()
        .await
        .expect("failed to initialize ksqlDB DDL");
    let searcher = KsqlSearcher::new(client, &topic);

    cert::seed_searcher_ordering_data(&repo).await;
    await_ordering_seed_materialized(&searcher).await;

    cert::test_searcher_ordering_limit_truncates_in_insertion_order(&searcher).await;
    cert::test_searcher_ordering_is_stable_across_repeated_queries(&searcher).await;
    cert::test_searcher_ordering_uses_resolved_version_position(&searcher).await;
    cert::test_searcher_ordering_breaks_millisecond_ties_by_id(&searcher).await;
    eprintln!("ksql searcher ordering cert: 4 tests passed");
}

/// Poll until every ordering-seed envelope — the twelve `ord-NN`, the resolved
/// version of the multi-version record, and both tied envelopes — is queryable.
async fn await_ordering_seed_materialized(searcher: &KsqlSearcher) {
    let star = vec!["*".to_string()];
    let args = Stash::new();

    for _ in 0..150 {
        let now = Utc::now().timestamp_millis();
        let ordered = searcher
            .find_all(r#"{"payload.type": "ordering"}"#, &args, &star, now)
            .await
            .unwrap_or_default();
        let tied = searcher
            .find_all(r#"{"payload.type": "orderingTie"}"#, &args, &star, now)
            .await
            .unwrap_or_default();
        let latest = searcher
            .find(r#"{"payload.name": "multi-v2"}"#, &args, &star, now)
            .await
            .ok()
            .flatten();

        if ordered.len() == 13 && tied.len() == 2 && latest.is_some() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("ordering seed data did not materialize in the ksqlDB table within 30s");
}

/// KsqlRepository.create is fire-and-forget to Kafka; the ksqlDB TABLE
/// materializes asynchronously. Poll (as a "*" caller) until every seeded
/// envelope — including the latest auth-versioned version — is queryable.
async fn await_auth_seed_materialized(searcher: &KsqlSearcher) {
    let star = vec!["*".to_string()];
    let args = Stash::new();

    for _ in 0..150 {
        let now = Utc::now().timestamp_millis();
        let shared = searcher
            .find_all(r#"{"payload.type": "authShared"}"#, &args, &star, now)
            .await
            .unwrap_or_default();
        let versioned = searcher
            .find(r#"{"payload.name": "versioned-v2"}"#, &args, &star, now)
            .await
            .ok()
            .flatten();
        let public = searcher
            .find(r#"{"payload.name": "public-doc"}"#, &args, &star, now)
            .await
            .ok()
            .flatten();
        let starred = searcher
            .find(r#"{"payload.name": "star-doc"}"#, &args, &star, now)
            .await
            .ok()
            .flatten();

        if shared.len() == 2 && versioned.is_some() && public.is_some() && starred.is_some() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("auth seed data did not materialize in the ksqlDB table within 30s");
}
