pub mod auth;
pub mod config;
pub mod error;
pub mod testing;

pub use auth::{
    system_session, token_session, Auth, AuthContext, AuthMark, Identity, NoAuth, Operation,
    Session, StashKeyAuth, SystemSession, TokenSession,
};
pub use config::{
    GraphletteConfig, InternalSingletonResolverConfig, InternalVectorResolverConfig, QueryConfig,
    RestletteConfig, RootConfig, RootConfigBuilder, ServerConfig, SingletonResolverConfig,
    VectorResolverConfig,
};
pub mod versions;
pub use error::{MeshqlError, Result};
pub use versions::{version_order, version_token, VersionRef};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type Stash = serde_json::Map<String, serde_json::Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    pub payload: Stash,
    pub created_at: DateTime<Utc>,
    pub deleted: bool,
    /// The plugin-owned authorization mark. Storage persists it verbatim and
    /// never reads it; only a `Session` interprets it.
    ///
    /// Serialized under its historical name so stored rows need no migration.
    #[serde(rename = "authorized_tokens", default)]
    pub auth: AuthMark,
}

impl Envelope {
    pub fn new(id: impl Into<String>, payload: Stash, auth: impl Into<AuthMark>) -> Self {
        Self {
            id: id.into(),
            payload,
            created_at: Utc::now(),
            deleted: false,
            auth: auth.into(),
        }
    }
}

/// Canonical ordering of a result set (architecture invariant: a result set is
/// returned in the insertion order of the envelopes it contains, so a `limit`
/// truncates a meaningful prefix rather than an arbitrary subset).
///
/// meshql is append-only: there is no edit, only a new `Envelope` version
/// sharing a canonical `id`. A read resolves each `id` to the latest version
/// at-or-before the `at` cutoff, and *that* version's position in the log is
/// the record's position in the result set. So the sort key is the **resolved**
/// version's `created_at`, not the id's first appearance.
///
/// `created_at` is millisecond-precision, so two envelopes can genuinely tie.
/// The tiebreaker is the envelope `id`, byte-ordered. Every adapter applies the
/// same two keys — including the ones that *do* have a monotonic sequence
/// (merkql log offset, SQLite `rowid`). Using the sequence as the primary sort
/// key would be truer to physical insertion order on those two adapters, but it
/// would make them disagree with Postgres/MySQL/Mongo/ksql — which have no such
/// sequence — whenever two envelopes land in the same millisecond. Cross-adapter
/// equivalence is worth more than sub-millisecond fidelity, so the sequence is
/// used only where it already was: to decide *which version* of an id resolves
/// when two versions share a millisecond.
///
/// The key is a total order over a result set, because a result set holds at
/// most one resolved version per `id`.
pub fn envelope_order(a: &Envelope, b: &Envelope) -> std::cmp::Ordering {
    a.created_at
        .timestamp_millis()
        .cmp(&b.created_at.timestamp_millis())
        .then_with(|| a.id.cmp(&b.id))
}

#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    async fn create(&self, envelope: Envelope, session: &dyn Session) -> Result<Envelope>;
    async fn read(
        &self,
        id: &str,
        session: &dyn Session,
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>>;
    async fn list(&self, session: &dyn Session) -> Result<Vec<Envelope>>;
    async fn remove(&self, id: &str, session: &dyn Session) -> Result<bool>;
    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        session: &dyn Session,
    ) -> Result<Vec<Envelope>>;
    async fn read_many(&self, ids: &[String], session: &dyn Session) -> Result<Vec<Envelope>>;
    async fn remove_many(
        &self,
        ids: &[String],
        session: &dyn Session,
    ) -> Result<HashMap<String, bool>>;

    /// Every version of one document, oldest first.
    ///
    /// A version the caller is not authorized to read still appears, as a
    /// tombstone carrying its timestamp and deletion flag but no token.
    /// Omitting it would make the history look continuous when it is not.
    ///
    /// Required. An adapter that cannot answer this fails its certification,
    /// which is the signal — a default returning "unsupported" would let an
    /// adapter fall out of conformance without anything saying so.
    async fn list_versions(&self, id: &str, session: &dyn Session) -> Result<Vec<VersionRef>>;

    /// Resolve one version by its token. Applies the same authorization as
    /// `read`.
    async fn read_version(
        &self,
        id: &str,
        token: &str,
        session: &dyn Session,
    ) -> Result<Option<Envelope>>;
}

#[async_trait::async_trait]
pub trait Searcher: Send + Sync {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Option<Stash>>;
    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Vec<Stash>>;
}

#[cfg(test)]
mod envelope_wire_tests {
    use super::*;

    /// The mark changed type but not its persisted shape, which is what lets
    /// existing rows migrate by doing nothing. Every adapter that serializes an
    /// `Envelope` — merkql, merksql, ksql, dynamo — writes the same bytes it
    /// wrote before, under the same key.
    #[test]
    fn the_mark_still_serializes_as_the_authorized_tokens_array() {
        let env = Envelope::new("id-1", Stash::new(), vec!["alice".to_string()]);
        let json = serde_json::to_value(&env).expect("an Envelope is serializable");
        assert_eq!(
            json.get("authorized_tokens"),
            Some(&serde_json::json!(["alice"])),
            "the mark must persist under its historical key, as a bare array"
        );
        assert!(
            json.get("auth").is_none(),
            "the Rust field name must not leak into the stored shape"
        );
    }

    /// A row written before this change deserializes into the new type.
    #[test]
    fn a_pre_existing_row_still_reads_back() {
        let stored = serde_json::json!({
            "id": "id-1",
            "payload": {"name": "sprocket"},
            "created_at": "2026-01-01T00:00:00Z",
            "deleted": false,
            "authorized_tokens": ["alice", "bob"],
        });
        let env: Envelope = serde_json::from_value(stored).expect("an old row still parses");
        assert_eq!(
            env.auth.as_parts(),
            ["alice".to_string(), "bob".to_string()]
        );
    }

    /// And a row written before `authorized_tokens` existed at all is an empty
    /// mark, not a parse error.
    #[test]
    fn a_row_with_no_mark_at_all_is_an_empty_mark() {
        let stored = serde_json::json!({
            "id": "id-1",
            "payload": {},
            "created_at": "2026-01-01T00:00:00Z",
            "deleted": false,
        });
        let env: Envelope = serde_json::from_value(stored).expect("a mark-less row still parses");
        assert!(env.auth.is_empty());
    }
}
