use std::env;

/// Configuration for connecting to Confluent Cloud Kafka REST API and ksqlDB.
#[derive(Debug, Clone)]
pub struct KsqlConfig {
    pub kafka_rest_url: String,
    pub kafka_cluster_id: String,
    pub kafka_api_key: String,
    pub kafka_api_secret: String,
    pub ksqldb_url: String,
    pub ksqldb_api_key: String,
    pub ksqldb_api_secret: String,
    pub auto_create_ddl: bool,
    pub max_retries: u32,
    pub retry_delay_ms: u64,
}

impl KsqlConfig {
    pub fn from_env() -> Result<Self, env::VarError> {
        Ok(Self {
            kafka_rest_url: env::var("CONFLUENT_KAFKA_REST_URL")?,
            kafka_cluster_id: env::var("CONFLUENT_KAFKA_CLUSTER_ID")?,
            kafka_api_key: env::var("CONFLUENT_KAFKA_API_KEY")?,
            kafka_api_secret: env::var("CONFLUENT_KAFKA_API_SECRET")?,
            ksqldb_url: env::var("CONFLUENT_KSQLDB_URL")?,
            ksqldb_api_key: env::var("CONFLUENT_KSQLDB_API_KEY")?,
            ksqldb_api_secret: env::var("CONFLUENT_KSQLDB_API_SECRET")?,
            auto_create_ddl: env::var("KSQL_AUTO_CREATE_DDL")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            max_retries: 10,
            retry_delay_ms: 200,
        })
    }

    /// Configuration for the certification suites.
    ///
    /// Defaults to the local stack `scripts/ksql-local.sh` starts, and
    /// **refuses any endpoint that is not loopback**. Certification is not
    /// allowed to touch a real Confluent Cloud cluster: the suites create and
    /// drop topics freely, and a developer with `CONFLUENT_*` already exported
    /// for their own cluster would otherwise have `cargo test` run against it
    /// without ever saying so.
    ///
    /// This is deliberately separate from [`KsqlConfig::from_env`], which is
    /// the production surface and still points wherever it is told.
    pub fn for_certification() -> Result<Self, String> {
        let kafka_rest_url = env::var("CONFLUENT_KAFKA_REST_URL")
            .unwrap_or_else(|_| "http://localhost:18083".to_string());
        let ksqldb_url = env::var("CONFLUENT_KSQLDB_URL")
            .unwrap_or_else(|_| "http://localhost:8088".to_string());

        for url in [&kafka_rest_url, &ksqldb_url] {
            if !is_loopback(url) {
                return Err(format!(
                    "refusing to certify against a remote endpoint: {url}\n\
                     \n\
                     The ksql certification runs only against a local stack, because it \n\
                     creates and drops topics. Start one with:\n\
                     \n\
                         eval \"$(scripts/ksql-local.sh)\"\n\
                     \n\
                     If CONFLUENT_KAFKA_REST_URL or CONFLUENT_KSQLDB_URL is set in your \n\
                     shell for a real cluster, unset it before running the tests."
                ));
            }
        }

        Ok(Self {
            kafka_rest_url,
            kafka_cluster_id: env::var("CONFLUENT_KAFKA_CLUSTER_ID").map_err(|_| {
                "CONFLUENT_KAFKA_CLUSTER_ID is not set. Run: eval \"$(scripts/ksql-local.sh)\""
                    .to_string()
            })?,
            kafka_api_key: env::var("CONFLUENT_KAFKA_API_KEY").unwrap_or_else(|_| "local".into()),
            kafka_api_secret: env::var("CONFLUENT_KAFKA_API_SECRET")
                .unwrap_or_else(|_| "local".into()),
            ksqldb_url,
            ksqldb_api_key: env::var("CONFLUENT_KSQLDB_API_KEY").unwrap_or_else(|_| "local".into()),
            ksqldb_api_secret: env::var("CONFLUENT_KSQLDB_API_SECRET")
                .unwrap_or_else(|_| "local".into()),
            auto_create_ddl: true,
            max_retries: 10,
            retry_delay_ms: 200,
        })
    }

    /// Derive the Kafka topic name from an entity name.
    pub fn topic_name(entity: &str) -> String {
        entity.to_string()
    }

    /// Derive the ksqlDB stream name from an entity name.
    pub fn stream_name(entity: &str) -> String {
        format!("{}_stream", entity.replace('-', "_"))
    }

    /// Derive the ksqlDB table name from an entity name.
    pub fn table_name(entity: &str) -> String {
        format!("{}_table", entity.replace('-', "_"))
    }
}

/// Is this URL pointing at this machine?
///
/// Host-only check: a loopback name or address. Anything else — a Confluent
/// Cloud bootstrap, a shared staging broker — is not somewhere a suite that
/// creates and drops topics belongs.
fn is_loopback(url: &str) -> bool {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let host = without_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(without_scheme);
    matches!(
        host,
        "localhost" | "127.0.0.1" | "::1" | "[::1]" | "0.0.0.0"
    )
}

#[cfg(test)]
mod certification_target_tests {
    use super::is_loopback;

    #[test]
    fn loopback_urls_are_allowed() {
        assert!(is_loopback("http://localhost:18083"));
        assert!(is_loopback("http://127.0.0.1:8088"));
        assert!(is_loopback("http://[::1]:8088"));
        assert!(is_loopback("http://localhost:18083/kafka/v3"));
    }

    #[test]
    fn a_confluent_cloud_endpoint_is_refused() {
        assert!(!is_loopback(
            "https://pkc-abcde.us-east-1.aws.confluent.cloud:443"
        ));
        assert!(!is_loopback("https://ksqldb-xyz.confluent.cloud"));
    }

    /// A host that merely *contains* "localhost" is not loopback.
    #[test]
    fn a_lookalike_host_is_refused() {
        assert!(!is_loopback("https://localhost.evil.example:443"));
        assert!(!is_loopback("https://notlocalhost:8088"));
    }
}
