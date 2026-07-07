//! The CDC connector — the event → mongo → merkql bridge.
//!
//! **This is infrastructure, not application code.** Events are triggered from
//! the *storage layer*, never the write path. A front end does one thing: `POST`
//! a business event, which commits to the event mesh's store (Mongo). The
//! connector then observes the store's change feed (Mongo change streams; the
//! Debezium model) and mirrors each committed write onto the `egg-events` topic.
//!
//! Why the storage layer and not a restlette `post_create` hook:
//!
//! - **No dual write.** The application writes exactly once — to the store. The
//!   event is *derived* from the committed write, so there is no window where
//!   the row exists but the event was lost (or vice versa). A `post_create` hook
//!   is a dual write: crash between the DB commit and the publish and the event
//!   never fires. CDC guarantees at-least-once delivery *after* commit — "if the
//!   event doesn't fire post-write, it will" — and that guarantee is the
//!   datastore/CDC layer's responsibility, not the mesh's.
//! - **Correct order and time.** Events carry the store's commit order and
//!   timestamp, not the application's wall-clock, so replay and temporal reads
//!   agree with what actually happened.
//!
//! The trait below abstracts "observe committed event writes." The example ships
//! a portable poll-based tail so the pipeline runs on any backend (and in tests
//! with no Mongo); a production deployment configures a native change-stream
//! connector instead. Either way, no restlette is touched.

use crate::events::{EventRecord, TOPIC};
use async_trait::async_trait;
use merkql::broker::{Broker, BrokerRef};
use merkql::record::ProducerRecord;
use meshql_core::{Searcher, Stash};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Something that observes committed event writes and yields them as records.
#[async_trait]
pub trait EventSource: Send + Sync {
    /// The verb (event mesh) this source tails.
    fn verb(&self) -> &str;
    /// Return event records committed since the last poll (at-least-once; the
    /// consumer tolerates duplicates because folding is idempotent).
    async fn poll(&self) -> anyhow::Result<Vec<EventRecord>>;
}

/// Portable CDC tail over a `Searcher`: lists the event mesh and emits any
/// envelope id not seen before. Stands in for a native change-stream connector
/// on backends without one, and lets the whole pipeline run in-process for
/// tests. It observes the committed store, never the request path.
pub struct RepositoryTail {
    verb: String,
    searcher: Arc<dyn Searcher>,
    seen: tokio::sync::Mutex<HashSet<String>>,
}

impl RepositoryTail {
    pub fn new(verb: impl Into<String>, searcher: Arc<dyn Searcher>) -> Self {
        Self {
            verb: verb.into(),
            searcher,
            seen: tokio::sync::Mutex::new(HashSet::new()),
        }
    }
}

#[async_trait]
impl EventSource for RepositoryTail {
    fn verb(&self) -> &str {
        &self.verb
    }
    async fn poll(&self) -> anyhow::Result<Vec<EventRecord>> {
        let now = chrono::Utc::now().timestamp_millis();
        let rows: Vec<Stash> = self
            .searcher
            .find_all("{}", &Stash::new(), &["*".to_string()], now)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let mut seen = self.seen.lock().await;
        let mut out = Vec::new();
        for mut row in rows {
            let id = row
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if id.is_empty() || seen.contains(&id) {
                continue;
            }
            seen.insert(id.clone());
            let created_at_ms = row
                .get("created_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(now);
            row.remove("id");
            out.push(EventRecord {
                verb: self.verb.clone(),
                id,
                created_at_ms,
                payload: row,
            });
        }
        Ok(out)
    }
}

/// Publish one event record onto the shared topic — the connector's produce
/// side. In a native change-stream deployment the connector does exactly this
/// for each committed write.
pub fn publish(broker: &BrokerRef, rec: &EventRecord) -> anyhow::Result<()> {
    let producer = Broker::producer(broker);
    let value = serde_json::to_string(&rec.to_json())?;
    let record = ProducerRecord::new(TOPIC, Some(rec.id.clone()), value);
    producer
        .send(&record)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(())
}

/// The connector process: poll every source tail and mirror new committed
/// events onto the topic. Configured infrastructure — no restlette involved.
pub async fn run_connector(
    broker: BrokerRef,
    sources: Vec<Arc<dyn EventSource>>,
    interval: Duration,
) {
    loop {
        for s in &sources {
            match s.poll().await {
                Ok(recs) => {
                    for r in &recs {
                        if let Err(e) = publish(&broker, r) {
                            eprintln!("[cdc {}] publish: {e}", s.verb());
                        }
                    }
                }
                Err(e) => eprintln!("[cdc {}] poll: {e}", s.verb()),
            }
        }
        tokio::time::sleep(interval).await;
    }
}
