//! Debezium's change-event envelope, narrowed to meshql envelopes.
//!
//! Keeping Debezium's shape is what makes the three sources interchangeable
//! from a consumer's point of view: a worker reads `op` and `source` and does
//! not care whether the record came from a SQLite rowid scan, a Mongo change
//! stream or a Postgres replication slot.

use meshql_core::Envelope;
use serde::{Deserialize, Serialize};

/// Debezium's `op`.
///
/// `Read` is the one that earns its keep here: it marks a record emitted
/// during the initial snapshot rather than seen live, so a consumer can tell
/// backfill from live traffic — for instance to suppress notifications while a
/// projection is being built from the beginning of time.
///
/// meshql envelopes are append-only, so these sources only ever emit `Read`
/// and `Create`. `Update` and `Delete` exist because the wire shape is
/// Debezium's and a consumer written against it should not have to special-case
/// their absence; a *deletion* in meshql is a new envelope version with
/// `deleted: true`, which arrives as a `Create` carrying that flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    #[serde(rename = "r")]
    Read,
    #[serde(rename = "c")]
    Create,
    #[serde(rename = "u")]
    Update,
    #[serde(rename = "d")]
    Delete,
}

impl Op {
    pub fn as_str(&self) -> &'static str {
        match self {
            Op::Read => "r",
            Op::Create => "c",
            Op::Update => "u",
            Op::Delete => "d",
        }
    }
}

/// Where a record sits relative to the initial snapshot — Debezium's
/// three-valued `snapshot` field, not a boolean.
///
/// The third value is load-bearing. `Last` marks the final record of a
/// snapshot, and it is the **first position in a snapshot that is safe to
/// resume from**: every earlier one names a row *inside* the snapshot, and
/// resuming the live stream from there would begin it at a point the stream
/// never reached. Without `Last`, a connector stopped between a completed
/// snapshot and its first live record has no resumable position at all and
/// re-snapshots the entire table on restart. Safe — duplicates, never a gap —
/// but needlessly expensive, and it hides the fact that the snapshot finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Snapshot {
    /// A live record: the snapshot is over (or never ran).
    False,
    /// A snapshot record with more to follow.
    True,
    /// The final record of the snapshot.
    Last,
}

impl Snapshot {
    /// Whether this record came out of the snapshot rather than the stream.
    pub fn is_snapshot(&self) -> bool {
        !matches!(self, Snapshot::False)
    }

    /// Whether a snapshot is still running *after* this record — i.e. whether
    /// this record's position is unsafe to resume a live stream from.
    pub fn in_progress(&self) -> bool {
        matches!(self, Snapshot::True)
    }
}

/// Debezium's `source` block: where the record came from, and **the store's
/// own position for it**.
///
/// `position` is the native cursor — a SQLite rowid, a Mongo resume token, a
/// Postgres LSN. It is carried downstream verbatim rather than renumbered so
/// that a consumer can dedupe and reason about ordering against the store's
/// numbering instead of trusting ours. It is also exactly what the connector
/// persists in its [`crate::OffsetStore`]: one value, one meaning, no
/// translation layer to drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// `"sqlite"`, `"mongodb"` or `"postgresql"`.
    pub connector: String,
    /// The table or collection the write landed in.
    pub entity: String,
    /// The store's commit timestamp for this write, epoch millis.
    pub ts_ms: i64,
    /// The store's native position for this record. `None` only where a store
    /// genuinely cannot name one.
    pub position: Option<String>,
    /// Where this record sits relative to the initial snapshot.
    pub snapshot: Snapshot,
}

/// One committed write, in Debezium's envelope.
///
/// No `PartialEq`: `meshql_core::Envelope` does not implement it, and the
/// comparison that actually matters here is the *wire* one — compare the
/// serialized JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRecord {
    pub op: Op,
    /// When *the connector* emitted this record, epoch millis. Distinct from
    /// `source.ts_ms`, which is when the store committed the write — the two
    /// differ by the connector's lag, and a consumer that cares about domain
    /// time must use `source.ts_ms`.
    pub ts_ms: i64,
    /// Always `None` for these sources: meshql envelopes are immutable, so
    /// there is no prior image of a row. Present so the wire shape is
    /// Debezium's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Envelope>,
    /// The committed envelope.
    pub after: Option<Envelope>,
    pub source: SourceInfo,
}

