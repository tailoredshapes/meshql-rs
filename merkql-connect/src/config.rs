//! The connector's configuration file — one binary, one config, standalone.
//!
//! Shaped like a Debezium connector definition: a source type and its
//! connection, the tables or collections to capture, the target topic and
//! store, a snapshot mode, and a heartbeat interval.
//!
//! ```toml
//! # merkql-connect.toml
//! topic        = "lay_report"
//! merkql_dir   = "/var/lib/merkql"
//! state_dir    = "/var/lib/merkql-connect"
//! snapshot_mode = "initial"          # initial | never | when_needed
//! offset_commit_interval_ms = 1000
//! heartbeat_interval_ms     = 10000  # postgres only; see `postgres`
//!
//! [source]
//! type   = "sqlite"
//! path   = "/var/lib/meshql/lay_report.db"
//! table  = "envelopes"
//! entity = "lay_report"
//! ```

use crate::source::SnapshotMode;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    /// The merkql topic to replicate onto. One topic per event meshlette.
    pub topic: String,
    /// The merkql store's data directory.
    pub merkql_dir: PathBuf,
    /// Where the connector keeps its offset file and its writer lock.
    pub state_dir: PathBuf,
    #[serde(default = "default_snapshot_mode")]
    pub snapshot_mode: SnapshotMode,
    #[serde(default = "default_offset_interval")]
    pub offset_commit_interval_ms: u64,
    #[serde(default = "default_heartbeat")]
    pub heartbeat_interval_ms: u64,
    pub source: SourceConfig,
}

