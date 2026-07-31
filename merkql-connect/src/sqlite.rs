//! SQLite CDC: the filesystem is the notification, SQLite is the reader.
//!
//! # Why not `sqlite3_update_hook`
//!
//! `update_hook`/`commit_hook` fire on the **connection performing the write**.
//! They are in-process, in-connection: a separate process holding its own
//! connection to the same file is never called. `merkql-connect` is a separate
//! deployable by design (see the crate docs — that is what makes it merkql's
//! sole writer), so the hooks are simply not available to it. This is not a
//! fallback; the hooks were never reachable from where this code runs.
//!
//! # Why not WAL parsing
//!
//! SQLite's WAL is a **physical**, page-level log. It holds modified database
//! pages, not rows — decoding envelopes out of it means reimplementing
//! SQLite's b-tree layout and tracking checkpoints, and it breaks whenever the
//! file format or the page layout shifts. It is the classic wrong turn here.
//!
//! We need the WAL for exactly one bit of information: **something changed.**
//!
//! # The design
//!
//! 1. Watch the database file and its `-wal`/`-journal` siblings with inotify
//!    (via `notify`). A write to the database modifies one of them, and that
//!    modification is the wake-up edge. No timer, no poll loop.
//! 2. When woken, read new rows over an ordinary SQLite connection. A reader
//!    in WAL mode sees a consistent snapshot of **committed** data only, so
//!    uncommitted and rolled-back work is invisible for free — this is the
//!    elegance of reading through SQLite rather than under it.
//! 3. The cursor is the **rowid**.
//!
//! # Why rowid is the correct cursor here
//!
//! SQLite permits exactly one write transaction at a time — writers are
//! serialised even in WAL mode. `rowid` is assigned inside that serialised
//! write transaction as `max(rowid) + 1`, so on an **append-only** table
//! *rowid order is commit order, exactly*. `WHERE rowid > ?` is therefore
//! complete and gap-free, which a `created_at` watermark is not: `created_at`
//! is stamped by the application when the Envelope is constructed, before the
//! insert, so two requests can commit in the opposite order to their
//! timestamps and a timestamp watermark would skip the straggler.
//!
//! This is **not** the "physical row id as an ordering authority" anti-pattern
//! that `domain-design.md` warns about. That warning is about reconstructing a
//! total order *before publishing*; the order on the topic here is still just
//! the order of appends. rowid is used only as a **resume position** — the
//! direct analogue of a Postgres LSN or a Mongo resume token, which the other
//! two sources in this crate use for the same purpose.
//!
//! Because rows are append-only, a row identified by a rowid never mutates, so
//! reading it *after* the commit that created it always yields exactly what
//! was committed. In a mutable schema that would be unsound — you would read a
//! later version than the change you were told about.

use crate::record::{ChangeRecord, Op, Snapshot, SourceInfo};
use crate::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use async_trait::async_trait;
use chrono::DateTime;
use futures::stream;
use meshql_core::{Envelope, Stash};
use notify::{RecursiveMode, Watcher};
use sqlx::{Row, SqlitePool};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

const CONNECTOR: &str = "sqlite";

/// A SQLite envelope table, watched.
pub struct SqliteSource {
    pool: SqlitePool,
    db_path: PathBuf,
    table: String,
    entity: String,
}

/// Reject anything that is not a plain identifier. The table name is
/// interpolated into SQL (SQLite has no bind parameter for identifiers), so
/// this is the boundary that keeps a config file from becoming an injection.
fn validate_identifier(name: &str) -> Result<(), CdcError> {
    let ok = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.chars().next().unwrap().is_ascii_digit();
    if ok {
        Ok(())
    } else {
        Err(CdcError::Backend(anyhow::anyhow!(
            "'{name}' is not a valid SQLite table name (letters, digits and underscore only)"
        )))
    }
}

