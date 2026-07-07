//! The business-event catalog — the verbs.
//!
//! These are the *only* things a front end ever writes. Every event is an
//! immutable fact recorded on an event mesh (`POST /<verb>/api` → Mongo). A
//! CDC connector (configured infrastructure, see `source`) mirrors those writes
//! onto the shared `egg-events` topic, where workers fold them into the nouns.
//!
//! Nothing here writes a domain model. There are no directly-written nouns.

use meshql_core::{Envelope, Stash};

/// The single shared merkql topic every business event lands on. A single
/// topic gives a total order across all event types, which keeps every
/// projection mutually consistent.
pub const TOPIC: &str = "egg-events";

/// Event-type discriminators. Each is also the name of the event mesh the
/// event is written to, and the value the CDC connector stamps onto the record.
pub mod verb {
    // Actor-lifecycle events — build the actor nouns.
    pub const BUILD_FARM: &str = "build_farm";
    pub const BUILD_COOP: &str = "build_coop";
    pub const BUY_HENS: &str = "buy_hens";
    pub const MOVE_HEN_TO_COOP: &str = "move_hen_to_coop";
    pub const RETIRE_HEN: &str = "retire_hen";
    pub const BUILD_CONTAINER: &str = "build_container";
    pub const REGISTER_CONSUMER: &str = "register_consumer";

    // Egg-flow events — build the analytic nouns.
    pub const EGGS_LAID: &str = "eggs_laid";
    pub const EGGS_STORED: &str = "eggs_stored";
    pub const EGGS_WITHDRAWN: &str = "eggs_withdrawn";
    pub const EGGS_TRANSFERRED: &str = "eggs_transferred";
    pub const EGGS_CONSUMED: &str = "eggs_consumed";
}

/// Every business event, in one place — the write surface of the system.
pub const ALL_VERBS: &[&str] = &[
    verb::BUILD_FARM,
    verb::BUILD_COOP,
    verb::BUY_HENS,
    verb::MOVE_HEN_TO_COOP,
    verb::RETIRE_HEN,
    verb::BUILD_CONTAINER,
    verb::REGISTER_CONSUMER,
    verb::EGGS_LAID,
    verb::EGGS_STORED,
    verb::EGGS_WITHDRAWN,
    verb::EGGS_TRANSFERRED,
    verb::EGGS_CONSUMED,
];

/// A business event as it flows on the log: the verb, plus the immutable
/// envelope that was written to the event mesh. This is what the CDC connector
/// produces and what workers consume.
#[derive(Clone, Debug)]
pub struct EventRecord {
    pub verb: String,
    /// Envelope id of the event itself (stable across replay).
    pub id: String,
    pub created_at_ms: i64,
    pub payload: Stash,
}

impl EventRecord {
    pub fn from_envelope(verb: impl Into<String>, env: &Envelope) -> Self {
        Self {
            verb: verb.into(),
            id: env.id.clone(),
            created_at_ms: env.created_at.timestamp_millis(),
            payload: env.payload.clone(),
        }
    }

    /// Wire form produced onto the topic by the CDC connector.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "verb": self.verb,
            "id": self.id,
            "created_at_ms": self.created_at_ms,
            "payload": self.payload,
        })
    }

    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            verb: v.get("verb")?.as_str()?.to_string(),
            id: v.get("id")?.as_str()?.to_string(),
            created_at_ms: v.get("created_at_ms")?.as_i64()?,
            payload: v.get("payload")?.as_object()?.clone(),
        })
    }

    /// Read a string field from the event payload.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.payload.get(key).and_then(|v| v.as_str())
    }

    /// Read an integer field from the event payload.
    pub fn int(&self, key: &str) -> i64 {
        self.payload.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
    }
}
