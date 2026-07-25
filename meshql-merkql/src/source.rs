//! A merkql-log-backed `ChangeSource` for streamlettes.
//!
//! Lives here rather than in `meshql-changes` on purpose: `meshql-changes`
//! must stay backend-agnostic and has no `merkql` dependency, so a merkql
//! source there would invert the dependency edge.

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use merkql::broker::{Broker, BrokerRef};
use merkql::consumer::{Consumer, ConsumerConfig, OffsetReset};
use meshql_changes::{ChangeEvent, ChangeSource};
use meshql_core::Envelope;
use std::sync::Mutex;
use std::time::Duration;

/// One merkql topic, tailed as a stream of `ChangeEvent`s.
///
/// One source owns one consumer; the consumer's position IS the source's
/// place in the log, so a source is not clonable and must not be shared
/// between two pumps.
pub struct MerkqlTopicSource {
    topic: String,
    /// Whether `poll` should attach the Envelope's `payload` to each event.
    include_payload: bool,
    /// `Consumer::poll` is synchronous and needs `&mut self`, but
    /// `ChangeSource::poll` takes `&self` from an async context. A
    /// `std::sync::Mutex` is the right lock here precisely because the
    /// critical section contains no `.await` — merkql's poll returns
    /// immediately (it ignores its timeout), so the guard never spans a
    /// yield point.
    consumer: Mutex<Consumer>,
}

impl MerkqlTopicSource {
    pub fn new(broker: BrokerRef, topic: &str, include_payload: bool) -> anyhow::Result<Self> {
        // Guard: pre-create the topic.
        //
        // `Consumer::subscribe` SILENTLY SKIPS a topic the broker cannot
        // find, and `positions` is only ever populated inside `subscribe`.
        // A source constructed before the entity's first write would
        // therefore deliver nothing — permanently, even once the topic
        // later appears. merkql creates topics lazily on produce and a
        // pump starts at boot, so that is exactly the state of every fresh
        // deployment. `create_topic` is idempotent (early `Ok` if present).
        broker
            .create_topic(topic, 1)
            .with_context(|| format!("pre-creating merkql topic '{topic}'"))?;

        // Guard: refuse a multi-partition topic.
        //
        // A single `Last-Event-ID` cannot address more than one partition:
        // `Consumer::poll` interleaves partition batches in HashMap order,
        // so "skip everything at-or-before the cursor" is only meaningful
        // within the cursor's own partition — history in the others is
        // dropped or replayed. Not redundant with the pre-creation guard:
        // `create_topic` returns early on an existing topic WITHOUT
        // validating its partition count. merkql's `default_partitions: 1`
        // means this stays invisible on the happy path, so it must fail
        // construction rather than degrade.
        let handle = broker
            .topic(topic)
            .ok_or_else(|| anyhow!("merkql topic '{topic}' missing right after create_topic"))?;
        let partitions = handle.num_partitions();
        if partitions != 1 {
            bail!(
                "merkql topic '{topic}' has {partitions} partitions; a streamlette requires a \
                 single-partition topic because one Last-Event-ID cursor cannot address more \
                 than one partition"
            );
        }

        // Guard: a fresh group id per source.
        //
        // `subscribe` prefers a COMMITTED offset over `offset_reset`
        // unconditionally; `Earliest` applies only when no committed offset
        // exists. Reusing a group id therefore yields a healthy-looking
        // stream that silently starts from another source's position.
        let mut consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: format!("streamlette-{}", uuid::Uuid::new_v4()),
                auto_commit: true,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer
            .subscribe(&[topic])
            .with_context(|| format!("subscribing to merkql topic '{topic}'"))?;

        Ok(Self {
            topic: topic.to_string(),
            include_payload,
            consumer: Mutex::new(consumer),
        })
    }
}

#[async_trait]
impl ChangeSource for MerkqlTopicSource {
    fn entity(&self) -> &str {
        &self.topic
    }

    async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
        let records = {
            let mut consumer = self
                .consumer
                .lock()
                .map_err(|e| anyhow!("streamlette consumer lock poisoned: {e}"))?;
            // The timeout is ignored by merkql — this returns immediately
            // with whatever has accumulated. The pump owns the cadence.
            consumer.poll(Duration::from_millis(0))?
        };

