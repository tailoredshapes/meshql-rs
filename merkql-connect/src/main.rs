//! `merkql-connect` — the connector process.
//!
//! ```text
//! merkql-connect /etc/merkql-connect/lay_report.toml
//! ```
//!
//! One binary, one config file, one topic. It is deployed **beside the
//! database**, not inside the meshql service — see the crate docs for why that
//! is what makes it merkql's sole writer rather than a second one.

use anyhow::{Context, Result};
use merkql::broker::{Broker, BrokerConfig};
use merkql_connect::config::{QueueConfig, SourceConfig};
#[cfg(feature = "ksql")]
use merkql_connect::sink::RepositorySink;
use merkql_connect::sink::TopicSink;
use merkql_connect::{run_connector, CommitSource, ConnectorConfig, OffsetStore, TopicWriter};
#[cfg(feature = "ksql")]
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: merkql-connect <config.toml>"))?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading connector config {path}"))?;
    let config = ConnectorConfig::from_toml(&text)
        .with_context(|| format!("parsing connector config {path}"))?;

    // Claim the topic BEFORE opening the source. If another connector already
    // owns it, failing here means we never read a change we cannot write.
    let writer = open_sink(&config)?;

    let mut offsets = OffsetStore::open(
        config.offset_path(),
        config.connector_name(),
        config.entity(),
        config.offset_commit_interval(),
    )?;

    let source = open_source(&config).await?;

    eprintln!(
        "[merkql-connect] {} -> {} topic '{}' (snapshot_mode = {:?})",
        config.connector_name(),
        writer.backend(),
        config.topic,
        config.snapshot_mode
    );

    run_connector(
        source.as_ref(),
        writer.as_ref(),
        &mut offsets,
        config.snapshot_mode,
    )
    .await
}

/// Build the configured queue sink.
///
/// merkql is constructed here rather than behind a `Repository` because it
/// needs the single-writer lock and the single-partition check that only
/// merkql requires. Every other queue is a `Repository` and goes through
/// [`RepositorySink`] unchanged.
fn open_sink(config: &ConnectorConfig) -> Result<Box<dyn TopicSink>> {
    match config.queue()? {
        QueueConfig::Merkql { dir } => {
            let broker = Broker::open(BrokerConfig::new(&dir))
                .map_err(|e| anyhow::anyhow!("opening merkql store {}: {e}", dir.display()))?;
            Ok(Box::new(TopicWriter::claim(
                broker,
                &config.topic,
                &config.state_dir,
            )?))
        }

        #[cfg(feature = "ksql")]
        QueueConfig::Ksql { entity } => {
            use meshql_ksql::{client::ConfluentClient, config::KsqlConfig, KsqlRepository};

            let ksql_config = KsqlConfig::from_env().context(
                "reading Confluent credentials from the environment for [queue] type = \"ksql\"; \
                 set CONFLUENT_KAFKA_REST_URL, CONFLUENT_KAFKA_CLUSTER_ID, \
                 CONFLUENT_KAFKA_API_KEY, CONFLUENT_KAFKA_API_SECRET, CONFLUENT_KSQLDB_URL, \
                 CONFLUENT_KSQLDB_API_KEY and CONFLUENT_KSQLDB_API_SECRET",
            )?;
            let entity = entity.unwrap_or_else(|| config.topic.clone());
            let client = Arc::new(ConfluentClient::new(&ksql_config));
            let repository = Arc::new(KsqlRepository::new(client, &entity, &ksql_config));
            Ok(Box::new(RepositorySink::new(&config.topic, repository)))
        }

        #[allow(unreachable_patterns)]
        other => anyhow::bail!(
            "this build of merkql-connect has no support for queue backend '{}'; \
             rebuild with the matching cargo feature",
            other.backend()
        ),
    }
}

async fn open_source(config: &ConnectorConfig) -> Result<Box<dyn CommitSource>> {
    match &config.source {
        #[cfg(feature = "sqlite")]
        SourceConfig::Sqlite {
            path,
            table,
            entity,
        } => Ok(Box::new(
            merkql_connect::sqlite::SqliteSource::open(path, table, entity).await?,
        )),

        #[cfg(feature = "mongo")]
        SourceConfig::Mongo {
            uri,
            database,
            collection,
            entity,
        } => Ok(Box::new(
            merkql_connect::mongo::MongoSource::open(uri, database, collection, entity).await?,
        )),

        #[cfg(feature = "postgres")]
        SourceConfig::Postgres {
            conn,
            table,
            entity,
            slot,
            publication,
        } => Ok(Box::new(
            merkql_connect::postgres::PostgresSource::open(
                conn,
                table,
                entity,
                slot,
                publication,
                config.heartbeat_interval(),
            )
            .await?,
        )),

        #[cfg(feature = "sap")]
        SourceConfig::Sap {
            service_root,
            entity_set,
            odata_version,
            entity,
            key_properties,
            changed_at_property,
            authorized_tokens,
            auth,
            poll_interval_ms,
        } => Ok(Box::new(
            merkql_connect::sap::SapSource::open(
                service_root,
                entity_set,
                entity,
                *odata_version,
                key_properties,
                changed_at_property.as_deref(),
                authorized_tokens,
                auth,
                std::time::Duration::from_millis(*poll_interval_ms),
            )
            .await?,
        )),

        // Credentials are read from the environment inside `open`, never from
        // `config`: see `salesforce`'s module docs and the `SourceConfig`
        // variant. Nothing in this arm can carry a secret from the TOML.
        #[cfg(feature = "salesforce")]
        SourceConfig::Salesforce {
            instance_url,
            api_version,
            sobject,
            fields,
            entity,
            authorized_tokens,
            auth,
            poll_interval_ms,
            lag_seconds,
            max_window_seconds,
            capture_deletes,
        } => Ok(Box::new(
            merkql_connect::salesforce::SalesforceSource::open(
                merkql_connect::salesforce::SalesforceOptions {
                    instance_url: instance_url.clone(),
                    api_version: api_version.clone(),
                    sobject: sobject.clone(),
                    fields: fields.clone(),
                    entity: entity.clone(),
                    authorized_tokens: authorized_tokens.clone(),
                    auth: *auth,
                    poll_interval: std::time::Duration::from_millis(*poll_interval_ms),
                    lag: std::time::Duration::from_secs(*lag_seconds),
                    max_window: std::time::Duration::from_secs(*max_window_seconds),
                    capture_deletes: *capture_deletes,
                },
            )
            .await?,
        )),

        #[allow(unreachable_patterns)]
        other => anyhow::bail!(
            "this build of merkql-connect has no support for source {other:?}; \
             rebuild with the matching cargo feature"
        ),
    }
}
