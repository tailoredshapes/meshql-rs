//! ChangeHub: fan change events out to SSE subscribers. A thin wrapper
//! over tokio::sync::broadcast. `run_tails` is the pump: poll every
//! source round-robin and publish — the shape of egg-economy's
//! `run_connector`. Poll *errors* are logged and retried next interval,
//! never fatal. A *panicking* source, however, kills the whole pump task
//! (open SSE connections would then receive only heartbeats) — keep
//! `ChangeSource::poll` panic-free; return Err instead.

use crate::{ChangeEvent, ChangeSource};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct ChangeHub {
    tx: broadcast::Sender<ChangeEvent>,
}

impl ChangeHub {
    /// `capacity` is the per-subscriber buffer; a subscriber that falls
    /// more than `capacity` events behind is lagged and gets its stream
    /// closed by the SSE layer (correctness over continuity).
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn publish(&self, event: ChangeEvent) {
        // Err means no subscribers — not an error for a notification hub.
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ChangeEvent> {
        self.tx.subscribe()
    }
}

/// Poll every source and publish new events, forever. Spawn this.
pub async fn run_tails(hub: ChangeHub, sources: Vec<Arc<dyn ChangeSource>>, interval: Duration) {
    loop {
        for source in &sources {
            match source.poll().await {
                Ok(events) => {
                    for event in events {
                        hub.publish(event);
                    }
                }
                Err(e) => eprintln!("[changes {}] poll: {e}", source.entity()),
            }
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeEvent;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ev(id: &str) -> ChangeEvent {
        ChangeEvent {
            entity: "hen".into(),
            id: id.into(),
            created_at: 1,
            deleted: false,
            auth: Default::default(),
            cursor: None,
            payload: None,
        }
    }

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let hub = ChangeHub::new(16);
        let mut rx = hub.subscribe();
        hub.publish(ev("a"));
        let got = rx.recv().await.unwrap();
        assert_eq!(got.id, "a");
    }

    /// A scripted source: emits one event on its first poll, errors on the
    /// second, then goes quiet.
    struct OneShot {
        polls: AtomicUsize,
    }

    #[async_trait]
    impl ChangeSource for OneShot {
        fn entity(&self) -> &str {
            "hen"
        }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            match self.polls.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(vec![ev("from-tail")]),
                1 => Err(anyhow::anyhow!("transient poll failure")),
                _ => Ok(vec![]),
            }
        }
    }

    #[tokio::test]
    async fn run_tails_pumps_sources_into_hub_and_survives_errors() {
        let hub = ChangeHub::new(16);
        let mut rx = hub.subscribe();
        let source = Arc::new(OneShot {
            polls: AtomicUsize::new(0),
        });

        let handle = tokio::spawn(run_tails(
            hub.clone(),
            vec![source.clone()],
            Duration::from_millis(10),
        ));

        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event within 2s")
            .unwrap();
        assert_eq!(got.id, "from-tail");

        // Give the loop time to hit the error poll and keep going.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            source.polls.load(Ordering::SeqCst) >= 3,
            "loop survived the error"
        );
        handle.abort();
    }
}
