//! An append-only handle on a topic.
//!
//! This module is where the "no reads" rule is made structural rather than
//! documentary. The only field is a `Producer`, whose whole public surface is
//! `send` and `send_batch`. It holds its broker privately and hands out no
//! access to it, so from here there is no route to a `Consumer`, a `Topic` or a
//! `Partition` — and therefore no route to a scan.
//!
//! If a future change needs to read the log, it has to add a field to this
//! struct. That shows up in a diff. A comment saying "please do not scan" does
//! not.

use merk_object::backend::Backend;
use merk_object::broker::{Broker, BrokerRef};
use merk_object::producer::Producer;
use merk_object::record::ProducerRecord;
use meshql_core::{MeshqlError, Result};
use std::sync::Arc;

/// A handle that can append to a topic and do nothing else.
pub struct AppendOnlyLog<B: Backend> {
    /// Deliberately the *only* field. See the module docs.
    producer: Arc<Producer<B>>,
    topic: String,
}

impl<B: Backend> AppendOnlyLog<B> {
    /// Take an append-only handle on `topic`.
    ///
    /// The broker is consumed for the length of this call and then dropped by
    /// the caller: what is retained is the producer alone. Provisioning is a
    /// separate step — see [`crate::provision`] — because it needs the broker
    /// and this deliberately does not keep it.
    pub fn new(broker: &BrokerRef<B>, topic: impl Into<String>) -> Self {
        Self {
            producer: Arc::new(Broker::producer(broker)),
            topic: topic.into(),
        }
    }

    pub fn topic(&self) -> &str {
        &self.topic
    }

    /// One append. When this returns `Ok` the bytes are durable in the store —
    /// there is no background writer and no separate flush — which is what lets
    /// a handler return `201` meaning *committed* rather than *queued*.
    ///
    /// The engine is synchronous and blocks the calling thread for a store round
    /// trip (~8 ms in region, and the tail is bimodal under contention). Under
    /// an async runtime that would tie up a worker, so the call goes through
    /// `spawn_blocking`.
    ///
    /// **Returns the partition the record landed on**, which the caller needs and
    /// cannot otherwise get. The wake-up message carries `{topic, partition}`, and
    /// `Repository::create` returns only an `Envelope`, so without this a gateway
    /// would have to re-derive the routing itself — recomputing
    /// `hash(key) % num_partitions` against a partition count it read separately.
    /// That would work until it silently did not: a notification sent to the wrong
    /// partition wakes the wrong worker, costs nothing, logs nothing, and leaves
    /// the right partition unread until the five-minute sweep. Taking the number
    /// the engine actually assigned removes the class of bug rather than the
    /// instance.
    pub async fn append(&self, key: String, value: String) -> Result<u32> {
        let producer = Arc::clone(&self.producer);
        let topic = self.topic.clone();
        tokio::task::spawn_blocking(move || {
            producer
                .send(&ProducerRecord::new(topic, Some(key), value))
                .map(|record| record.partition)
                .map_err(|e| MeshqlError::Storage(e.to_string()))
        })
        .await
        .map_err(|e| MeshqlError::Storage(format!("append task panicked: {e}")))?
    }

    /// Many appends, coalesced into one store round trip per partition.
    ///
    /// Records are keyed, and routing is `hash(key) % num_partitions`, so a
    /// batch of unique keys fans out across partitions and this is one request
    /// per partition touched rather than one per record.
    ///
    /// Returns the distinct partitions touched, ascending — one wake-up each, and
    /// no duplicates, because a batch of 200 records spread over 8 partitions
    /// needs 8 messages rather than 200.
    pub async fn append_batch(&self, entries: Vec<(String, String)>) -> Result<Vec<u32>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let producer = Arc::clone(&self.producer);
        let topic = self.topic.clone();
        tokio::task::spawn_blocking(move || {
            let records: Vec<ProducerRecord> = entries
                .into_iter()
                .map(|(key, value)| ProducerRecord::new(topic.clone(), Some(key), value))
                .collect();
            let written = producer
                .send_batch(&records)
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let mut partitions: Vec<u32> = written.iter().map(|r| r.partition).collect();
            partitions.sort_unstable();
            partitions.dedup();
            Ok(partitions)
        })
        .await
        .map_err(|e| MeshqlError::Storage(format!("append task panicked: {e}")))?
    }
}