impl SqliteSource {
    /// Open a read connection to `db_path` and prepare to watch it.
    ///
    /// `entity` is the logical mesh name reported in `source.entity`; `table`
    /// is the physical table holding envelopes.
    pub async fn open(
        db_path: impl AsRef<Path>,
        table: &str,
        entity: &str,
    ) -> Result<Self, CdcError> {
        validate_identifier(table)?;
        let db_path = db_path.as_ref().to_path_buf();

        // An in-memory database has no file to watch, and no other process
        // could reach it anyway. Loud, because the alternative is a connector
        // that starts cleanly and is never notified of anything.
        if !db_path.exists() {
            return Err(CdcError::NoFeed {
                connector: CONNECTOR,
                reason: format!(
                    "{} does not exist; merkql-connect watches a real database file and \
                     cannot observe an in-memory or not-yet-created database",
                    db_path.display()
                ),
            });
        }

        let pool = SqlitePool::connect(&format!("sqlite:{}", db_path.display()))
            .await
            .map_err(|e| {
                CdcError::Backend(anyhow::anyhow!("opening {}: {e}", db_path.display()))
            })?;

        Ok(Self {
            pool,
            db_path,
            table: table.to_string(),
            entity: entity.to_string(),
        })
    }

    async fn max_rowid(&self) -> Result<i64, CdcError> {
        let sql = format!("SELECT COALESCE(MAX(rowid), 0) AS m FROM {}", self.table);
        let row = sqlx::query(&sql)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading max rowid: {e}")))?;
        row.try_get::<i64, _>("m")
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading max rowid: {e}")))
    }

    /// Build the inotify watcher, or fail loudly.
    ///
    /// **No silent fallback to a timer.** If the platform has no usable
    /// filesystem notification — an unsupported filesystem, an inotify watch
    /// limit, a build where `notify` resolves to its polling watcher — this
    /// returns [`CdcError::NoFeed`] and the connector refuses to start. A
    /// connector that quietly degrades to polling is a connector whose latency
    /// and load characteristics silently stop matching what was deployed.
    fn watch(&self) -> Result<(notify::RecommendedWatcher, Receiver), CdcError> {
        if notify::RecommendedWatcher::kind() == notify::WatcherKind::PollWatcher {
            return Err(CdcError::NoFeed {
                connector: CONNECTOR,
                reason: "notify resolved to its polling watcher on this platform; \
                         merkql-connect requires real filesystem notifications and will \
                         not silently degrade to a timer"
                    .into(),
            });
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // Any event on the watched files is only ever a hint that there
            // may be new committed rows; the reader decides what is actually
            // there. So an error here is not fatal — it just means we might
            // have missed an edge, and the next edge will pick it up.
            if res.is_ok() {
                let _ = tx.send(());
            }
        })
        .map_err(|e| CdcError::NoFeed {
            connector: CONNECTOR,
            reason: format!("creating a filesystem watcher: {e}"),
        })?;

        // Watch the *directory*, not the files: SQLite deletes and recreates
        // `-wal` and `-shm` at checkpoint boundaries, and a watch on a deleted
        // inode goes quiet forever. A non-recursive directory watch survives
        // that, at the cost of some events for unrelated files — which cost
        // nothing, because an event only ever triggers a read that finds
        // nothing.
        let dir = self.db_path.parent().ok_or_else(|| CdcError::NoFeed {
            connector: CONNECTOR,
            reason: format!(
                "{} has no parent directory to watch",
                self.db_path.display()
            ),
        })?;
        watcher
            .watch(dir, RecursiveMode::NonRecursive)
            .map_err(|e| CdcError::NoFeed {
                connector: CONNECTOR,
                reason: format!("watching {}: {e}", dir.display()),
            })?;

        Ok((watcher, rx))
    }

    fn row_to_record(
        &self,
        row: &sqlx::sqlite::SqliteRow,
        snapshot: Snapshot,
    ) -> Result<(i64, ChangeRecord), CdcError> {
        let bad = |e: sqlx::Error| CdcError::Backend(anyhow::anyhow!("decoding envelope row: {e}"));

        let rowid: i64 = row.try_get("rowid").map_err(bad)?;
        let id: String = row.try_get("id").map_err(bad)?;
        let created_at_ms: i64 = row.try_get("created_at_ms").map_err(bad)?;
        let deleted: i64 = row.try_get("deleted").map_err(bad)?;
        let tokens_json: String = row.try_get("authorized_tokens").map_err(bad)?;
        let payload_json: String = row.try_get("payload").map_err(bad)?;

        let authorized_tokens: Vec<String> = serde_json::from_str(&tokens_json)
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("decoding authorized_tokens: {e}")))?;
        let payload: Stash = serde_json::from_str(&payload_json)
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("decoding payload: {e}")))?;

        let envelope = Envelope {
            id,
            payload,
            created_at: DateTime::from_timestamp_millis(created_at_ms).unwrap_or_default(),
            deleted: deleted != 0,
            authorized_tokens,
        };

        let record = ChangeRecord::new(
            if snapshot.is_snapshot() {
                Op::Read
            } else {
                Op::Create
            },
            envelope,
            SourceInfo {
                connector: CONNECTOR.to_string(),
                entity: self.entity.clone(),
                ts_ms: created_at_ms,
                position: Some(rowid.to_string()),
                snapshot,
            },
        );
        Ok((rowid, record))
    }
}

