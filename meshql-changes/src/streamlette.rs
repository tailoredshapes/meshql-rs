//! A per-meshlette SSE surface: one entity, one hub, one pump.
//!
//! Distinct from `changes_router`'s deployment-level `/changes` feed, which
//! stays exactly as it is — this adds a pump model, it does not replace one.

use crate::{ChangeEvent, ChangeSource};
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

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
}
