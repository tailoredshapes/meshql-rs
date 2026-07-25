//! A merkql-log-backed `ChangeSource` for streamlettes.
//!
//! Lives here rather than in `meshql-changes` on purpose: `meshql-changes`
//! must stay backend-agnostic and has no `merkql` dependency, so a merkql
//! source there would invert the dependency edge.

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use merkql::broker::{Broker, BrokerRef};
use merkql::consumer::{Consumer, ConsumerConfig, OffsetReset};
use merkql::record::Record;
use meshql_changes::{ChangeEvent, ChangeSource, SeekableSource};
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
    /// Kept so `backfill` can open a SECOND, throwaway consumer. The one
    /// below is permanently at the live tail (see `backfill`), and merkql
    /// v0.2.0 has no seek API, so replay needs its own reader.
    broker: BrokerRef,
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
            broker,
            consumer: Mutex::new(consumer),
        })
    }

    /// The single Record → ChangeEvent mapping.
    ///
    /// Shared by `poll` and `backfill` deliberately: the same log record is
    /// delivered live by one and replayed by the other, and the streamlette
    /// handler dedupes ACROSS them by cursor. Two copies of this that drifted
    /// would surface as one change arriving twice in two different shapes —
    /// the kind of bug that only shows up on a client reconnect.
    fn to_change_event(&self, record: &Record) -> anyhow::Result<ChangeEvent> {
        let envelope: Envelope = serde_json::from_str(&record.value).with_context(|| {
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
            // The Envelope's `payload` field, NEVER the whole Envelope: this
            // is serialized verbatim into the SSE frame, and the `WireEvent`
            // split only strips the top-level `authorized_tokens` — it cannot
            // police tokens nested inside a payload.
            payload: self
                .include_payload
                .then(|| serde_json::Value::Object(envelope.payload)),
        })
    }
}

/// `"{partition}:{offset}"`, the format `to_change_event` emits.
///
/// Deliberately minimal — Task 10 owns the full unusable-cursor policy. This
/// rejects only what could never address THIS log: anything unparseable, and
/// any partition but 0 (a streamlette topic is single-partition by
/// construction; `MerkqlTopicSource::new` refuses anything else).
fn parse_cursor(cursor: &str) -> Option<u64> {
    let (partition, offset) = cursor.split_once(':')?;
    if partition.parse::<u32>().ok()? != 0 {
        return None;
    }
    offset.parse::<u64>().ok()
}

#[async_trait]
impl SeekableSource for MerkqlTopicSource {
    /// Replay everything strictly after `cursor`.
    ///
    /// **Uses a second, throwaway consumer, not `self.consumer`.** The
    /// source's own consumer cannot replay: `Consumer::poll` reads
    /// `position..tail` and then sets `position = tail`, and merkql v0.2.0
    /// exposes no seek/rewind. The pump has been draining that consumer, so
    /// it sits at the live tail permanently.
    ///
    /// So: a fresh consumer with a FRESH group id (a reused one would find a
    /// committed offset, which `subscribe` prefers over `OffsetReset` —
    /// silently starting the replay from someone else's position), read the
    /// whole log in one poll, drop everything at-or-before the cursor, and
    /// drop the consumer. `auto_commit: false` so this throwaway never leaves
    /// an offset behind.
    ///
    /// Re-reading the log prefix is the accepted cost of having no seek API.
    async fn backfill(&self, cursor: &str) -> anyhow::Result<Vec<ChangeEvent>> {
        // An unusable cursor is an ERROR, never a silent "replay everything":
        // the latter looks like a working resume while flooding the client
        // with the entire log.
        let from = parse_cursor(cursor)
            .ok_or_else(|| anyhow!("unusable resume cursor '{cursor}' on '{}'", self.topic))?;

        let mut replay = Broker::consumer(
            &self.broker,
            ConsumerConfig {
                group_id: format!("streamlette-backfill-{}", uuid::Uuid::new_v4()),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        replay
            .subscribe(&[self.topic.as_str()])
            .with_context(|| format!("subscribing a backfill consumer to '{}'", self.topic))?;
        // One poll returns `0..tail` for the (single) partition.
        let records = replay.poll(Duration::from_millis(0))?;
        drop(replay);

        records
            .iter()
            .filter(|record| record.offset > from)
            .map(|record| self.to_change_event(record))
            .collect()
    }

    fn cursor_is_valid(&self, cursor: &str) -> bool {
        parse_cursor(cursor).is_some()
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

        // Same mapping `backfill` uses — see `to_change_event`.
        records
            .iter()
            .map(|record| self.to_change_event(record))
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

    /// The whole point of the second consumer: the source's OWN consumer has
    /// already been drained to the live tail by the pump, and merkql has no
    /// seek API, so backfill cannot reuse it.
    #[tokio::test]
    async fn backfill_returns_events_strictly_after_the_cursor() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        for id in ["hen-1", "hen-2", "hen-3", "hen-4"] {
            produce(&broker, "hen", id);
        }

        // Drain to the tail exactly as the pump would, so the source's own
        // consumer is at 4 and can never replay 0..4 again.
        assert_eq!(source.poll().await.unwrap().len(), 4);

        let replayed = source.backfill("0:1").await.unwrap();
        let ids: Vec<_> = replayed.iter().map(|e| e.id.clone()).collect();
        assert_eq!(
            ids,
            vec!["hen-3", "hen-4"],
            "backfill must return what follows the cursor, exclusive of it"
        );
        let cursors: Vec<_> = replayed.iter().map(|e| e.cursor.clone()).collect();
        assert_eq!(
            cursors,
            vec![Some("0:2".to_string()), Some("0:3".to_string())],
            "replayed cursors must be the real log offsets — they are what the \
             handler dedupes against"
        );
    }

    /// A cursor at the tail replays nothing; a cursor pointing before the
    /// start replays everything. Both are boundary cases a `>` vs `>=` slip
    /// would get wrong.
    #[tokio::test]
    async fn backfill_boundaries_are_exclusive_of_the_cursor() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        for id in ["hen-1", "hen-2", "hen-3"] {
            produce(&broker, "hen", id);
        }

        assert_eq!(
            source.backfill("0:2").await.unwrap().len(),
            0,
            "a cursor at the tail has nothing after it"
        );
        let all = source.backfill("0:0").await.unwrap();
        assert_eq!(
            all.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            vec!["hen-2", "hen-3"]
        );
    }

    /// The live and replayed representations of one record must be identical
    /// — the pump publishes one and the resume path the other, and the
    /// handler dedupes across them. A divergence would be invisible until a
    /// client saw the same change twice with different shapes.
    #[tokio::test]
    async fn backfill_maps_records_exactly_as_poll_does() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker.clone(), "hen", true).unwrap();
        produce_envelope(
            &broker,
            "hen",
            Envelope::new(
                "hen-1",
                stash(serde_json::json!({"eggs": 3})),
                vec!["farm-team".to_string()],
            ),
        );
        produce_envelope(
            &broker,
            "hen",
            Envelope::new("hen-2", stash(serde_json::json!({"eggs": 4})), vec![]),
        );