type Receiver = tokio::sync::mpsc::UnboundedReceiver<()>;

/// The feed's state machine, threaded through `stream::unfold`.
struct Feed {
    source: SqliteSource,
    cursor: i64,
    /// `Some(end)` while emitting the snapshot: rows up to and including
    /// `end`, the position captured *before* the snapshot began. Once drained
    /// this becomes `None` and the feed switches to streaming past `end`.
    snapshot_end: Option<i64>,
    buffer: VecDeque<ChangeRecord>,
    rx: Receiver,
    /// Dropping the watcher stops the notifications, so it must live as long
    /// as the feed even though nothing reads it.
    _watcher: notify::RecommendedWatcher,
}

impl Feed {
    /// Rows after the cursor, bounded by the snapshot end when snapshotting.
    ///
    /// The row sitting exactly on `snapshot_end` is tagged `Snapshot::Last`:
    /// it is the point at which the snapshot has covered everything it
    /// promised, and therefore the first position in the snapshot that a live
    /// stream may resume from.
    async fn fetch(&self) -> Result<Vec<(i64, ChangeRecord)>, CdcError> {
        let sql = match self.snapshot_end {
            Some(_) => format!(
                "SELECT rowid, id, created_at_ms, deleted, authorized_tokens, payload \
                 FROM {} WHERE rowid > ? AND rowid <= ? ORDER BY rowid LIMIT 500",
                self.source.table
            ),
            None => format!(
                "SELECT rowid, id, created_at_ms, deleted, authorized_tokens, payload \
                 FROM {} WHERE rowid > ? ORDER BY rowid LIMIT 500",
                self.source.table
            ),
        };

        let mut query = sqlx::query(&sql).bind(self.cursor);
        if let Some(end) = self.snapshot_end {
            query = query.bind(end);
        }
        let rows = query
            .fetch_all(&self.source.pool)
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading new envelopes: {e}")))?;

        rows.iter()
            .map(|r| {
                let rowid: i64 = r
                    .try_get("rowid")
                    .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading rowid: {e}")))?;
                let flag = match self.snapshot_end {
                    Some(end) if rowid == end => Snapshot::Last,
                    Some(_) => Snapshot::True,
                    None => Snapshot::False,
                };
                self.source.row_to_record(r, flag)
            })
            .collect()
    }
}

