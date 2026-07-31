//! Durable offset storage — Debezium's "periodically committed offsets".
//!
//! One file per connector, holding the last native position whose record has
//! been appended to the topic. The ordering rule is the entire correctness
//! argument:
//!
//! 1. append the record to merkql,
//! 2. *then* commit the position.
//!
//! A crash between the two replays the record — a duplicate, which an
//! idempotent fold absorbs. Committing first would skip it, and a skipped
//! record is a hole in a log that projections are folded from, which nothing
//! downstream can detect or repair. Duplicates are cheap; gaps are permanent.
//!
//! Commits are *periodic* rather than per-record for the same reason a
//! duplicate is acceptable at all: the only cost of a stale committed position
//! is replaying the records after it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Stored {
    connector: String,
    entity: String,
    position: String,
    /// True while the initial snapshot is still in progress. A position
    /// committed mid-snapshot is not a streaming position and must not be
    /// resumed from as one — see `OffsetStore::resume`.
    #[serde(default)]
    snapshot_in_progress: bool,
}

/// The connector's durable position.
pub struct OffsetStore {
    path: PathBuf,
    connector: String,
    entity: String,
    stored: Option<Stored>,
    /// Staged but not yet written — flushed by `maybe_commit` on the interval,
    /// or by `commit_now`.
    pending: Option<Stored>,
    interval: Duration,
    last_commit: Instant,
}

