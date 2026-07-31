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
use merkql_connect::config::SourceConfig;
use merkql_connect::{run_connector, CommitSource, ConnectorConfig, OffsetStore, TopicWriter};

#[tokio::main]
async fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: merkql-connect <config.toml>"))?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading connector config {path}"))?;
    let config = ConnectorConfig::from_toml(&text)
        .with_context(|| format!("parsing connector config {path}"))?;

    let broker = Broker::open(BrokerConfig::new(&config.merkql_dir)).map_err(|e| {
        anyhow::anyhow!("opening merkql store {}: {e}", config.merkql_dir.display())
    })?;

    // Claim the topic BEFORE opening the source. If another connector already
    // owns it, failing here means we never read a change we cannot write.
    let writer = TopicWriter::claim(broker, &config.topic, &config.state_dir)?;

    let mut offsets = OffsetStore::open(
        config.offset_path(),
        config.connector_name(),
        config.entity(),
        config.offset_commit_interval(),
    )?;

    let source = open_source(&config).await?;

    eprintln!(
        "[merkql-connect] {} -> topic '{}' (snapshot_mode = {:?})",
        config.connector_name(),
        config.topic,
        config.snapshot_mode
    );

    run_connector(source.as_ref(), &writer, &mut offsets, config.snapshot_mode).await
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

        #[allow(unreachable_patterns)]
        other => anyhow::bail!(
            "this build of merkql-connect has no support for source {other:?}; \
             rebuild with the matching cargo feature"
        ),
    }
}