        let live = source.poll().await.unwrap();
        let replayed = source.backfill("0:0").await.unwrap();
        assert_eq!(replayed.len(), 1);
        let a = &live[1];
        let b = &replayed[0];
        assert_eq!(a.id, b.id);
        assert_eq!(a.entity, b.entity);
        assert_eq!(a.created_at, b.created_at);
        assert_eq!(a.deleted, b.deleted);
        assert_eq!(a.authorized_tokens, b.authorized_tokens);
        assert_eq!(a.cursor, b.cursor);
        assert_eq!(a.payload, b.payload);
        assert_eq!(a.wire_json(), b.wire_json());
    }

    #[tokio::test]
    async fn backfill_respects_include_payload() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        produce_envelope(
            &broker,
            "hen",
            Envelope::new("hen-1", stash(serde_json::json!({"eggs": 3})), vec![]),
        );
        produce_envelope(
            &broker,
            "hen",
            Envelope::new("hen-2", stash(serde_json::json!({"eggs": 4})), vec![]),
        );

        let replayed = source.backfill("0:0").await.unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].payload, None);
    }

    /// A cursor the source cannot parse must be an error, never a silent
    /// "replay everything" (which would look like a working resume while
    /// flooding the client with the whole log).
    #[tokio::test]
    async fn backfill_rejects_a_malformed_cursor() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker.clone(), "hen", false).unwrap();
        produce(&broker, "hen", "hen-1");

        for bad in ["", "7", "0:", "0:x", "1:0"] {
            assert!(
                source.backfill(bad).await.is_err(),
                "backfill must reject the unusable cursor {bad:?}"
            );
        }
    }

    /// Minimal by design — Task 10 owns the full unusable-cursor policy. This
    /// only refuses input that could never address this log.
    #[test]
    fn cursor_validity_refuses_obviously_malformed_input() {
        let broker = broker();
        let source = MerkqlTopicSource::new(broker, "hen", false).unwrap();

        assert!(source.cursor_is_valid("0:0"));
        assert!(source.cursor_is_valid("0:12345"));

        assert!(!source.cursor_is_valid(""), "empty");
        assert!(!source.cursor_is_valid("7"), "no separator");
        assert!(!source.cursor_is_valid("0:"), "no offset");
        assert!(!source.cursor_is_valid("a:0"), "non-numeric partition");
        assert!(!source.cursor_is_valid("0:x"), "non-numeric offset");
        assert!(!source.cursor_is_valid("0:-1"), "negative offset");
        // A streamlette topic is single-partition by construction, so any
        // other partition is addressing a log this source does not own.
        assert!(!source.cursor_is_valid("1:0"), "wrong partition");
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
