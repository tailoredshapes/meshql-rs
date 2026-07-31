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
            SourceConfig::Sap { entity, .. } => entity,
            SourceConfig::Salesforce { entity, .. } => entity,
        }
    }

    pub fn connector_name(&self) -> &'static str {
        match &self.source {
            SourceConfig::Sqlite { .. } => "sqlite",
            SourceConfig::Mongo { .. } => "mongodb",
            SourceConfig::Postgres { .. } => "postgresql",
            SourceConfig::Sap { .. } => "sap",
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