fn default_snapshot_mode() -> SnapshotMode {
    SnapshotMode::Initial
}
fn default_offset_interval() -> u64 {
    1000
}
fn default_heartbeat() -> u64 {
    10_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceConfig {
    Sqlite {
        /// Path to the database file. Must be a real file — see the `sqlite`
        /// module for why an in-memory database cannot be watched.
        path: PathBuf,
        #[serde(default = "default_table")]
        table: String,
        entity: String,
    },
    Mongo {
        uri: String,
        database: String,
        collection: String,
        entity: String,
    },
    /// **Not implemented in this build.** The configuration shape is settled
    /// so an operator's file and the eventual connector agree, and
    /// `open_source` fails loudly rather than starting something that captures
    /// nothing.
    ///
    /// The blocker is client-side, not design-side. Streaming logical
    /// replication requires issuing `START_REPLICATION … LOGICAL` over a
    /// CopyBoth duplex on a connection opened with `replication=database`, and
    /// **no released Rust PostgreSQL crate exposes that**: `tokio-postgres`
    /// 0.7.18 has neither `copy_both_simple` nor a replication connection
    /// mode, and `sqlx` has no equivalent. `postgres-protocol` ships the
    /// pgoutput message parsers but nothing that can obtain the stream to feed
    /// them.
    ///
    /// Two implementable designs, both slot-based and neither lossy — note
    /// that `LISTEN`/`NOTIFY` **alone** is not one of them, because
    /// notifications are dropped when no one is listening and a connector
    /// restarting would silently lose every change in the gap:
    ///
    /// 1. **Consume the slot through SQL.**
    ///    `pg_logical_slot_get_binary_changes(slot, NULL, NULL,
    ///    'proto_version', '1', 'publication_names', pub)` reads pgoutput out
    ///    of a real replication slot over an ordinary connection. The slot
    ///    retains WAL until consumed, so nothing is lost, positions are real
    ///    LSNs, and calling it while idle is exactly the heartbeat that
    ///    advances `confirmed_flush_lsn` and releases WAL. The cost: the feed
    ///    is driven by repeated calls rather than a socket the server pushes
    ///    down.
    /// 2. **A trigger-emitted `NOTIFY` as the wake-up edge, with the slot
    ///    still the source of truth** — the same arrangement the SQLite source
    ///    uses with inotify. A dropped notification then costs latency, not
    ///    data, because the slot still holds everything. The cost: a trigger
    ///    on the envelope table.
    ///
    /// Whichever is chosen, the WAL trap is the thing to design around: an
    /// unconsumed slot pins WAL indefinitely and fills the disk, so a
    /// connector down over a weekend takes the database with it. That is why
    /// `heartbeat_interval_ms` exists — the connector must advance the slot on
    /// a timer even when the watched table is completely idle, which is
    /// counterintuitively the *dangerous* case.
    Postgres {
        /// A libpq connection string. The connector opens it a second time in
        /// replication mode, so the role needs the `REPLICATION` attribute.
        conn: String,
        #[serde(default = "default_table")]
        table: String,
        entity: String,
        /// Replication slot name. One slot per connector; see the `postgres`
        /// module for why an abandoned slot is dangerous.
        slot: String,
        /// Publication covering `table`. Created if absent.
        publication: String,
    },
}

fn default_table() -> String {
    "envelopes".to_string()
}

impl ConnectorConfig {
    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn offset_commit_interval(&self) -> Duration {
        Duration::from_millis(self.offset_commit_interval_ms)
    }

    pub fn heartbeat_interval(&self) -> Duration {
        Duration::from_millis(self.heartbeat_interval_ms)
    }

    /// The entity name the source reports — used to name the offset file, so
    /// two connectors sharing a state dir cannot collide.
    pub fn entity(&self) -> &str {
        match &self.source {
            SourceConfig::Sqlite { entity, .. } => entity,
            SourceConfig::Mongo { entity, .. } => entity,
            SourceConfig::Postgres { entity, .. } => entity,
        }
    }

    pub fn connector_name(&self) -> &'static str {
        match &self.source {
            SourceConfig::Sqlite { .. } => "sqlite",
            SourceConfig::Mongo { .. } => "mongodb",
            SourceConfig::Postgres { .. } => "postgresql",
        }
    }

    pub fn offset_path(&self) -> PathBuf {
        self.state_dir.join(format!("{}.offsets.json", self.topic))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQLITE: &str = r#"
        topic = "lay_report"
        merkql_dir = "/var/lib/merkql"
        state_dir = "/var/lib/merkql-connect"
        snapshot_mode = "when_needed"

        [source]
        type = "sqlite"
        path = "/var/lib/meshql/lay_report.db"
        entity = "lay_report"
    "#;

    #[test]
    fn a_sqlite_connector_parses_with_debezium_shaped_keys() {
        let cfg = ConnectorConfig::from_toml(SQLITE).unwrap();
        assert_eq!(cfg.topic, "lay_report");
        assert_eq!(cfg.snapshot_mode, SnapshotMode::WhenNeeded);
        assert_eq!(cfg.connector_name(), "sqlite");
        assert_eq!(cfg.entity(), "lay_report");
        // Defaults, so a minimal config is a working config.
        assert_eq!(cfg.offset_commit_interval(), Duration::from_millis(1000));
        match cfg.source {
            SourceConfig::Sqlite { table, .. } => assert_eq!(table, "envelopes"),
            other => panic!("expected a sqlite source, got {other:?}"),
        }
    }

    #[test]
    fn the_snapshot_mode_defaults_to_initial() {
        let cfg = ConnectorConfig::from_toml(
            r#"
            topic = "t"
            merkql_dir = "/m"
            state_dir = "/s"
            [source]
            type = "sqlite"
            path = "/db"
            entity = "t"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.snapshot_mode, SnapshotMode::Initial);
    }

    #[test]
    fn a_postgres_connector_parses() {
        let cfg = ConnectorConfig::from_toml(
            r#"
            topic = "lay_report"
            merkql_dir = "/m"
            state_dir = "/s"
            heartbeat_interval_ms = 5000

            [source]
            type = "postgres"
            conn = "host=db user=cdc dbname=farm"
            entity = "lay_report"
            slot = "merkql_lay_report"
            publication = "merkql_lay_report_pub"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.connector_name(), "postgresql");
        assert_eq!(cfg.heartbeat_interval(), Duration::from_millis(5000));
    }

    /// Two connectors sharing a state directory must not share an offset file.
    #[test]
    fn offset_paths_are_per_topic() {
        let a = ConnectorConfig::from_toml(SQLITE).unwrap();
        let mut b = a.clone();
        b.topic = "eggs_collected".into();
        assert_ne!(a.offset_path(), b.offset_path());
    }
}
