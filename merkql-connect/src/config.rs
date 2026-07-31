//! The connector's configuration file — one binary, one config, standalone.
//!
//! Shaped like a Debezium connector definition: a source type and its
//! connection, the tables or collections to capture, the target topic and
//! store, a snapshot mode, and a heartbeat interval.
//!
//! ```toml
//! # merkql-connect.toml
//! topic        = "lay_report"
//! state_dir    = "/var/lib/merkql-connect"
//! snapshot_mode = "initial"          # initial | never | when_needed
//! offset_commit_interval_ms = 1000
//! heartbeat_interval_ms     = 10000  # postgres only; see `postgres`
//!
//! [queue]
//! type = "merkql"
//! dir  = "/var/lib/merkql"
//!
//! [source]
//! type   = "sqlite"
//! path   = "/var/lib/meshql/lay_report.db"
//! table  = "envelopes"
//! entity = "lay_report"
//! ```
//!
//! The `[queue]` block is what makes the destination configurable — merkql for
//! development and early growth, Kafka in production, Postgres for the
//! medium/large tier. Swapping it is a config edit, not a rebuild. See
//! [`QueueConfig`].

use crate::source::SnapshotMode;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    /// The topic to replicate onto. One topic per event meshlette.
    pub topic: String,
    /// Which persistent queue to append to. Optional only so that configs
    /// predating the `[queue]` block keep working via `merkql_dir`; see
    /// [`ConnectorConfig::queue`].
    #[serde(default)]
    queue: Option<QueueConfig>,
    /// Deprecated: the merkql store's data directory. Superseded by
    /// `[queue] type = "merkql"`. Still read so existing deployments do not
    /// break on upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merkql_dir: Option<PathBuf>,
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

/// Which persistent queue the connector appends to.
///
/// The queue is a deployment decision, not a connector decision. merkql is the
/// development and early-growth answer; Kafka is the production one; a
/// Postgres-backed queue covers the medium/large tier. Every one of them is a
/// `meshql_core::Repository`, so the connector loop is identical across all of
/// them — see [`crate::sink::TopicSink`] for the one behavioural difference
/// between the merkql-direct sink and the repository sink.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum QueueConfig {
    /// merkql on the local filesystem. Single-writer per process, enforced
    /// with an advisory lock — see [`crate::sink::TopicWriter`].
    Merkql {
        /// The merkql store's data directory.
        dir: PathBuf,
    },
    /// Kafka, through ksqlDB (`meshql-ksql`).
    ///
    /// Credentials are read from the environment (`CONFLUENT_*`) rather than
    /// named here, because a connector config file is deployed as ordinary
    /// configuration and Kafka API secrets are not.
    Ksql {
        /// The ksqlDB entity (stream) backing this topic. Defaults to the
        /// connector's topic name.
        #[serde(default)]
        entity: Option<String>,
    },
}

