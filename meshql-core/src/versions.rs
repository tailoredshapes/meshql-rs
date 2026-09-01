//! Addressing one version of a document.
//!
//! meshql keeps every version of every document, but until now nothing could
//! enumerate them, and a version had no address: `created_at` is millisecond
//! precision, so two versions of one document can tie, and the `envelope_order`
//! tiebreak is the envelope id — a constant within one document's history.
//!
//! The token here closes that. It is derived from the fields that identify a
//! version rather than from anything the store provides, for two reasons.
//! Postgres and MySQL carry only the five envelope columns and have no stable
//! per-row key to borrow (`ctid` moves on `VACUUM`), so a natural key would
//! mean a migration on every deployment. And a content-derived token survives a
//! move between adapters: migrate SQLite to Postgres and every version URL
//! still resolves.

use crate::Envelope;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One version of a document, as it appears in a version listing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionRef {
    /// Addresses this version. `None` when the caller is not authorized to read
    /// it — the entry still appears, because omitting it would make the history
    /// look continuous when it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub created_at: DateTime<Utc>,
    pub deleted: bool,
}

impl VersionRef {
    pub fn visible(env: &Envelope) -> Self {
        Self {
            token: Some(version_token(env)),
            created_at: env.created_at,
            deleted: env.deleted,
        }
    }

    /// A version the caller cannot read. Carries when it happened and whether
    /// it was a deletion, and nothing else.
    pub fn tombstone(env: &Envelope) -> Self {
        Self {
            token: None,
            created_at: env.created_at,
            deleted: env.deleted,
        }
    }

    pub fn authorized(&self) -> bool {
        self.token.is_some()
    }
}

/// Derive a version's token.
///
/// Two versions collide only when the document id, the timestamp, the deletion
/// flag, the authorized tokens, and the payload are all identical — which makes
/// them the same version recorded twice, indistinguishable by any other means.
///
/// The payload is serialized with its keys sorted, so the token does not depend
/// on map iteration order.
pub fn version_token(env: &Envelope) -> String {
    let mut sorted: std::collections::BTreeMap<&String, &serde_json::Value> =
        std::collections::BTreeMap::new();
    for (k, v) in &env.payload {
        sorted.insert(k, v);
    }
    let mut tokens: Vec<&String> = env.authorized_tokens.iter().collect();
    tokens.sort();

    let mut h = Sha256::new();
    h.update(env.id.as_bytes());
    h.update(b"\x1f");
    h.update(env.created_at.timestamp_millis().to_be_bytes());
    h.update(b"\x1f");
    h.update([env.deleted as u8]);
    h.update(b"\x1f");
    h.update(serde_json::to_vec(&tokens).unwrap_or_default());
    h.update(b"\x1f");
    h.update(serde_json::to_vec(&sorted).unwrap_or_default());
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Order versions of one document: oldest first, ties broken by token.
///
/// The tiebreak is arbitrary but identical on every adapter and stable across
/// replays, which is what makes a version list reproducible. Today SQLite
/// breaks ties on `rowid` while Postgres and Mongo break them not at all, so
/// `read(id, at:)` resolves nondeterministically on two of them.
pub fn version_order(a: &Envelope, b: &Envelope) -> std::cmp::Ordering {
    a.created_at
        .timestamp_millis()
        .cmp(&b.created_at.timestamp_millis())
        .then_with(|| version_token(a).cmp(&version_token(b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Envelope;
    use chrono::TimeZone;

    fn env(id: &str, ms: i64, payload: serde_json::Value) -> Envelope {
        let mut e = Envelope::new(
            id.to_string(),
            payload.as_object().cloned().unwrap(),
            vec!["*".to_string()],
        );
        e.created_at = Utc.timestamp_millis_opt(ms).unwrap();
        e
    }

    #[test]
    fn the_same_version_hashes_the_same_every_time() {
        let a = env(
            "d1",
            1000,
            serde_json::json!({"name": "Auth", "tier": "prod"}),
        );
        let b = env(
            "d1",
            1000,
            serde_json::json!({"name": "Auth", "tier": "prod"}),
        );
        assert_eq!(version_token(&a), version_token(&b));
    }

    /// The case the whole design exists for: two writes in one millisecond are
    /// distinguishable, where a timestamp alone cannot tell them apart.
    #[test]
    fn two_versions_in_one_millisecond_have_distinct_tokens() {
        let a = env("d1", 1000, serde_json::json!({"name": "Auth"}));
        let b = env("d1", 1000, serde_json::json!({"name": "Auth Service"}));
        assert_eq!(a.created_at, b.created_at);
        assert_ne!(version_token(&a), version_token(&b));
    }

    #[test]
    fn a_different_document_at_the_same_instant_differs() {
        let a = env("d1", 1000, serde_json::json!({"name": "Auth"}));
        let b = env("d2", 1000, serde_json::json!({"name": "Auth"}));
        assert_ne!(version_token(&a), version_token(&b));
    }

    #[test]
    fn a_deletion_differs_from_the_version_it_deletes() {
        let a = env("d1", 1000, serde_json::json!({"name": "Auth"}));
        let mut b = env("d1", 1000, serde_json::json!({"name": "Auth"}));
        b.deleted = true;
        assert_ne!(version_token(&a), version_token(&b));
    }

    /// The token cannot depend on map iteration order, or the same version
    /// would address differently on two adapters.
    #[test]
    fn payload_key_order_does_not_change_the_token() {
        let a = env("d1", 1000, serde_json::json!({"a": 1, "b": 2}));
        let mut b = env("d1", 1000, serde_json::json!({}));
        b.payload.insert("b".into(), serde_json::json!(2));
        b.payload.insert("a".into(), serde_json::json!(1));
        assert_eq!(version_token(&a), version_token(&b));
    }

    #[test]
    fn ordering_is_oldest_first_and_total_within_a_millisecond() {
        let older = env("d1", 1000, serde_json::json!({"n": 1}));
        let a = env("d1", 2000, serde_json::json!({"n": 2}));
        let b = env("d1", 2000, serde_json::json!({"n": 3}));
        assert_eq!(version_order(&older, &a), std::cmp::Ordering::Less);
        assert_ne!(version_order(&a, &b), std::cmp::Ordering::Equal);
        // Total: whichever way it falls, it falls the same way every time.
        assert_eq!(version_order(&a, &b), version_order(&a, &b));
        assert_eq!(version_order(&b, &a), version_order(&a, &b).reverse());
    }

    #[test]
    fn a_tombstone_carries_when_but_not_what() {
        let e = env("d1", 1000, serde_json::json!({"secret": true}));
        let t = VersionRef::tombstone(&e);
        assert!(!t.authorized());
        assert!(t.token.is_none());
        assert_eq!(t.created_at, e.created_at);
    }
}