#[async_trait]
impl CommitSource for SqliteSource {
    fn connector(&self) -> &'static str {
        CONNECTOR
    }

    fn entity(&self) -> &str {
        &self.entity
    }

    async fn changes(&self, from: Resume, mode: SnapshotMode) -> Result<ChangeStream, CdcError> {
        // Establish the notification channel BEFORE reading anything. A write
        // landing between the read and the watch would otherwise produce no
        // edge and no row — the one arrangement that loses a record silently.
        let (watcher, rx) = self.watch()?;

        let max_rowid = self.max_rowid().await?;

        let (cursor, snapshot_end) = match from {
            Resume::At(position) => {
                let rowid: i64 = position.parse().map_err(|_| CdcError::UnusablePosition {
                    connector: CONNECTOR,
                    position: position.clone(),
                    reason: "not a SQLite rowid".into(),
                })?;
                // A stored position past the end of the table means the table
                // was truncated, rebuilt, or restored from an older backup.
                // Resuming from it would wait forever; silently restarting
                // from 0 or from the tail would double-publish or skip. The
                // snapshot-mode policy decides, not this function.
                if rowid > max_rowid {
                    return Err(CdcError::UnusablePosition {
                        connector: CONNECTOR,
                        position,
                        reason: format!(
                            "rowid is past the end of {} (max rowid {max_rowid}); the table \
                             has been truncated, rebuilt or restored",
                            self.table
                        ),
                    });
                }
                (rowid, None)
            }
            // Snapshot-then-stream. The streaming position is captured first
            // (`max_rowid`, read above), the existing rows up to it are
            // emitted as `op: r`, and streaming resumes from exactly that
            // point — so there is no gap between the two phases, and no
            // window in which a row is neither snapshotted nor streamed.
            Resume::Cold if mode.snapshots_on_cold_start() => (0, Some(max_rowid)),
            // `never`: start at the live tail, ignoring history.
            Resume::Cold => (max_rowid, None),

            // An interrupted snapshot **is** resumable here, because the
            // snapshot is an ordered walk of the primary key: the stored rowid
            // was emitted, so everything after it is exactly what remains. The
            // alternative — restarting at 0 — re-emits the whole table.
            //
            // `max_rowid` is re-read on this path rather than restored, so it
            // may exceed the value the interrupted run captured. Rows written
            // in between are then delivered as part of the *snapshot* (`op: r`)
            // instead of the live stream (`op: c`). That is a deliberate
            // trade: the alternative is a gap, and a consumer that treats `r`
            // and `c` differently is only distinguishing backfill from live
            // traffic, which is precisely what this record is.
            Resume::Snapshotting(position) if mode.snapshots_on_cold_start() => {
                let rowid: i64 = position.parse().map_err(|_| CdcError::UnusablePosition {
                    connector: CONNECTOR,
                    position: position.clone(),
                    reason: "not a SQLite rowid".into(),
                })?;
                if rowid > max_rowid {
                    return Err(CdcError::UnusablePosition {
                        connector: CONNECTOR,
                        position,
                        reason: format!(
                            "snapshot rowid is past the end of {} (max rowid {max_rowid}); the \
                             table has been truncated, rebuilt or restored mid-snapshot",
                            self.table
                        ),
                    });
                }
                (rowid, Some(max_rowid))
            }
            // A stored snapshot position under `never` is contradictory — that
            // mode does not snapshot. Start at the tail, as `never` means.
            Resume::Snapshotting(_) => (max_rowid, None),
        };

        let feed = Feed {
            source: SqliteSource {
                pool: self.pool.clone(),
                db_path: self.db_path.clone(),
                table: self.table.clone(),
                entity: self.entity.clone(),
            },
            cursor,
            snapshot_end,
            buffer: VecDeque::new(),
            rx,
            _watcher: watcher,
        };

        Ok(Box::pin(stream::unfold(feed, |mut feed| async move {
            loop {
                if let Some(record) = feed.buffer.pop_front() {
                    return Some((Ok(record), feed));
                }

                match feed.fetch().await {
                    Err(e) => return Some((Err(e), feed)),
                    Ok(rows) if !rows.is_empty() => {
                        for (rowid, record) in rows {
                            feed.cursor = rowid;
                            feed.buffer.push_back(record);
                        }
                        continue;
                    }
                    Ok(_) => {}
                }

                // Nothing left to read. If that was the snapshot draining,
                // switch to streaming from the captured position and go round
                // again — never sleep between the phases.
                if feed.snapshot_end.take().is_some() {
                    continue;
                }

                // Park until the filesystem says something changed. Note the
                // order: we wait only *after* a read came back empty, and we
                // drain surplus edges *before* the next read — so an edge that
                // arrives while we are reading is still pending afterwards and
                // cannot be lost. Coalescing is what makes one transaction's
                // several file touches cost one read.
                feed.rx.recv().await?;
                while feed.rx.try_recv().is_ok() {}
            }
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_must_be_plain_identifiers() {
        assert!(validate_identifier("envelopes").is_ok());
        assert!(validate_identifier("hen_events_2").is_ok());
        for bad in [
            "",
            "2legit",
            "envelopes; DROP TABLE envelopes",
            "envelopes--",
            "env elopes",
            "\"envelopes\"",
        ] {
            assert!(
                validate_identifier(bad).is_err(),
                "{bad:?} must be refused before it reaches SQL"
            );
        }
    }

    /// A database file that does not exist gives no feed, loudly. The failure
    /// mode this prevents is a connector that starts, watches nothing, and
    /// reports healthy forever.
    #[tokio::test]
    async fn a_missing_database_file_is_a_loud_no_feed() {
        let dir = tempfile::tempdir().unwrap();
        let err = match SqliteSource::open(dir.path().join("nope.db"), "envelopes", "hen").await {
            Ok(_) => panic!("a missing database must not open"),
            Err(e) => e,
        };
        assert!(
            matches!(err, CdcError::NoFeed { .. }),
            "must be NoFeed, got: {err}"
        );
    }
}
