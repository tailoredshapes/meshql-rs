//! A per-meshlette SSE surface: one entity, one hub, one pump.
//!
//! Distinct from `changes_router`'s deployment-level `/changes` feed, which
//! stays exactly as it is — this adds a pump model, it does not replace one.

use crate::{ChangeEvent, ChangeHub, ChangeSource};
use std::sync::Arc;
use std::time::Duration;

/// A source that can replay history from a cursor. Implemented by
/// `meshql-merkql`'s log-backed source; kept as a trait here so
/// `meshql-changes` needs no merkql dependency.
#[async_trait::async_trait]
pub trait SeekableSource: ChangeSource {
    /// Events strictly after `cursor`, in log order.
    async fn backfill(&self, cursor: &str) -> anyhow::Result<Vec<ChangeEvent>>;
    /// Whether `cursor` is usable. An unusable cursor degrades the
    /// connection to `resume: false` — never a silent skip.
    fn cursor_is_valid(&self, cursor: &str) -> bool;
}

/// Where a streamlette's events come from, and how often we go looking.
pub enum StreamSource {
    /// Poll-diff an existing store via `SearcherTail`. Every backend.
    /// No resume, no payload.
    Tail {
        source: Arc<dyn ChangeSource>,
        poll_interval: Duration,
    },
    /// A log-backed source that supports resume and may carry payloads.
    Seekable {
        source: Arc<dyn SeekableSource>,
        poll_interval: Duration,
    },
}

impl StreamSource {
    /// How long the pump sleeps between polls. Both variants poll — the
    /// difference between them is resume, not cadence.
    pub fn poll_interval(&self) -> Duration {
        match self {
            Self::Tail { poll_interval, .. } => *poll_interval,
            Self::Seekable { poll_interval, .. } => *poll_interval,
        }
    }

    /// Whether a client may reconnect with `Last-Event-ID` and be backfilled.
    pub fn supports_resume(&self) -> bool {
        matches!(self, Self::Seekable { .. })
    }
}

/// One entity's SSE surface.
pub struct StreamletteConfig {
    /// e.g. "/message_posted/stream"
    pub path: String,
    /// the `ChangeEvent.entity` this stream carries
    pub entity: String,
    pub source: StreamSource,
    /// Broadcast buffer. Payload-carrying streams lag sooner at the same
    /// capacity, so size this up when payloads are on.
    pub hub_capacity: usize,
}

/// Drive one source into one hub. One task per streamlette, so a slow
/// source never blocks a fast one — the reason this is not `run_tails`,
/// which shares a single round-robin task and one interval across every
/// source it owns. A log-backed source is cheap enough to poll
/// aggressively (an in-memory offset compare); a `SearcherTail` is not
/// (a full `find_all` plus payload-hash diff). Sharing a loop between
/// them makes the per-source interval inert.
///
/// Loops forever by design — the caller owns the `tokio::spawn` handle.
pub async fn run_pump(source: Arc<dyn ChangeSource>, hub: ChangeHub, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match source.poll().await {
            Ok(events) => {
                for event in events {
                    hub.publish(event);
                }
            }
            // A transient source error must not kill the pump — the next
            // tick retries. Subscribers see a gap, which the lagged/refetch
            // path already covers.
            Err(e) => eprintln!("[streamlette {}] poll: {e}", source.entity()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChangeHub;
    use async_trait::async_trait;

    fn ev(id: &str) -> ChangeEvent {
        ChangeEvent {
            entity: "hen".into(),
            id: id.into(),
            created_at: 1,
            deleted: false,
            authorized_tokens: vec![],
            cursor: None,
            payload: None,
        }
    }

    struct StubSource;

    #[async_trait]
    impl ChangeSource for StubSource {
        fn entity(&self) -> &str {
            "hen"
        }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
    }

    struct StubSeekable;

    #[async_trait]
    impl ChangeSource for StubSeekable {
        fn entity(&self) -> &str {
            "hen"
        }
        async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl SeekableSource for StubSeekable {
        async fn backfill(&self, _cursor: &str) -> anyhow::Result<Vec<ChangeEvent>> {
            Ok(vec![])
        }
        fn cursor_is_valid(&self, cursor: &str) -> bool {
            cursor.contains(':')
        }
    }

    fn tail() -> StreamSource {
        StreamSource::Tail {
            source: Arc::new(StubSource),
            poll_interval: Duration::from_millis(250),
        }
    }

    fn seekable() -> StreamSource {
        StreamSource::Seekable {
            source: Arc::new(StubSeekable),
            poll_interval: Duration::from_millis(50),
        }
    }

    #[test]
    fn tail_does_not_support_resume() {
        assert!(!tail().supports_resume());
    }

    #[test]
    fn seekable_supports_resume() {
        assert!(seekable().supports_resume());
    }

    #[test]
    fn poll_interval_reads_through_both_variants() {
        assert_eq!(tail().poll_interval(), Duration::from_millis(250));
        assert_eq!(seekable().poll_interval(), Duration::from_millis(50));
    }

    /// Task 6 needs to hand a `Seekable`'s source to the shared pump, which
    /// takes `Arc<dyn ChangeSource>`. Pin the upcast here so a regression
    /// shows up as a compile failure in this crate, not downstream.
    #[test]
    fn seekable_source_upcasts_to_change_source() {
        let seekable: Arc<dyn SeekableSource> = Arc::new(StubSeekable);
        let base: Arc<dyn ChangeSource> = seekable;
        assert_eq!(base.entity(), "hen");
    }

    #[tokio::test]
    async fn pump_publishes_polled_events_to_the_hub() {
        use std::sync::Mutex;

        struct BatchSource {
            batches: Mutex<Vec<Vec<ChangeEvent>>>,
        }
        #[async_trait]
        impl ChangeSource for BatchSource {
            fn entity(&self) -> &str {
                "hen"
            }
            async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
                Ok(self.batches.lock().unwrap().pop().unwrap_or_default())
            }
        }

        let hub = ChangeHub::new(16);
        let mut rx = hub.subscribe();
        let source = Arc::new(BatchSource {
            batches: Mutex::new(vec![vec![ev("e1")]]),
        });

        tokio::spawn(run_pump(source, hub.clone(), Duration::from_millis(5)));

        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("pump must publish within 2s")
            .unwrap();
        assert_eq!(got.id, "e1");
    }

    /// A transient storage blip must not silently stop a stream forever.
    #[tokio::test]
    async fn pump_survives_a_source_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FlakySource {
            polls: AtomicUsize,
        }
        #[async_trait]
        impl ChangeSource for FlakySource {
            fn entity(&self) -> &str {
                "hen"
            }
            async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
                match self.polls.fetch_add(1, Ordering::SeqCst) {
                    0 => Err(anyhow::anyhow!("transient poll failure")),
                    1 => Ok(vec![ev("after-error")]),
                    _ => Ok(vec![]),
                }
            }
        }

        let hub = ChangeHub::new(16);
        let mut rx = hub.subscribe();
        let source = Arc::new(FlakySource {
            polls: AtomicUsize::new(0),
        });

        tokio::spawn(run_pump(source, hub.clone(), Duration::from_millis(5)));

        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("pump must keep polling after an error")
            .unwrap();
        assert_eq!(got.id, "after-error");
    }
}
