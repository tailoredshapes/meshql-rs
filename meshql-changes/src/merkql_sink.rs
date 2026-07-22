//! The merkql-writing sink: an ADDITIONAL ChangeHub subscriber that mirrors
//! every ChangeEvent onto a merkql topic (one topic per entity), alongside —
//! never instead of — whatever's already broadcasting to SSE subscribers.
//! `ChangeHub`/`run_tails` are untouched; this is a second `hub.subscribe()`
//! consumer, per the pipeline design
//! (docs/superpowers/specs/2026-07-22-merkql-worker-pipeline-design.md).

use crate::ChangeEvent;
use merkql::broker::{Broker, BrokerRef};
use merkql::record::ProducerRecord;
use tokio::sync::broadcast;

/// Publish one `ChangeEvent` onto the merkql topic named after its entity.
/// Auto-creates the topic on first write (merkql's `Producer::send` default,
/// `BrokerConfig::auto_create_topics == true`). Record key is the event's
/// `id`, so every event for the same entity instance routes to the same
/// partition and is delivered to a consumer in commit order — mirrors
/// `examples/egg-economy/src/source.rs::publish`.
pub fn publish_to_merkql(broker: &BrokerRef, event: &ChangeEvent) -> anyhow::Result<()> {
    let producer = Broker::producer(broker);
    let record = ProducerRecord::new(
        event.entity.clone(),
        Some(event.id.clone()),
        event.wire_json(),
    );
    producer.send(&record)?;
    Ok(())
}

/// Subscribe to the hub and mirror every event onto merkql, forever. Spawn
/// this ALONGSIDE `run_tails` — `tokio::spawn(run_merkql_sink(hub.subscribe(), broker.clone()))`
/// — never in place of it; the SSE path is untouched.
///
/// Lag handling: unlike the SSE path (a lagged client just reconnects and
/// refetches through GraphQL — the notification is lost but no data is),
/// a lagged merkql sink has no equivalent recovery: the events it missed
/// are never written to the topic, and the worker downstream has no other
/// way to learn about them. This is logged loudly rather than silently
/// skipped. Sizing `ChangeHub::new(capacity)` generously relative to write
/// burst volume is the operator's mitigation; driving the sink directly off
/// `ChangeSource::poll` instead of the broadcast hub (bypassing this whole
/// class of loss) is a real improvement but out of scope for this additive
/// change — flagged here, not solved here, per the spec's framing of the
/// sink as "simply an additional subscriber task."
pub async fn run_merkql_sink(mut rx: broadcast::Receiver<ChangeEvent>, broker: BrokerRef) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                if let Err(e) = publish_to_merkql(&broker, &event) {
                    eprintln!("[merkql-sink {}] publish: {e}", event.entity);
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                eprintln!(
                    "[merkql-sink] lagged by {n} events — those events were NEVER \
                     written to merkql; increase ChangeHub capacity"
                );
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeHub;
    use merkql::broker::{Broker, BrokerConfig, BrokerRef};
    use merkql::consumer::{ConsumerConfig, OffsetReset};
    use std::time::Duration;

    fn broker() -> BrokerRef {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        Broker::open(BrokerConfig::new(dir.path())).unwrap()
    }

    fn ev(entity: &str, id: &str, created_at: i64) -> ChangeEvent {
        ChangeEvent {
            entity: entity.into(),
            id: id.into(),
            created_at,
            deleted: false,
            authorized_tokens: vec!["farm-team".into()],
        }
    }

    #[test]
    fn publish_writes_a_token_free_record_to_the_entity_topic() {
        let broker = broker();
        publish_to_merkql(&broker, &ev("lay_report", "lr-1", 1000)).unwrap();

        let mut consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: "test".into(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer.subscribe(&["lay_report"]).unwrap();
        let records = consumer.poll(Duration::from_millis(50)).unwrap();
        assert_eq!(records.len(), 1);

        let v: serde_json::Value = serde_json::from_str(&records[0].value).unwrap();
        assert_eq!(v["entity"], "lay_report");
        assert_eq!(v["id"], "lr-1");
        assert_eq!(v["created_at"], 1000);
        assert_eq!(v["deleted"], false);
        assert!(
            !records[0].value.contains("farm-team"),
            "tokens must never reach the merkql topic"
        );
        assert_eq!(records[0].key.as_deref(), Some("lr-1"));
    }

    #[test]
    fn different_entities_route_to_different_topics() {
        let broker = broker();
        publish_to_merkql(&broker, &ev("lay_report", "lr-1", 1)).unwrap();
        publish_to_merkql(&broker, &ev("hen_productivity", "hp-1", 2)).unwrap();

        for (topic, id) in [("lay_report", "lr-1"), ("hen_productivity", "hp-1")] {
            let mut consumer = Broker::consumer(
                &broker,
                ConsumerConfig {
                    group_id: format!("test-{topic}"),
                    auto_commit: false,
                    offset_reset: OffsetReset::Earliest,
                },
            );
            consumer.subscribe(&[topic]).unwrap();
            let records = consumer.poll(Duration::from_millis(50)).unwrap();
            assert_eq!(records.len(), 1, "expected exactly one record on topic {topic}");
            assert_eq!(records[0].key.as_deref(), Some(id));
        }
    }

    #[tokio::test]
    async fn run_merkql_sink_mirrors_hub_events_by_entity_topic() {
        let broker = broker();
        let hub = ChangeHub::new(16);
        tokio::spawn(run_merkql_sink(hub.subscribe(), broker.clone()));

        hub.publish(ev("lay_report", "lr-1", 1000));
        hub.publish(ev("hen_productivity", "hp-1", 2000));
        tokio::time::sleep(Duration::from_millis(50)).await;

        for (topic, id) in [("lay_report", "lr-1"), ("hen_productivity", "hp-1")] {
            let mut consumer = Broker::consumer(
                &broker,
                ConsumerConfig {
                    group_id: format!("test-{topic}"),
                    auto_commit: false,
                    offset_reset: OffsetReset::Earliest,
                },
            );
            consumer.subscribe(&[topic]).unwrap();
            let records = consumer.poll(Duration::from_millis(50)).unwrap();
            assert_eq!(records.len(), 1, "expected one record on topic {topic}");
            assert_eq!(records[0].key.as_deref(), Some(id));
        }
    }

    #[tokio::test]
    async fn run_merkql_sink_survives_lag_and_keeps_mirroring() {
        let broker = broker();
        let hub = ChangeHub::new(2); // tiny buffer, easy to overrun before the task is scheduled
        let rx = hub.subscribe();
        tokio::spawn(run_merkql_sink(rx, broker.clone()));

        for i in 0..10 {
            hub.publish(ev("lay_report", &format!("e{i}"), i as i64));
        }
        // Published after the burst; must still land — proves the task
        // logged-and-continued past the Lagged error instead of exiting
        // (mirrors run_tails's "poll errors ... never fatal" contract).
        hub.publish(ev("lay_report", "sentinel", 999));
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut consumer = Broker::consumer(
            &broker,
            ConsumerConfig {
                group_id: "test".into(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer.subscribe(&["lay_report"]).unwrap();
        let records = consumer.poll(Duration::from_millis(50)).unwrap();
        assert!(
            records.iter().any(|r| r.key.as_deref() == Some("sentinel")),
            "sink must still be running (and mirroring) after a broadcast lag"
        );
    }
}