impl QueueConfig {
    /// `"merkql"`, `"ksql"` — logged at startup so an operator can confirm
    /// which queue a config actually selected.
    pub fn backend(&self) -> &'static str {
        match self {
            QueueConfig::Merkql { .. } => "merkql",
            QueueConfig::Ksql { .. } => "ksql",
        }
    }
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
    /// A logical replication slot, woken by a trigger-emitted `NOTIFY`. See
    /// the `postgres` module for the full argument; the two things an operator
    /// must know are:
    ///
    /// - **The server needs `wal_level = logical`.** It is not runtime-settable,
    ///   and `open` refuses to start without it.
    /// - **An unconsumed slot pins WAL indefinitely and fills the disk**, so a
    ///   connector down over a weekend takes the database with it. That is why
    ///   `heartbeat_interval_ms` exists — the connector advances the slot on a
    ///   timer even when the watched table is completely idle, which is
    ///   counterintuitively the *dangerous* case. Retiring a connector means
    ///   dropping its slot; leaving the config file behind is not enough.
    Postgres {
        /// A libpq connection string. The connector opens it a second time for
        /// `LISTEN`, and the role needs rights to create a publication and a
        /// logical replication slot.
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
        let config: Self = toml::from_str(text)?;
        // Fail at parse time rather than at first append: a config naming no
        // queue at all has no destination, and discovering that after the
        // source has already been opened means changes were read that could
        // not be written.
        config.queue()?;
        Ok(config)
    }

    /// The configured queue.
    ///
    /// Prefers an explicit `[queue]` block, and falls back to a bare
    /// `merkql_dir` so configs written before the block existed keep working.
    /// Naming both is an error rather than a silent precedence rule — an
    /// operator who wrote both had a belief about which one wins, and there is
    /// no way to tell which.
    pub fn queue(&self) -> anyhow::Result<QueueConfig> {
        match (&self.queue, &self.merkql_dir) {
            (Some(_), Some(_)) => anyhow::bail!(
                "config names both a [queue] block and a top-level `merkql_dir`. \
                 `merkql_dir` is the deprecated spelling of `[queue] type = \"merkql\"`; \
                 keep one."
            ),
            (Some(queue), None) => Ok(queue.clone()),
            (None, Some(dir)) => Ok(QueueConfig::Merkql { dir: dir.clone() }),
            (None, None) => anyhow::bail!(
                "config names no queue: add a [queue] block, e.g.\n\n\
                 [queue]\ntype = \"merkql\"\ndir = \"/var/lib/merkql\"\n"
            ),
        }
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

    #[test]
    fn a_queue_block_selects_the_backend() {
        let cfg = ConnectorConfig::from_toml(
            r#"
            topic = "lay_report"
            state_dir = "/s"

            [queue]
            type = "ksql"

            [source]
            type = "sqlite"
            path = "/db"
            entity = "lay_report"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.queue().unwrap().backend(), "ksql");
    }

    #[test]
    fn a_merkql_queue_block_carries_its_directory() {
        let cfg = ConnectorConfig::from_toml(
            r#"
            topic = "lay_report"
            state_dir = "/s"

            [queue]
            type = "merkql"
            dir = "/var/lib/merkql"

            [source]
            type = "sqlite"
            path = "/db"
            entity = "lay_report"
        "#,
        )
        .unwrap();
        match cfg.queue().unwrap() {
            QueueConfig::Merkql { dir } => assert_eq!(dir, PathBuf::from("/var/lib/merkql")),
            other => panic!("expected a merkql queue, got {other:?}"),
        }
    }

    /// Configs written before `[queue]` existed named a bare `merkql_dir`.
    /// They must keep working, because an upgrade that silently stopped
    /// replicating would be indistinguishable from a quiet source.
    #[test]
    fn a_bare_merkql_dir_still_resolves_to_the_merkql_queue() {
        let cfg = ConnectorConfig::from_toml(SQLITE).unwrap();
        match cfg.queue().unwrap() {
            QueueConfig::Merkql { dir } => assert_eq!(dir, PathBuf::from("/var/lib/merkql")),
            other => panic!("expected a merkql queue, got {other:?}"),
        }
    }

    /// Both spellings means the operator held a belief about precedence, and
    /// there is no way to know which. Refuse rather than pick.
    #[test]
    fn naming_both_a_queue_block_and_merkql_dir_is_refused() {
        let err = ConnectorConfig::from_toml(
            r#"
            topic = "t"
            state_dir = "/s"
            merkql_dir = "/old"

            [queue]
            type = "merkql"
            dir = "/new"

            [source]
            type = "sqlite"
            path = "/db"
            entity = "t"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("keep one"), "got: {err}");
    }

    /// A config with no destination must fail at parse time, before the source
    /// is opened — otherwise changes are read that can never be written.
    #[test]
    fn a_config_with_no_queue_is_refused_at_parse_time() {
        let err = ConnectorConfig::from_toml(
            r#"
            topic = "t"
            state_dir = "/s"

            [source]
            type = "sqlite"
            path = "/db"
            entity = "t"
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("names no queue"), "got: {err}");
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
