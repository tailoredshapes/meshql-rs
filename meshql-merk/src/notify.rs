//! Waking a consumer, from the producer.
//!
//! `Consumer::poll` pulls, and on AWS there is no storage-side wake-up at all:
//! **directory buckets emit no S3 Event Notifications**, so nothing in the store
//! can tell a consumer anything. The wake-up therefore comes from the producer:
//! after a successful append, one SQS FIFO message carrying `{topic, partition}`
//! and nothing else, with `MessageGroupId = partition`.
//!
//! ## Why this is not the forbidden dual write
//!
//! `domain-design.md` forbids publishing the event after the write: a dual write
//! with no shared transaction, where a crash between the two loses the event and
//! corrupts every downstream projection. What crosses this queue is **not the
//! event**. It is a hint that says "look at partition 3", carrying no domain
//! data. The event is already durable in the log before the message is sent, the
//! consumer reads its committed offset and pulls the delta, so a duplicate
//! message is a no-op and a lost one is caught by the next append or by the
//! liveness sweep.
//!
//! The rule that keeps that true: **the payload must never contain anything but
//! `topic` and `partition`.** If a worker could read a message and learn
//! something it could not learn from the log, this has become a dual write.
//! [`tests::the_body_carries_exactly_topic_and_partition`] is that rule.
//!
//! ## The FIFO deduplication trap
//!
//! SQS FIFO has a **five-minute deduplication window**. With content-based
//! deduplication enabled, every notification for the same `(topic, partition)`
//! inside that window collapses to one — so the second append to a partition
//! within five minutes would wake nobody, and the liveness sweep, which runs
//! every five minutes, is exactly the wrong period to rescue it. That turns
//! "freshness comes from the producer notify, which is immediate" into "freshness
//! is up to five minutes" without anything logging a complaint.
//!
//! So the deduplication id must be **unique per notification**, and the queue
//! must not rely on content-based deduplication. Ordering still comes from
//! `MessageGroupId`, which is the partition. [`Notification::deduplication_id`]
//! is unique per call and [`tests::two_notifications_for_one_partition_are_not_deduplicated`]
//! pins it.

use serde::Serialize;

/// A wake-up hint. Two fields, and there must never be a third.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notification {
    pub topic: String,
    pub partition: u32,
}

impl Notification {
    pub fn new(topic: impl Into<String>, partition: u32) -> Self {
        Self {
            topic: topic.into(),
            partition,
        }
    }

    /// The message body: `{"topic":"…","partition":N}` and nothing else.
    pub fn body(&self) -> String {
        // Infallible for two scalar fields; the expect documents that rather
        // than hiding a Result nobody can act on.
        serde_json::to_string(self).expect("two scalars always serialise")
    }

    /// FIFO ordering group. One consumer per partition falls out of this, which
    /// is the shape the one-group-per-partition offset rule needs.
    pub fn message_group_id(&self) -> String {
        format!("{}-p{}", self.topic, self.partition)
    }

    /// Unique per notification. See the module docs on the deduplication trap:
    /// anything derived from the content suppresses wake-ups for five minutes.
    pub fn deduplication_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// Every `(topic, partition)` in a plan — the liveness sweep's whole job.
///
/// Five minutes is a failure-recovery bound, not a latency budget: freshness
/// comes from the producer notify, which is immediate. Shortening the interval
/// buys nothing and costs a `head` per partition per tick, plus a `list` when the
/// head shows nothing new, and idle polling is a leading cause of a bill that
/// climbs on flat volume.
pub fn sweep(plan: &crate::provision::TopicPlan) -> Vec<Notification> {
    plan.topics
        .iter()
        .flat_map(|spec| {
            (0..spec.partitions).map(move |partition| Notification::new(&spec.name, partition))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provision::TopicPlan;

    #[test]
    fn the_body_carries_exactly_topic_and_partition() {
        let body = Notification::new("graph_event", 3).body();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let obj = parsed.as_object().expect("an object");

        assert_eq!(obj.len(), 2, "the payload grew a field: {body}");
        assert_eq!(obj.get("topic").unwrap(), "graph_event");
        assert_eq!(obj.get("partition").unwrap(), 3);
        // Named explicitly, because these are the fields most likely to be added
        // "just for convenience" — and any of them would make this a dual write.
        for forbidden in ["offset", "offsets", "id", "event_id", "payload", "value"] {
            assert!(!obj.contains_key(forbidden), "payload carries {forbidden}");
        }
    }

    #[test]
    fn the_group_id_is_per_partition() {
        assert_eq!(
            Notification::new("story_event", 7).message_group_id(),
            "story_event-p7"
        );
        assert_ne!(
            Notification::new("story_event", 7).message_group_id(),
            Notification::new("story_event", 8).message_group_id()
        );
    }

    #[test]
    fn two_notifications_for_one_partition_are_not_deduplicated() {
        let a = Notification::new("story_event", 1);
        let b = Notification::new("story_event", 1);
        assert_eq!(a.body(), b.body(), "identical content");
        assert_ne!(
            a.deduplication_id(),
            b.deduplication_id(),
            "identical content must still be two messages: SQS FIFO's dedup window is \
             five minutes, so a content-derived id would silently drop the second \
             wake-up and the five-minute sweep would not rescue it"
        );
    }

    #[test]
    fn the_sweep_covers_every_partition_once() {
        let plan = TopicPlan::from_toml_str(
            "[[topic]]\nname=\"a\"\npartitions=8\n[[topic]]\nname=\"b\"\npartitions=2\n",
        )
        .unwrap();
        let messages = sweep(&plan);
        assert_eq!(messages.len() as u32, plan.total_partitions());
        assert_eq!(messages.len(), 10);

        let mut seen: Vec<String> = messages.iter().map(|m| m.message_group_id()).collect();
        seen.sort();
        let before = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), before, "the sweep notified a partition twice");
    }
}