impl OffsetStore {
    /// Open (or start) the offset file for one connector+entity pair.
    pub fn open(
        path: impl Into<PathBuf>,
        connector: &str,
        entity: &str,
        interval: Duration,
    ) -> Result<Self> {
        let path = path.into();
        let stored = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let stored: Stored = serde_json::from_str(&text)
                    .with_context(|| format!("parsing offset file {}", path.display()))?;
                // A file written by a different connector or for a different
                // entity is not a position we can use; treating it as one
                // would resume a Mongo token as a SQLite rowid. Loud, because
                // it means the deployment is misconfigured.
                if stored.connector != connector || stored.entity != entity {
                    anyhow::bail!(
                        "offset file {} holds a position for {}/{}, but this connector is {}/{}",
                        path.display(),
                        stored.connector,
                        stored.entity,
                        connector,
                        entity
                    );
                }
                Some(stored)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| format!("reading offset file {}", path.display()))
            }
        };

        Ok(Self {
            path,
            connector: connector.to_string(),
            entity: entity.to_string(),
            stored,
            pending: None,
            interval,
            last_commit: Instant::now(),
        })
    }

    /// Where the source should start.
    ///
    /// A position committed while a snapshot was still running names a row
    /// *inside* the snapshot, not a place in the live stream, so it must never
    /// be handed back as [`crate::Resume::At`] — resuming the live stream from
    /// it would begin at a point the stream never reached and skip everything
    /// between.
    ///
    /// It is handed back as [`crate::Resume::Snapshotting`] instead of being
    /// discarded. The distinction is the source's to act on: one that cannot
    /// continue a partial snapshot calls
    /// [`crate::Resume::without_snapshot_resume`] and restarts it, which is
    /// what every source did before the variant existed; one that can, resumes
    /// and skips re-reading history it already emitted. Either way this method
    /// no longer decides on the source's behalf, and no source has to
    /// mis-stage `snapshot_in_progress` to get the position back.
    pub fn resume(&self) -> crate::Resume {
        match &self.stored {
            Some(s) if s.snapshot_in_progress => crate::Resume::Snapshotting(s.position.clone()),
            Some(s) => crate::Resume::At(s.position.clone()),
            None => crate::Resume::Cold,
        }
    }

    /// Stage a position. Nothing is written until an interval elapses
    /// (`maybe_commit`) or `commit_now` is called.
    pub fn stage(&mut self, position: &str, snapshot_in_progress: bool) {
        self.pending = Some(Stored {
            connector: self.connector.clone(),
            entity: self.entity.clone(),
            position: position.to_string(),
            snapshot_in_progress,
        });
    }

    /// Write the staged position if the commit interval has elapsed.
    ///
    /// Returns the position actually written, if any — the caller needs that
    /// to tell a source what has become durable (see
    /// [`crate::CommitSource::durable_through`]), and "what got committed" is
    /// not the same as "what was staged" when the interval has not elapsed.
    pub fn maybe_commit(&mut self) -> Result<Option<String>> {
        if self.pending.is_some() && self.last_commit.elapsed() >= self.interval {
            return self.commit_now();
        }
        Ok(None)
    }

    /// Write the staged position immediately. Returns what was written.
    pub fn commit_now(&mut self) -> Result<Option<String>> {
        let Some(pending) = self.pending.take() else {
            return Ok(None);
        };
        self.write(&pending)?;
        let position = pending.position.clone();
        self.stored = Some(pending);
        self.last_commit = Instant::now();
        Ok(Some(position))
    }

    /// Atomic replace: write a sibling temp file, fsync it, rename over the
    /// target, then fsync the directory. Without the directory fsync the
    /// rename itself can be lost on a crash even though the file's contents
    /// were durable, which yields a *stale* position — a replay, so still not
    /// a gap, but the whole point of the file is to bound how much replays.
    fn write(&self, stored: &Stored) -> Result<()> {
        use std::io::Write;

        let dir = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("offset path {} has no parent", self.path.display()))?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating offset directory {}", dir.display()))?;

        let tmp = self.path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)
                .with_context(|| format!("creating {}", tmp.display()))?;
            f.write_all(serde_json::to_string(stored)?.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming {} onto {}", tmp.display(), self.path.display()))?;
        // Best-effort: some filesystems refuse an fsync on a directory handle.
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Resume;
    use std::path::Path;

    fn store(dir: &Path) -> OffsetStore {
        OffsetStore::open(
            dir.join("offsets.json"),
            "sqlite",
            "envelopes",
            Duration::from_millis(0),
        )
        .unwrap()
    }

    #[test]
    fn a_fresh_store_is_a_cold_start() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(store(dir.path()).resume(), Resume::Cold);
    }

    #[test]
    fn a_committed_position_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path());
        s.stage("42", false);
        s.commit_now().unwrap();
        drop(s);

        assert_eq!(store(dir.path()).resume(), Resume::At("42".to_string()));
    }

    /// A staged-but-uncommitted position must NOT survive — it names records
    /// whose append may not have happened. Losing it replays; keeping it could
    /// skip.
    #[test]
    fn a_staged_position_is_not_durable_until_committed() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path());
        s.stage("42", false);
        drop(s);

        assert_eq!(store(dir.path()).resume(), Resume::Cold);
    }

    /// A mid-snapshot position comes back as `Snapshotting`, never as `At`.
    ///
    /// The distinction it draws is the whole safety property: handing this
    /// position back as a *streaming* position would begin the live feed at a
    /// place it never reached and drop everything before it. Handing it back
    /// as `Snapshotting` lets a source that can continue an ordered walk do
    /// so, while a source that cannot calls `without_snapshot_resume()` and
    /// gets the historical restart.
    #[test]
    fn a_position_committed_mid_snapshot_resumes_as_snapshotting_not_as_a_stream() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path());
        s.stage("17", true);
        s.commit_now().unwrap();
        drop(s);

        let resumed = store(dir.path()).resume();
        assert_eq!(
            resumed,
            Resume::Snapshotting("17".to_string()),
            "a mid-snapshot position must be offered as a snapshot cursor"
        );
        assert!(
            !matches!(resumed, Resume::At(_)),
            "a mid-snapshot position must never be a streaming position"
        );
        // And the opt-out still yields exactly the old behaviour.
        assert_eq!(resumed.without_snapshot_resume(), Resume::Cold);
    }

    #[test]
    fn the_interval_gates_commits() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = OffsetStore::open(
            dir.path().join("offsets.json"),
            "sqlite",
            "envelopes",
            Duration::from_secs(3600),
        )
        .unwrap();
        s.stage("42", false);
        s.maybe_commit().unwrap();
        assert_eq!(
            store(dir.path()).resume(),
            Resume::Cold,
            "interval not elapsed"
        );

        s.commit_now().unwrap();
        assert_eq!(store(dir.path()).resume(), Resume::At("42".to_string()));
    }

    /// A position belonging to another connector must not be silently adopted
    /// — a Mongo resume token read as a SQLite rowid would parse as garbage
    /// and start the feed somewhere arbitrary.
    #[test]
    fn a_foreign_offset_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path());
        s.stage("42", false);
        s.commit_now().unwrap();

        let err = match OffsetStore::open(
            dir.path().join("offsets.json"),
            "mongodb",
            "envelopes",
            Duration::from_millis(0),
        ) {
            Ok(_) => panic!("a sqlite offset file must not be usable by the mongo connector"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("sqlite"), "got: {err}");
    }
}
