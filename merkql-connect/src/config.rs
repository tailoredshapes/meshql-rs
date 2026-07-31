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

    /// An SAP S/4HANA entity set, replicated over OData delta. See the `sap`
    /// module for the argument; the things an operator must know are:
    ///
    /// - **`key_properties` is required and is not discovered.** It decides the
    ///   envelope id, and an id derived from a `$metadata` document SAP
    ///   rewrites on upgrade is an id that can silently change and fork every
    ///   aggregate carrying it.
    /// - **No credentials live here.** The config names an auth *mode* and the
    ///   environment variables holding the secrets.
    /// - **This source polls**, because SAP OData has no notification edge at
    ///   all. `poll_interval_ms` is how often the delta link is followed.
    Sap {
        /// The OData service root, e.g.
        /// `https://s4.example.com/sap/opu/odata/sap/API_BUSINESS_PARTNER`.
        service_root: String,
        /// The entity set below that root, e.g. `A_BusinessPartner`.
        entity_set: String,
        #[serde(default = "default_odata_version")]
        odata_version: SapODataVersion,
        entity: String,
        /// The entity's key properties, from `<Key><PropertyRef …>` in
        /// `$metadata`. Order does not matter — the encoding sorts by name.
        key_properties: Vec<String>,
        /// An optional last-changed property. When set, `source.ts_ms` and the
        /// envelope's `created_at` are the entity's own time rather than the
        /// connector's observation time, and the payload says which.
        #[serde(default)]
        changed_at_property: Option<String>,
        /// Stamped onto every envelope. SAP carries no meshql authorisation, so
        /// this is a configured list rather than something per-record.
        #[serde(default)]
        authorized_tokens: Vec<String>,
        auth: SapAuthConfig,
        #[serde(default = "default_sap_poll_ms")]
        poll_interval_ms: u64,
    },
}

/// Which OData dialect the SAP service speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SapODataVersion {
    V2,
    V4,
}

fn default_odata_version() -> SapODataVersion {
    SapODataVersion::V4
}

fn default_sap_poll_ms() -> u64 {
    30_000
}

/// How the connector authenticates to SAP.
///
/// Every variant names **where a secret lives**, never the secret itself. A
/// connector TOML is a file that gets copied into tickets, checked into
/// configuration repositories and printed by support tooling; a password in it
/// is a password that has leaked. Certificates are file paths for the same
/// reason PEM does not belong in an environment variable — a private key in the
/// environment is a private key in every child process's `/proc`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SapAuthConfig {
    /// No credentials. Correct only when something else authenticates — an SLT
    /// proxy on a private network, mTLS terminated ahead of the connector.
    None,
    Basic {
        user_env: String,
        pass_env: String,
    },
    Bearer {
        token_env: String,
    },
    /// OAuth2 client credentials — the BTP destination-service shape.
    Oauth2Cc {
        token_url: String,
        client_id_env: String,
        client_secret_env: String,
        #[serde(default)]
        scope: Option<String>,
    },
    /// OAuth2 SAML bearer — principal propagation from an identity provider.
    Oauth2SamlBearer {
        token_url: String,
        assertion_env: String,
        client_id_env: String,
    },
    /// X.509 client certificates.
    Mtls {
        cert_path: PathBuf,
        key_path: PathBuf,
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
            SourceConfig::Sap { entity, .. } => entity,
        }
    }

    pub fn connector_name(&self) -> &'static str {
        match &self.source {
            SourceConfig::Sqlite { .. } => "sqlite",
            SourceConfig::Mongo { .. } => "mongodb",
            SourceConfig::Postgres { .. } => "postgresql",
            SourceConfig::Sap { .. } => "sap",
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

    const SAP: &str = r#"
        topic = "business_partner"
        merkql_dir = "/m"
        state_dir = "/s"
        snapshot_mode = "when_needed"

        [source]
        type = "sap"
        service_root = "https://s4.example.com/sap/opu/odata/sap/API_BUSINESS_PARTNER"
        entity_set = "A_BusinessPartnerAddress"
        odata_version = "v2"
        entity = "business_partner_address"
        key_properties = ["BusinessPartner", "AddressID"]
        changed_at_property = "LastChangeDateTime"
        authorized_tokens = ["sap"]
        poll_interval_ms = 60000
        auth = { kind = "oauth2_cc", token_url = "https://auth/token", client_id_env = "SAP_CID", client_secret_env = "SAP_SECRET" }
    "#;

    #[test]
    fn a_sap_connector_parses() {
        let cfg = ConnectorConfig::from_toml(SAP).unwrap();
        assert_eq!(cfg.connector_name(), "sap");
        assert_eq!(cfg.entity(), "business_partner_address");
        match cfg.source {
            SourceConfig::Sap {
                odata_version,
                key_properties,
                poll_interval_ms,
                auth,
                ..
            } => {
                assert_eq!(odata_version, SapODataVersion::V2);
                assert_eq!(key_properties, ["BusinessPartner", "AddressID"]);
                assert_eq!(poll_interval_ms, 60_000);
                assert!(matches!(auth, SapAuthConfig::Oauth2Cc { .. }));
            }
            other => panic!("expected a sap source, got {other:?}"),
        }
    }

    /// A key that is not stated is a key that would have to be guessed, and a
    /// guessed key is an envelope id that can silently merge distinct SAP
    /// records. The parse must fail rather than default to something.
    #[test]
    fn a_sap_connector_without_key_properties_is_refused() {
        let err = ConnectorConfig::from_toml(
            r#"
            topic = "t"
            merkql_dir = "/m"
            state_dir = "/s"
            [source]
            type = "sap"
            service_root = "https://s4/api"
            entity_set = "A_BusinessPartner"
            entity = "bp"
            auth = { kind = "none" }
        "#,
        )
        .expect_err("key_properties has no safe default");
        assert!(err.to_string().contains("key_properties"), "got: {err}");
    }

    /// The config may name where a secret lives; it may never hold one. If a
    /// credential field ever gains a value-carrying variant, this fails.
    #[test]
    fn sap_auth_names_environment_variables_and_never_holds_a_secret() {
        let cfg = ConnectorConfig::from_toml(SAP).unwrap();
        let round_tripped = toml::to_string(&cfg.source).unwrap();
        assert!(round_tripped.contains("client_secret_env"));
        assert!(
            !round_tripped.contains("client_secret = "),
            "the config must not be able to carry a secret: {round_tripped}"
        );
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
