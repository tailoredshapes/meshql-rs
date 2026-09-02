//! `Envelope` ↔ record value conversion.
//!
//! Taken from `meshql-merkql`, which is the one thing worth reusing from it: an
//! envelope is stored as its serde JSON in the record's `value`, and the record
//! key is the envelope id. The key matters — routing is
//! `hash(key) % num_partitions`, and the `Repository` trait exposes no key
//! selector, so the envelope id is what every meshql adapter over a log uses.
//!
//! On merkql, per-record keys plus more than one partition is the documented
//! trap: it scatters an aggregate's events with no ordering between them. On
//! merk-cloud that advice inverts. A partition there is a serial resource — one
//! append at a time, ~8 ms each — and adding writers makes it worse, not better.
//! A Lambda gateway is definitionally many writers, so even distribution across
//! partitions is what it needs and unique keys deliver it. The cost is that
//! nothing orders two events about the same aggregate, which is why every fold
//! downstream has to be a set function with a deterministic tie-break rather
//! than a sequence function.

use meshql_core::{Envelope, MeshqlError, Result};

/// The record value for an envelope: its JSON, verbatim.
pub fn envelope_to_value(envelope: &Envelope) -> Result<String> {
    serde_json::to_string(envelope).map_err(|e| MeshqlError::Parse(e.to_string()))
}

/// The inverse, for a consumer folding the log.
pub fn value_to_envelope(value: &str) -> Result<Envelope> {
    serde_json::from_str(value).map_err(|e| MeshqlError::Parse(e.to_string()))
}

/// The producer key for an envelope.
pub fn envelope_key(envelope: &Envelope) -> String {
    envelope.id.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshql_core::Stash;
    use serde_json::json;

    fn envelope() -> Envelope {
        let mut payload = Stash::new();
        payload.insert("type".into(), json!("story_created"));
        payload.insert("body".into(), json!("hello"));
        payload.insert("occurred_at".into(), json!(1_785_369_600_000i64));
        Envelope::new(
            "e-1",
            payload,
            vec!["public".to_string(), "account:a_4d2e".to_string()],
        )
    }

    #[test]
    fn round_trips_every_envelope_field() {
        let original = envelope();
        let value = envelope_to_value(&original).unwrap();
        let back = value_to_envelope(&value).unwrap();

        assert_eq!(back.id, original.id);
        assert_eq!(back.payload, original.payload);
        assert_eq!(back.deleted, original.deleted);
        assert_eq!(back.auth, original.auth);
        // Millisecond fidelity is the contract every adapter is held to.
        assert_eq!(
            back.created_at.timestamp_millis(),
            original.created_at.timestamp_millis()
        );
    }

    #[test]
    fn the_key_is_the_envelope_id() {
        assert_eq!(envelope_key(&envelope()), "e-1");
    }

    #[test]
    fn a_number_stays_a_number() {
        // A payload number silently becoming a string is the classic
        // serialisation failure, and `occurred_at` being a number rather than a
        // string is a schema requirement upstream.
        let value = envelope_to_value(&envelope()).unwrap();
        let back = value_to_envelope(&value).unwrap();
        assert!(back.payload.get("occurred_at").unwrap().is_number());
    }
}
