//! The push-shaped source seam, and the snapshot / unusable-position policy.
//!
//! # Why this is a stream and not a `poll()`
//!
//! The seam this replaces (`EventSource` in `examples/egg-economy/src/source.rs`)
//! was pull-shaped: `async fn poll(&self) -> Vec<Record>`, called on a timer.
//! That shape had exactly one implementation — a poller — and it *could* only
//! ever have had pollers, because a caller driving the clock cannot express
//! "the database will tell you." A real change feed is a Mongo `watch()`
//! cursor, a Postgres replication connection, a filesystem notification: the
//! store decides when there is something to say.
//!
//! So [`CommitSource::changes`] hands back a [`ChangeStream`] and the source
//! owns delivery. A source with nothing to report simply does not yield, and
//! the connector loop parks on `.next()` rather than sleeping and asking again.

use crate::record::ChangeRecord;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Where a source should start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    /// No stored position — first ever start for this connector+topic.
    Cold,
    /// A position previously read out of a record's `source.position` and
    /// committed to the offset store.
    At(String),
}

/// Debezium's snapshot modes, and with them the unusable-position policy.
///
/// This is the same reasoning `meshql-changes`'s streamlette applies to a
/// `Last-Event-ID` it cannot honour: an unusable cursor must never become a
/// silent skip. There it degrades a connection to live-only and *says so*;
/// here the equivalent choices are "start over from history" or "refuse to
/// start", and which one a deployment wants is a domain decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotMode {
    /// Snapshot on a cold start; stream thereafter. An unusable stored
    /// position is a **hard error** — the operator asked for exactly one
    /// snapshot and silently taking a second one would republish the whole
    /// history without anyone deciding to.
    Initial,
    /// Never snapshot: stream from wherever the source's live position is on a
    /// cold start. History that predates the connector is *not* captured, and
    /// an unusable stored position is a **hard error**. Correct only when
    /// something else already loaded history.
    Never,
    /// Snapshot on a cold start, and again whenever a stored position has
    /// become unusable — a dropped replication slot, an expired Mongo resume
    /// token, a rebuilt database. This is the mode that keeps running through
    /// the failure the other two stop on, at the cost of re-emitting history.
    /// Safe because folds are idempotent over a whole log.
    WhenNeeded,
}

impl SnapshotMode {
    /// Whether a cold start should snapshot existing rows.
    pub fn snapshots_on_cold_start(&self) -> bool {
        matches!(self, SnapshotMode::Initial | SnapshotMode::WhenNeeded)
    }

    /// Whether an unusable stored position may be recovered by re-snapshotting
    /// rather than failing the connector.
    pub fn recovers_from_unusable_position(&self) -> bool {
        matches!(self, SnapshotMode::WhenNeeded)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CdcError {
    /// The stored position cannot be honoured: the slot is gone, the resume
    /// token has aged out of the oplog, the database was rebuilt. **Never
    /// return this and then start anyway from somewhere else** — that is the
    /// silent skip this whole type exists to prevent. The connector decides
    /// what to do about it, per [`SnapshotMode`].
    #[error(
        "{connector}: stored position {position:?} is unusable ({reason}); \
             snapshot_mode decides whether to re-snapshot or stop"
    )]
    UnusablePosition {
        connector: &'static str,
        position: String,
        reason: String,
    },

    /// The change *notification* channel could not be established — no
    /// inotify, no replication slot, no `watch()`. Distinct from a transient
    /// backend error precisely so it can be made fatal: a connector that
    /// cannot be notified must not quietly become a poller.
    #[error("{connector}: cannot establish a change feed: {reason}")]
    NoFeed {
        connector: &'static str,
        reason: String,
    },

    #[error(transparent)]
    Backend(#[from] anyhow::Error),
}

/// A live feed of committed writes. Ends only on a fatal error.
pub type ChangeStream = Pin<Box<dyn Stream<Item = Result<ChangeRecord, CdcError>> + Send>>;

/// One store, watched.
#[async_trait]
pub trait CommitSource: Send + Sync {
    /// `"sqlite"`, `"mongodb"`, `"postgresql"` — copied into
    /// `SourceInfo::connector`.
    fn connector(&self) -> &'static str;

    /// The table or collection being watched.
    fn entity(&self) -> &str;

    /// Open the feed.
    ///
    /// Implementations **must** follow snapshot-then-stream when `from` is
    /// [`Resume::Cold`] and `mode` snapshots: capture the streaming position
    /// *first*, emit existing rows with [`crate::Op::Read`] and
    /// `source.snapshot == true`, then switch to live records from the
    /// captured position. Capturing first is what removes the gap; the price
    /// is that writes landing during the snapshot may be emitted twice, which
    /// at-least-once permits.
    ///
    /// If `from` is [`Resume::At`] and the position cannot be honoured, return
    /// [`CdcError::UnusablePosition`] rather than starting from anywhere else.
    async fn changes(&self, from: Resume, mode: SnapshotMode) -> Result<ChangeStream, CdcError>;

    /// Every record up to and including `position` is now on the topic **and**
    /// committed to the offset store.
    ///
    /// This is the flush confirmation every real replication protocol has, and
    /// it exists because some sources hold the data until told to let go.
    /// PostgreSQL is the sharp case: a replication slot retains WAL until its
    /// `confirmed_flush_lsn` advances, and advancing it is *destructive* — the
    /// server may then recycle that WAL. Advancing when the records were merely
    /// *read* rather than *made durable* is a silent gap: read LSNs 1–5, append
    /// 1–2, crash, and 3–5 no longer exist anywhere.
    ///
    /// So the connector calls this only after an offset commit has actually
    /// hit the disk. Sources that hold nothing back — SQLite, MongoDB — take
    /// the default and do nothing.
    async fn durable_through(&self, _position: &str) -> Result<(), CdcError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_start_snapshot_policy() {
        assert!(SnapshotMode::Initial.snapshots_on_cold_start());
        assert!(SnapshotMode::WhenNeeded.snapshots_on_cold_start());
        assert!(!SnapshotMode::Never.snapshots_on_cold_start());
    }

    /// Only `when_needed` may turn an unusable position into a re-snapshot.
    /// If `initial` ever did, an operator who asked for one snapshot would get
    /// the entire history republished by a transient failure, with nothing in
    /// the configuration having said so.
    #[test]
    fn only_when_needed_recovers_from_an_unusable_position() {
        assert!(SnapshotMode::WhenNeeded.recovers_from_unusable_position());
        assert!(!SnapshotMode::Initial.recovers_from_unusable_position());
        assert!(!SnapshotMode::Never.recovers_from_unusable_position());
    }

    #[test]
    fn snapshot_modes_parse_from_debeziums_names() {
        for (name, mode) in [
            ("initial", SnapshotMode::Initial),
            ("never", SnapshotMode::Never),
            ("when_needed", SnapshotMode::WhenNeeded),
        ] {
            let parsed: SnapshotMode =
                serde_json::from_value(serde_json::Value::String(name.into())).unwrap();
            assert_eq!(parsed, mode);
        }
    }
}
