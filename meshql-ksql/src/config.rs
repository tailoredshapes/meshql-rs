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
