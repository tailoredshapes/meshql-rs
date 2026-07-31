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
    /// Salesforce, polled over REST/SOQL on `SystemModstamp`, with deletes
    /// enumerated separately. See the `salesforce` module for the mechanism
    /// argument and — more importantly — for what a watermark poller does not
    /// capture. Three things an operator must know:
    ///
    /// - **Credentials never live in this file.** `auth` names the OAuth flow;
    ///   `SALESFORCE_CLIENT_ID`, `SALESFORCE_CLIENT_SECRET` and (for
    ///   `refresh_token`) `SALESFORCE_REFRESH_TOKEN` come from the
    ///   environment. This file is version-controlled and copied to hosts.
    /// - **`lag_seconds` is a latency floor, not a tuning knob.** It is the
    ///   assumed worst case between Salesforce stamping `SystemModstamp` and
    ///   the row becoming visible to a query; lowering it opens a gap that no
    ///   downstream check can detect.
    /// - **Delete tracking expires after about 30 days.** Unlike a PostgreSQL
    ///   slot, nothing holds it for us — the window runs on wall-clock time,
    ///   so a connector stopped for a month loses a month of deletions and its
    ///   stored cursor stops being usable at all.
    Salesforce {
        /// Login or My Domain host, e.g. `https://acme.my.salesforce.com`.
        /// Data calls go to whatever `instance_url` the token response names.
        instance_url: String,
        #[serde(default = "default_api_version")]
        api_version: String,
        /// One SObject per connector, matching one topic per meshlette. A
        /// shared cursor across SObjects would let the slowest one throttle
        /// every other.
        sobject: String,
        /// The fields to select. Required and explicit: SOQL has no
        /// `SELECT *`, and describing the object instead would silently change
        /// the payload shape the day an admin adds a custom field.
        fields: Vec<String>,
        entity: String,
        /// Written onto every envelope. Required, with no default: an empty
        /// token list means *public* in meshql, and CRM data should not become
        /// public by omission.
        authorized_tokens: Vec<String>,
        #[serde(default)]
        auth: SalesforceAuth,
        #[serde(default = "default_salesforce_poll_interval")]
        poll_interval_ms: u64,
        #[serde(default = "default_salesforce_lag")]
        lag_seconds: u64,
        /// Caps one query's time span, so a connector restarted after a long
        /// outage walks forward in bounded steps instead of issuing one query
        /// spanning the outage.
        #[serde(default = "default_salesforce_max_window")]
        max_window_seconds: u64,
        #[serde(default = "default_true")]
        capture_deletes: bool,
    },
}

/// Which OAuth flow the connected app is configured for. The secrets
/// themselves come from the environment; this only names the shape of the
/// exchange.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SalesforceAuth {
    #[default]
    ClientCredentials,
    RefreshToken,
}

fn default_table() -> String {
    "envelopes".to_string()
}

fn default_api_version() -> String {
    "v62.0".to_string()
}
fn default_salesforce_poll_interval() -> u64 {
    5_000
}
fn default_salesforce_lag() -> u64 {
    30
}
fn default_salesforce_max_window() -> u64 {
    3_600
}
fn default_true() -> bool {
    true
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
            SourceConfig::Salesforce { entity, .. } => entity,
        }
    }

    pub fn connector_name(&self) -> &'static str {
        match &self.source {
            SourceConfig::Sqlite { .. } => "sqlite",
            SourceConfig::Mongo { .. } => "mongodb",
            SourceConfig::Postgres { .. } => "postgresql",
            SourceConfig::Salesforce { .. } => "salesforce",
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

    /// The Salesforce source names an org, an SObject and an auth *mode* — and
    /// no credential. If a secret were ever accepted here it would be
    /// version-controlled by construction.
    #[test]
    fn a_salesforce_connector_parses_and_carries_no_secret() {
        let text = r#"
            topic = "accounts"
            merkql_dir = "/m"
            state_dir = "/s"

            [source]
            type = "salesforce"
            instance_url = "https://acme.my.salesforce.com"
            sobject = "Account"
            fields = ["Name", "AnnualRevenue"]
            entity = "accounts"
            authorized_tokens = ["farm"]
            auth = "refresh_token"
        "#;
        let cfg = ConnectorConfig::from_toml(text).unwrap();
        assert_eq!(cfg.connector_name(), "salesforce");
        assert_eq!(cfg.entity(), "accounts");
        match cfg.source {
            SourceConfig::Salesforce {
                api_version,
                auth,
                lag_seconds,
                capture_deletes,
                ..
            } => {
                assert_eq!(api_version, "v62.0", "a minimal config is a working config");
                assert_eq!(auth, SalesforceAuth::RefreshToken);
                assert_eq!(lag_seconds, 30);
                assert!(capture_deletes, "deletes are captured unless refused");
            }
            other => panic!("expected a salesforce source, got {other:?}"),
        }

        assert!(
            !text.contains("secret") && !text.contains("client_id"),
            "credentials come from the environment, never from this file"
        );
    }

    /// An empty `authorized_tokens` list means PUBLIC in meshql, so the field
    /// has no default — omitting it must fail to parse rather than quietly
    /// publish CRM data to every reader of the mesh.
    #[test]
    fn a_salesforce_source_without_authorized_tokens_does_not_parse() {
        let err = ConnectorConfig::from_toml(
            r#"
            topic = "accounts"
            merkql_dir = "/m"
            state_dir = "/s"

            [source]
            type = "salesforce"
            instance_url = "https://acme.my.salesforce.com"
            sobject = "Account"
            fields = ["Name"]
            entity = "accounts"
        "#,
        )
        .expect_err("authorized_tokens has no default, deliberately");
        assert!(err.to_string().contains("authorized_tokens"), "got: {err}");
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