impl ChangeRecord {
    /// A snapshot read (`op: r`) or a live create (`op: c`).
    pub fn new(op: Op, envelope: Envelope, source: SourceInfo) -> Self {
        Self {
            op,
            ts_ms: chrono::Utc::now().timestamp_millis(),
            before: None,
            after: Some(envelope),
            source,
        }
    }

    /// The merkql producer key for this record.
    ///
    /// The Envelope id, matching `MerkqlRepository::write_envelope`'s hardcoded
    /// `ProducerRecord::new(&self.topic, Some(envelope.id.clone()), value)`.
    /// Deliberately identical so a CDC-fed topic and a merkql-as-primary-store
    /// topic partition the same way. Note what this means: partitioning is by
    /// **envelope id**, which is unique per record, NOT by aggregate. Do not
    /// design a consumer that assumes one aggregate's records share a
    /// partition — they do not.
    pub fn key(&self) -> Option<String> {
        self.after
            .as_ref()
            .or(self.before.as_ref())
            .map(|e| e.id.clone())
    }

    /// The record's native store position, if it has one.
    pub fn position(&self) -> Option<&str> {
        self.source.position.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope() -> Envelope {
        let mut payload = meshql_core::Stash::new();
        payload.insert("eggs".to_string(), json!(3));
        Envelope::new("hen-1", payload, vec!["farm".to_string()])
    }

    fn source() -> SourceInfo {
        SourceInfo {
            connector: "sqlite".to_string(),
            entity: "envelopes".to_string(),
            ts_ms: 1751892345123,
            position: Some("42".to_string()),
            snapshot: Snapshot::False,
        }
    }

    /// The `op` codes are Debezium's single letters on the wire, not Rust
    /// variant names. A consumer written against Debezium's shape reads `"c"`.
    #[test]
    fn op_serializes_as_debeziums_single_letter_codes() {
        for (op, letter) in [
            (Op::Read, "r"),
            (Op::Create, "c"),
            (Op::Update, "u"),
            (Op::Delete, "d"),
        ] {
            assert_eq!(serde_json::to_value(op).unwrap(), json!(letter));
            assert_eq!(op.as_str(), letter);
        }
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let rec = ChangeRecord::new(Op::Create, envelope(), source());
        let wire = serde_json::to_string(&rec).unwrap();
        let back: ChangeRecord = serde_json::from_str(&wire).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), wire);
        assert_eq!(back.key(), rec.key());
        assert_eq!(back.source, rec.source);
        assert_eq!(back.op, rec.op);
    }

    /// The native position must survive onto the wire — it is what a consumer
    /// dedupes against, and what the connector's own offset store holds.
    #[test]
    fn the_source_block_carries_the_native_position_onto_the_wire() {
        let rec = ChangeRecord::new(Op::Read, envelope(), source());
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
        assert_eq!(v["op"], "r");
        assert_eq!(v["source"]["position"], "42");
        assert_eq!(v["source"]["connector"], "sqlite");
        assert_eq!(v["source"]["ts_ms"], 1751892345123i64);
        assert_eq!(v["after"]["id"], "hen-1");
    }

    /// Debezium's `snapshot` is three-valued on the wire, and `last` is the
    /// value the offset store depends on to know a snapshot finished.
    #[test]
    fn snapshot_serializes_as_debeziums_three_values() {
        for (flag, wire) in [
            (Snapshot::False, "false"),
            (Snapshot::True, "true"),
            (Snapshot::Last, "last"),
        ] {
            assert_eq!(serde_json::to_value(flag).unwrap(), json!(wire));
        }
    }

    /// `last` is a snapshot record whose position IS resumable — that
    /// distinction is the entire reason the field is not a boolean.
    #[test]
    fn only_a_non_final_snapshot_record_blocks_resuming() {
        assert!(!Snapshot::False.is_snapshot());
        assert!(Snapshot::True.is_snapshot());
        assert!(Snapshot::Last.is_snapshot());

        assert!(Snapshot::True.in_progress());
        assert!(
            !Snapshot::Last.in_progress(),
            "the last snapshot record is resumable"
        );
        assert!(!Snapshot::False.in_progress());
    }

    /// The key must be the Envelope id and nothing else — merkql hardcodes
    /// that, and a divergence here would partition the CDC topic differently
    /// from a merkql-as-primary-store topic for the same entity.
    #[test]
    fn the_producer_key_is_the_envelope_id() {
        let rec = ChangeRecord::new(Op::Create, envelope(), source());
        assert_eq!(rec.key(), Some("hen-1".to_string()));
    }
}
