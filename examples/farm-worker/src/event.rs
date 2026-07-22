//! The wire shape `meshql_changes::merkql_sink::publish_to_merkql` writes
//! onto each entity's merkql topic. Deliberately NOT a dependency on
//! `meshql-changes` itself — the worker only needs to agree on the wire
//! contract (the same discipline an SSE client follows against
//! `ChangeEvent::wire_json()`'s shape, never the producer's internals).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ThinEvent {
    pub entity: String,
    pub id: String,
    pub created_at: i64,
    pub deleted: bool,
}
