use cucumber::World as _;
#[allow(unused_imports)]
use meshql_cert::steps::searcher;
use meshql_cert::CertWorld;
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
