//! Consuming one partition, safely.
//!
//! This module exists because `merk_object::consumer::Consumer` has two
//! properties that combine into silent, permanent data loss, and one that makes
//! the design's "one invocation, one partition" shape inexpressible.
//!
//! **1. `poll` advances the cursor before the caller has folded anything.**
//! `Consumer::poll` sets `*position = tail` at read time
//! (`merk-object/src/consumer.rs:120-129`). `Consumer::close` commits when
//! `auto_commit` is set (`:145-147`), and `Drop` calls `close` (`:156-160`). So a
//! consumer with `auto_commit: true` whose handler returns `Err` drops during
//! unwind, commits offsets for records that were polled and never folded, and
//! those events are **gone from the projection, silently and permanently**. That
//! is at-most-once delivery, it is one configuration line away at all times, and
//! it is not in merk-cloud's documented limitations.
//!
//! **2. Committed offsets are last-write-wins.** `ConsumerGroup::commit`
//! serialises the *whole* offsets map and calls `Backend::put`, which is
//! documented as an unconditional overwrite — the `Backend` trait has no
//! conditional put at all. The forward-only guard operates on a map loaded once
//! at `open` and never re-read, so it protects one process's in-memory view, not
//! two processes. Two consumers committing different partitions of the same
//! group clobber each other. The good news, which the README does not state: the
//! clobber can only push an offset **backwards**, never forwards, because every
//! value written is one the process either read from the store or advanced
//! itself. So the consequence is replay and cost, not loss — provided folds are
//! idempotent, which they must be anyway.
//!
//! **3. `subscribe` takes topics, not partitions.** There is no way to ask
//! `Consumer` for one partition of a topic; it subscribes to every partition the
//! topic has. A worker that wants a single partition per invocation therefore
//! cannot use `Consumer` at all — using it with a per-partition group id would be
//! *correct* (each offsets object still has one writer) but would fold every
//! event once per partition, an N-fold waste on an N-partition topic.
//!
//! [`SafeConsumer`] answers all three by driving `Topic`, `Partition` and
//! `ConsumerGroup` directly, all of which are public:
//!
//! - It has **no `auto_commit` field and no `Drop` impl**, so defect 1 is not
//!   configurable here. The cursor advances only after a fold returns `Ok`.
//! - It takes a `(worker, topic, partition)` triple and derives the group id from
//!   it, so every offsets object has exactly one writer by construction and
//!   defect 2 cannot arise.
//! - It reads one partition.

use merk_object::backend::Backend;
use merk_object::broker::BrokerRef;
use merk_object::group::TopicPartition;
use merk_object::record::Record;
use meshql_core::{MeshqlError, Result};
use std::collections::HashMap;

/// The group id for one worker's view of one partition.
///
/// Every offsets object in the store is `groups/{group_id}/offsets`, so a group
/// id that names the partition gives each object a single writer. This is the
/// whole mitigation for the last-write-wins commit, and it is a naming
/// convention rather than a mechanism, so it is worth having one function.
///
/// The partition count is baked into the set of group ids, which is another
/// reason partition count is immutable: raising it means new group ids and a
/// replay.
pub fn group_id_for(worker: &str, topic: &str, partition: u32) -> String {
    format!("{worker}-{topic}-p{partition}")
}

/// Where a never-committed group starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// From offset zero. What a projection rebuild wants.
    Earliest,
    /// From the current tail. Skips history.
    Latest,
}

/// One worker's cursor over one partition.
pub struct SafeConsumer<B: Backend> {
    broker: BrokerRef<B>,
    group_id: String,
    tp: TopicPartition,
    /// The next offset to read. Advanced **only** after a fold returns `Ok`.
    position: u64,
    max_batch: u64,
}

impl<B: Backend> SafeConsumer<B> {
    /// Default records per fold. Bounded so one invocation's memory and wall
    /// clock are bounded; the caller loops while [`Self::fold`] returns a full
    /// batch.
    pub const DEFAULT_MAX_BATCH: u64 = 1_000;