        records
            .into_iter()
            .map(|record| {
                let envelope: Envelope =
                    serde_json::from_str(&record.value).with_context(|| {
                        format!(
                            "decoding envelope at {}:{} on '{}'",
                            record.partition, record.offset, self.topic
                        )
                    })?;
                Ok(ChangeEvent {
                    entity: self.topic.clone(),
                    id: envelope.id,
                    created_at: envelope.created_at.timestamp_millis(),
                    deleted: envelope.deleted,
                    authorized_tokens: envelope.authorized_tokens,
                    cursor: Some(format!("{}:{}", record.partition, record.offset)),
                    // The Envelope's `payload` field, NEVER the whole
                    // Envelope: this is serialized verbatim into the SSE
                    // frame, and the `WireEvent` split only strips the
                    // top-level `authorized_tokens` — it cannot police
                    // tokens nested inside a payload.
                    payload: self
                        .include_payload
                        .then(|| serde_json::Value::Object(envelope.payload)),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merkql::broker::BrokerConfig;
    use merkql::record::ProducerRecord;
    use tempfile::TempDir;

    /// Leaked so the data dir outlives the broker for the whole test —
    /// dropping the TempDir first would pull the log out from under it.
    fn broker() -> BrokerRef {
        let dir: &'static TempDir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        Broker::open(BrokerConfig::new(dir.path())).unwrap()
    }

    fn produce(broker: &BrokerRef, topic: &str, id: &str) {
        produce_envelope(
            broker,
            topic,
            Envelope::new(id, serde_json::Map::new(), vec![]),
        );
    }

    fn produce_envelope(broker: &BrokerRef, topic: &str, envelope: Envelope) {
        let key = envelope.id.clone();
        let value = serde_json::to_string(&envelope).unwrap();
        Broker::producer(broker)
            .send(&ProducerRecord::new(topic, Some(key), value))
            .unwrap();
    }

    fn stash(json: serde_json::Value) -> meshql_core::Stash {
        match json {
            serde_json::Value::Object(map) => map,
            other => panic!("a Stash must be a JSON object, got {other}"),
        }
    }

    #[tokio::test]
    async fn rejects_a_multi_partition_topic() {
        let broker = broker();
        broker.create_topic("multi", 2).unwrap();

        // Matched rather than `unwrap_err`, which would demand `Debug` on a
        // type that wraps merkql's non-Debug `Consumer`.
        let err = match MerkqlTopicSource::new(broker, "multi", false) {
            Ok(_) => panic!("constructing a source on a 2-partition topic must fail"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("single-partition"), "got: {err}");
    }

    #[tokio::test]
    async fn delivers_the_first_write_when_started_before_any_write() {
        let broker = broker();
        // No topic yet — the state of every fresh deployment at boot.
        let source = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();

        produce(&broker, "hen", "hen-1");

        let events = source.poll().await.unwrap();
        assert_eq!(
            events.len(),
            1,
            "first write after a cold start must be delivered"
        );
        assert_eq!(events[0].id, "hen-1");
        assert_eq!(events[0].entity, "hen");
    }

    #[tokio::test]
    async fn each_source_uses_a_fresh_group_id() {
        let broker = broker();
        for id in ["hen-1", "hen-2", "hen-3"] {
            produce(&broker, "hen", id);
        }

        let a = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        assert_eq!(a.poll().await.unwrap().len(), 3);
        // Essential, not incidental: merkql persists a group offset only in
        // `commit_sync` or in `close` under auto_commit — never during
        // `poll`. Without this drop nothing is committed, `subscribe` falls
        // through to Earliest even for a hardcoded group id, and the test
        // would pass against the very bug it exists to catch.
        drop(a);

        let b = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        assert_eq!(
            b.poll().await.unwrap().len(),
            3,
            "a second source must see full history — a reused group_id would resume at 3 and see 0"
        );
    }

    #[tokio::test]
    async fn cursor_carries_the_records_real_partition_and_offset() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        for id in ["hen-1", "hen-2", "hen-3"] {
            produce(&broker, "hen", id);
        }

        let events = source.poll().await.unwrap();
        let cursors: Vec<_> = events.iter().map(|e| e.cursor.clone()).collect();
        // The real log positions, not a constant or a loop index that would
        // coincide with them only on a fresh single-partition topic.
        assert_eq!(
            cursors,
            vec![
                Some("0:0".to_string()),
                Some("0:1".to_string()),
                Some("0:2".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn attaches_the_payload_when_include_payload_is_set() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker.clone(), "hen", true).unwrap();
        produce_envelope(
            &broker,
            "hen",
            Envelope::new("hen-1", stash(serde_json::json!({"eggs": 3})), vec![]),
        );

        let events = source.poll().await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload, Some(serde_json::json!({"eggs": 3})));
    }

    #[tokio::test]
    async fn omits_the_payload_when_include_payload_is_unset() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        produce_envelope(
            &broker,
            "hen",
            Envelope::new("hen-1", stash(serde_json::json!({"eggs": 3})), vec![]),
        );

        let events = source.poll().await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload, None);
    }

    /// The payload must be the Envelope's `payload` field, never the whole
    /// Envelope. `ChangeEvent.payload` is serialized verbatim into the SSE
    /// frame, and the `WireEvent` split only strips the TOP-LEVEL
    /// `authorized_tokens` — it cannot police tokens nested inside a payload.
    /// Asserted against `wire_json` rather than the struct precisely so that
    /// nesting is caught.
    #[tokio::test]
    async fn payload_never_carries_authorized_tokens_onto_the_wire() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker.clone(), "hen", true).unwrap();
        produce_envelope(
            &broker,
            "hen",
            Envelope::new(
                "hen-1",
                stash(serde_json::json!({"eggs": 3})),
                vec!["SECRET-TEAM-TOKEN".to_string()],
            ),
        );

        let events = source.poll().await.unwrap();
        assert_eq!(events.len(), 1);
        // The tokens must still reach the filter — they are dropped from the
        // wire, not from the event.
        assert_eq!(events[0].authorized_tokens, vec!["SECRET-TEAM-TOKEN"]);

        let wire = events[0].wire_json();
        assert!(
            !wire.contains("SECRET-TEAM-TOKEN"),
            "token leaked onto the wire: {wire}"
        );
        assert!(
            !wire.contains("authorized_tokens"),
            "authorized_tokens leaked onto the wire: {wire}"
        );
        // And the payload really is carried — otherwise this test would pass
        // trivially against a source that emits no payload at all.
        let v: serde_json::Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(v["payload"], serde_json::json!({"eggs": 3}));
    }
}
