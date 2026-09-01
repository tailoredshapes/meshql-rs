pub mod auth;
pub mod config;
pub mod error;
pub mod testing;

pub use auth::{envelope_visible_to, tokens_visible_to, Auth, AuthContext, NoAuth, StashKeyAuth};
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
    pub authorized_tokens: Vec<String>,
}

impl Envelope {
    pub fn new(id: impl Into<String>, payload: Stash, tokens: Vec<String>) -> Self {
        Self {
            id: id.into(),
            payload,
            created_at: Utc::now(),
            deleted: false,
            authorized_tokens: tokens,
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
    async fn create(&self, envelope: Envelope, tokens: &[String]) -> Result<Envelope>;
    async fn read(
        &self,
        id: &str,
        tokens: &[String],
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>>;
    async fn list(&self, tokens: &[String]) -> Result<Vec<Envelope>>;
    async fn remove(&self, id: &str, tokens: &[String]) -> Result<bool>;
    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        tokens: &[String],
    ) -> Result<Vec<Envelope>>;
    async fn read_many(&self, ids: &[String], tokens: &[String]) -> Result<Vec<Envelope>>;
    async fn remove_many(&self, ids: &[String], tokens: &[String])
        -> Result<HashMap<String, bool>>;

    /// Every version of one document, oldest first.
    ///
    /// A version the caller is not authorized to read still appears, as a
    /// tombstone carrying its timestamp and deletion flag but no token.
    /// Omitting it would make the history look continuous when it is not.
    ///
    /// The default refuses rather than returning an empty list, so a caller can
    /// tell "this document has no history" from "this adapter cannot answer".
    async fn list_versions(&self, _id: &str, _tokens: &[String]) -> Result<Vec<VersionRef>> {
        Err(MeshqlError::Unsupported(
            "this adapter does not implement version listing".into(),
        ))
    }

    /// Resolve one version by its token. Applies the same authorization as
    /// `read`.
    async fn read_version(
        &self,
        _id: &str,
        _token: &str,
        _tokens: &[String],
    ) -> Result<Option<Envelope>> {
        Err(MeshqlError::Unsupported(
            "this adapter does not implement version reads".into(),
        ))
    }
}

#[async_trait::async_trait]
pub trait Searcher: Send + Sync {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Option<Stash>>;
    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Vec<Stash>>;
}