    pub fn open(
        broker: BrokerRef<B>,
        worker: &str,
        topic: &str,
        partition: u32,
        start: Start,
    ) -> Result<Self> {
        let group_id = group_id_for(worker, topic, partition);
        let tp = TopicPartition {
            topic: topic.to_string(),
            partition,
        };

        let committed = broker
            .committed_offset(&group_id, &tp)
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let position = match (committed, start) {
            (Some(offset), _) => offset,
            (None, Start::Earliest) => 0,
            (None, Start::Latest) => Self::tail(&broker, topic, partition)?,
        };

        Ok(Self {
            broker,
            group_id,
            tp,
            position,
            max_batch: Self::DEFAULT_MAX_BATCH,
        })
    }

    pub fn with_max_batch(mut self, n: u64) -> Self {
        self.max_batch = n.max(1);
        self
    }

    pub fn group_id(&self) -> &str {
        &self.group_id
    }

    /// The next offset this consumer will read. Also the offset it would commit.
    pub fn position(&self) -> u64 {
        self.position
    }

    fn tail(broker: &BrokerRef<B>, topic: &str, partition: u32) -> Result<u64> {
        let topic_handle = broker
            .topic(topic)
            .ok_or_else(|| MeshqlError::Storage(format!("unknown topic '{topic}'")))?;
        let part = topic_handle.partition(partition).ok_or_else(|| {
            MeshqlError::Storage(format!("topic '{topic}' has no partition {partition}"))
        })?;
        let mut guard = part
            .write()
            .map_err(|e| MeshqlError::Storage(format!("partition lock: {e}")))?;
        guard
            .refresh()
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        Ok(guard.next_offset())
    }

    /// Read the next batch **without advancing anything.**
    ///
    /// Calling this twice returns the same records. That is the opposite of
    /// `Consumer::poll`, and it is the point.
    pub fn peek(&self) -> Result<Vec<Record>> {
        let topic_handle = self
            .broker
            .topic(&self.tp.topic)
            .ok_or_else(|| MeshqlError::Storage(format!("unknown topic '{}'", self.tp.topic)))?;
        let part = topic_handle.partition(self.tp.partition).ok_or_else(|| {
            MeshqlError::Storage(format!(
                "topic '{}' has no partition {}",
                self.tp.topic, self.tp.partition
            ))
        })?;

        let mut guard = part
            .write()
            .map_err(|e| MeshqlError::Storage(format!("partition lock: {e}")))?;
        guard
            .refresh()
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let tail = guard.next_offset();
        if self.position >= tail {
            return Ok(Vec::new());
        }
        let to = tail.min(self.position + self.max_batch);
        guard
            .read_range(self.position, to)
            .map_err(|e| MeshqlError::Storage(e.to_string()))
    }

    /// Read a batch, hand it to `fold`, and advance and commit **only if the
    /// fold succeeded.**
    ///
    /// Returns the number of records folded. A short batch means the partition is
    /// caught up; a full one means loop again.
    ///
    /// The order — projection write inside `fold`, then commit — is the order
    /// `domain-design.md` requires. Committing first means a crash between the
    /// two resumes with the events considered consumed and the projection missing
    /// them.
    pub fn fold<F>(&mut self, mut fold: F) -> Result<usize>
    where
        F: FnMut(&[Record]) -> Result<()>,
    {
        let records = self.peek()?;
        if records.is_empty() {
            return Ok(0);
        }

        // If this returns Err, `self.position` is untouched and nothing is
        // committed, so the next call re-reads exactly these records.
        fold(&records)?;

        self.position += records.len() as u64;
        self.commit()?;
        Ok(records.len())
    }

    /// Persist the cursor. Public because a worker that batches several folds
    /// before checkpointing needs to say when.
    pub fn commit(&self) -> Result<()> {
        let mut positions = HashMap::new();
        positions.insert(self.tp.clone(), self.position);
        self.broker
            .commit_offsets(&self.group_id, &positions)
            .map_err(|e| MeshqlError::Storage(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_ids_name_the_partition() {
        assert_eq!(
            group_id_for("graph-projector", "graph_event", 3),
            "graph-projector-graph_event-p3"
        );
    }

    #[test]
    fn group_ids_are_distinct_per_partition_and_per_worker() {
        let mut seen = std::collections::HashSet::new();
        for worker in ["graph-projector", "timeline-projector"] {
            for topic in ["graph_event", "story_event"] {
                for partition in 0..8u32 {
                    assert!(
                        seen.insert(group_id_for(worker, topic, partition)),
                        "collision at {worker}/{topic}/p{partition}"
                    );
                }
            }
        }
        assert_eq!(seen.len(), 2 * 2 * 8);
    }
}
