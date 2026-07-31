//! PostgreSQL CDC: `NOTIFY` is the wake-up edge, the replication slot is the
//! truth.
//!
//! # This is not the `LISTEN`/`NOTIFY` anti-pattern — read this before judging
//!
//! `LISTEN`/`NOTIFY` **on its own** is disqualifying for change capture, and
//! for a good reason: a notification is delivered only to sessions that are
//! listening at the moment it fires. Restart the connector and every change
//! that happened in the gap is simply gone, with nothing anywhere recording
//! that it was missed. That is the failure this crate exists to prevent.
//!
//! That objection does not apply here, because **no data ever travels over the
//! notification.** A logical replication slot holds every change until it is
//! explicitly consumed; the `NOTIFY` payload is empty and carries nothing but
//! the fact that *something* happened. So:
//!
//! - **Losing the edge is survivable.** A dropped notification costs latency
//!   until the next one — or until the heartbeat tick — and nothing else,
//!   because the slot still holds the change.
//! - **Losing the data is impossible.** The slot retains WAL until
//!   `confirmed_flush_lsn` advances, and this module only advances it once the
//!   connector reports the records durable.
//!
//! This is structurally the same arrangement as the SQLite source: inotify is
//! an event edge and the ordinary connection is the durable read; `NOTIFY` is
//! an event edge and the slot is the durable read. In neither case does
//! anything sit on a clock asking "anything yet?" — the only timer here is the
//! heartbeat, which exists for a different reason entirely (see below).
//!
//! # Peek, then advance — never `get`
//!
//! `pg_logical_slot_get_binary_changes` returns changes **and consumes them**
//! in one step. That is unusable: read LSNs 1–5, append 1–2 to the topic,
//! crash, and 3–5 exist nowhere. The slot has forgotten them and the offset
//! store never recorded them.
//!
//! So this module uses `pg_logical_slot_peek_binary_changes`, which reads
//! without consuming, and advances the slot separately via
//! [`CommitSource::durable_through`] — which the connector calls only after an
//! offset commit has hit the disk. A crash anywhere before that leaves the
//! slot exactly where it was: the batch replays. Duplicates, never a gap.
//!
//! # The heartbeat, and why the *idle* deployment is the dangerous one
//!
//! An unconsumed slot pins WAL. Postgres cannot recycle any segment at or
//! after `confirmed_flush_lsn`, so a slot that stops advancing grows the WAL
//! directory without bound until the disk fills and the whole cluster stops.
//!
//! The counterintuitive part: this bites hardest when *the watched table is
//! idle*. The slot only advances when this connector advances it, and it only
//! has cause to advance when it sees changes it cares about. Meanwhile every
//! other table in the database is generating WAL that the slot is pinning. A
//! quiet event mesh in a busy database is the worst case, not the best one.
//!
//! Hence the heartbeat, and hence the ordering it uses, which is load-bearing:
//!
//! 1. capture `target := pg_current_wal_lsn()`,
//! 2. peek **up to `target`**,
//! 3. if that came back empty, advance to `target`.
//!
//! Capturing first is what makes step 3 safe. Reversing 1 and 2 would let a
//! write land between the empty peek and the `pg_current_wal_lsn()` call, and
//! advancing past it would skip it — a silent gap manufactured by the very
//! mechanism meant to keep the database healthy.
//!
//! The advance is additionally clamped to what the connector has reported
//! durable, so an idle tick can never release WAL for records that are still
//! in flight.
//!
//! # A slot left behind is a production hazard
//!
//! If this connector is stopped for a weekend and its slot is not dropped, the
//! slot keeps pinning WAL the entire time. [`PostgresSource::retained_wal_bytes`]
//! reports the backlog and `open` shouts about it at startup, because the
//! failure mode is a database that fills its disk with nothing obviously
//! wrong. **Dropping a connector permanently means dropping its slot**
//! (`SELECT pg_drop_replication_slot(...)`); leaving the config file behind is
//! not enough.

use crate::record::{ChangeRecord, Op, Snapshot, SourceInfo};
use crate::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use meshql_core::{Envelope, Stash};
use sqlx::postgres::{PgListener, PgPool};
use sqlx::Row;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const CONNECTOR: &str = "postgresql";

/// Warn above this much pinned WAL. 512 MiB is small enough to be a real
/// signal and large enough not to cry wolf on a busy cluster.
const WAL_BACKLOG_WARN_BYTES: i64 = 512 * 1024 * 1024;

/// A Postgres LSN, comparable. Postgres prints them `XXXXXXXX/XXXXXXXX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Lsn(u64);

impl Lsn {
    fn parse(text: &str) -> Option<Lsn> {
        let (high, low) = text.split_once('/')?;
        let high = u64::from_str_radix(high.trim(), 16).ok()?;
        let low = u64::from_str_radix(low.trim(), 16).ok()?;
        Some(Lsn((high << 32) | low))
    }

    fn text(&self) -> String {
        format!("{:X}/{:X}", self.0 >> 32, self.0 & 0xFFFF_FFFF)
    }
}

fn validate_identifier(name: &str, what: &str) -> Result<(), CdcError> {
    let ok = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.chars().next().unwrap().is_ascii_digit();
    if ok {
        Ok(())
    } else {
        Err(CdcError::Backend(anyhow::anyhow!(
            "'{name}' is not a valid PostgreSQL {what} (letters, digits and underscore only)"
        )))
    }
}

pub struct PostgresSource {
    pool: PgPool,
    conn: String,
    table: String,
    entity: String,
    slot: String,
    publication: String,
    channel: String,
    heartbeat: Duration,
    /// The highest LSN the connector has reported durable. Shared with the
    /// feed so an idle heartbeat can never advance past records still in
    /// flight.
    durable: Arc<Mutex<Option<Lsn>>>,
}

impl PostgresSource {
    pub async fn open(
        conn: &str,
        table: &str,
        entity: &str,
        slot: &str,
        publication: &str,
        heartbeat: Duration,
    ) -> Result<Self, CdcError> {
        validate_identifier(table, "table name")?;
        validate_identifier(slot, "replication slot name")?;
        validate_identifier(publication, "publication name")?;

        let pool = PgPool::connect(conn)
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("connecting to PostgreSQL: {e}")))?;

        let source = Self {
            pool,
            conn: conn.to_string(),
            table: table.to_string(),
            entity: entity.to_string(),
            slot: slot.to_string(),
            publication: publication.to_string(),
            channel: format!("merkql_connect_{entity}"),
            heartbeat,
            durable: Arc::new(Mutex::new(None)),
        };

        source.require_logical_wal().await?;
        // Establish capture at OPEN, not at first stream. Two reasons: a
        // deployment whose slot or publication cannot be created should fail
        // at startup rather than at the first change, and — the correctness
        // one — the slot must already be retaining WAL before any snapshot
        // runs, which `open` preceding `changes` guarantees structurally.
        source.ensure_capture().await?;
        source.warn_about_retained_wal().await;
        Ok(source)
    }

    /// Logical decoding is impossible without `wal_level = logical`, and it is
    /// not a runtime-settable parameter. Failing here beats failing later with
    /// an opaque error from slot creation.
    async fn require_logical_wal(&self) -> Result<(), CdcError> {
        let level: String = sqlx::query_scalar("SHOW wal_level")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading wal_level: {e}")))?;
        if level != "logical" {
            return Err(CdcError::NoFeed {
                connector: CONNECTOR,
                reason: format!(
                    "wal_level is '{level}', but logical decoding requires 'logical'. \
                     Set `wal_level = logical` in postgresql.conf and restart the server; \
                     it cannot be changed at runtime."
                ),
            });
        }
        Ok(())
    }

    /// Create the publication, the slot, and the notify trigger. All idempotent.
    async fn ensure_capture(&self) -> Result<(), CdcError> {
        let bad = |ctx: &str| {
            let ctx = ctx.to_string();
            move |e: sqlx::Error| CdcError::Backend(anyhow::anyhow!("{ctx}: {e}"))
        };

        let has_publication: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_publication WHERE pubname = $1)")
                .bind(&self.publication)
                .fetch_one(&self.pool)
                .await
                .map_err(bad("checking publication"))?;
        if !has_publication {
            sqlx::query(&format!(
                "CREATE PUBLICATION {} FOR TABLE {}",
                self.publication, self.table
            ))
            .execute(&self.pool)
            .await
            .map_err(bad("creating publication"))?;
        }

        let has_slot: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
        )
        .bind(&self.slot)
        .fetch_one(&self.pool)
        .await
        .map_err(bad("checking replication slot"))?;
        if !has_slot {
            sqlx::query("SELECT pg_create_logical_replication_slot($1, 'pgoutput')")
                .bind(&self.slot)
                .execute(&self.pool)
                .await
                .map_err(bad("creating replication slot"))?;
        }

        // The wake-up edge. Statement-level, not row-level: one notification
        // per statement is all an edge needs, and a row-level trigger would
        // fire once per inserted row for no benefit. `pg_notify` is delivered
        // at COMMIT, so the edge is inherently post-commit — a notification
        // can never announce a write that then rolls back.
        let function = format!("{}_notify", self.slot);
        let trigger = format!("{}_notify_trg", self.slot);
        sqlx::query(&format!(
            "CREATE OR REPLACE FUNCTION {function}() RETURNS trigger \
             LANGUAGE plpgsql AS $fn$ BEGIN PERFORM pg_notify('{}', ''); RETURN NULL; END $fn$",
            self.channel
        ))
        .execute(&self.pool)
        .await
        .map_err(bad("creating notify function"))?;

        sqlx::query(&format!(
            "DROP TRIGGER IF EXISTS {trigger} ON {}",
            self.table
        ))
        .execute(&self.pool)
        .await
        .map_err(bad("dropping stale notify trigger"))?;
        sqlx::query(&format!(
            "CREATE TRIGGER {trigger} AFTER INSERT ON {} \
             FOR EACH STATEMENT EXECUTE FUNCTION {function}()",
            self.table
        ))
        .execute(&self.pool)
        .await
        .map_err(bad("creating notify trigger"))?;

        Ok(())
    }

    /// How much WAL this connector's slot is currently pinning.
    ///
    /// `None` when the slot does not exist. This is the number that turns into
    /// a full disk if the connector is left stopped.
    pub async fn retained_wal_bytes(&self) -> Result<Option<i64>, CdcError> {
        let bytes: Option<i64> = sqlx::query_scalar(
            "SELECT pg_wal_lsn_diff(pg_current_wal_lsn(), confirmed_flush_lsn)::bigint \
             FROM pg_replication_slots WHERE slot_name = $1",
        )
        .bind(&self.slot)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CdcError::Backend(anyhow::anyhow!("measuring retained WAL: {e}")))?
        .flatten();
        Ok(bytes)
    }

    /// The slot's current `confirmed_flush_lsn`, if the slot exists.
    pub async fn confirmed_flush_lsn(&self) -> Result<Option<String>, CdcError> {
        let lsn: Option<String> = sqlx::query_scalar(
            "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name = $1",
        )
        .bind(&self.slot)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading confirmed_flush_lsn: {e}")))?
        .flatten();
        Ok(lsn)
    }

    async fn slot_exists(&self) -> Result<bool, CdcError> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
        )
        .bind(&self.slot)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| CdcError::Backend(anyhow::anyhow!("checking replication slot: {e}")))
    }

    async fn current_wal_lsn(&self) -> Result<Lsn, CdcError> {
        let text: String = sqlx::query_scalar("SELECT pg_current_wal_lsn()::text")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading pg_current_wal_lsn: {e}")))?;
        Lsn::parse(&text)
            .ok_or_else(|| CdcError::Backend(anyhow::anyhow!("unparseable current WAL LSN {text}")))
    }

    /// Warn loudly if the slot has been pinning WAL. Called at startup, which
    /// is exactly when an operator is in a position to do something about it.
    async fn warn_about_retained_wal(&self) {
        if let Ok(Some(bytes)) = self.retained_wal_bytes().await {
            if bytes > WAL_BACKLOG_WARN_BYTES {
                eprintln!(
                    "[merkql-connect postgresql] WARNING: replication slot '{}' is pinning \
                     {} MiB of WAL. PostgreSQL cannot recycle it while this slot lags, and \
                     the disk will fill if the connector stays behind. If this connector has \
                     been retired, DROP THE SLOT: SELECT pg_drop_replication_slot('{}');",
                    self.slot,
                    bytes / (1024 * 1024),
                    self.slot
                );
            }
        }
    }
}

// ── pgoutput ─────────────────────────────────────────────────────────────

/// A cursor over one pgoutput message body.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn u8(&mut self) -> Option<u8> {
        let v = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(v)
    }
    fn u16(&mut self) -> Option<u16> {
        let slice = self.bytes.get(self.at..self.at + 2)?;
        self.at += 2;
        Some(u16::from_be_bytes([slice[0], slice[1]]))
    }
    fn u64(&mut self) -> Option<u64> {
        let slice = self.bytes.get(self.at..self.at + 8)?;
        self.at += 8;
        Some(u64::from_be_bytes([
            slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
        ]))
    }
    fn u32(&mut self) -> Option<u32> {
        let slice = self.bytes.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }
    fn cstring(&mut self) -> Option<String> {
        let end = self.bytes[self.at..].iter().position(|b| *b == 0)? + self.at;
        let text = String::from_utf8_lossy(&self.bytes[self.at..end]).into_owned();
        self.at = end + 1;
        Some(text)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.at..self.at + n)?;
        self.at += n;
        Some(slice)
    }
}

/// One decoded tuple: column name -> value, `None` for SQL NULL.
type Tuple = HashMap<String, Option<String>>;

/// Parse a pgoutput `TupleData` against the column names from the matching
/// Relation message.
fn parse_tuple(reader: &mut Reader, columns: &[String]) -> Option<Tuple> {
    let ncols = reader.u16()? as usize;
    let mut tuple = HashMap::new();
    for i in 0..ncols {
        let kind = reader.u8()?;
        let value = match kind {
            // 'n' null; 'u' unchanged TOAST (never present on an INSERT).
            b'n' | b'u' => None,
            // 't' text, 'b' binary — we request proto_version 1, which is
            // text-only, but accept both rather than silently dropping a row.
            b't' | b'b' => {
                let len = reader.u32()? as usize;
                Some(String::from_utf8_lossy(reader.take(len)?).into_owned())
            }
            _ => return None,
        };
        if let Some(name) = columns.get(i) {
            tuple.insert(name.clone(), value);
        }
    }
    Some(tuple)
}

/// What a single pgoutput message turned into.
enum Decoded {
    /// A Relation message — column layout for a relation id.
    Relation(u32, Vec<String>),
    /// An Insert — the only operation an append-only envelope table produces.
    /// The relation id is carried for the tests that pin the wire format; the
    /// decoder itself has already used it to look the columns up.
    #[allow(dead_code)]
    Insert(u32, Tuple),
    /// A Commit, carrying the transaction's **end LSN** — the position just
    /// past the commit record.
    ///
    /// This, not the Insert's own LSN, is the resumable position. Logical
    /// decoding replays whole transactions: setting `confirmed_flush_lsn` to
    /// an LSN *inside* a transaction makes PostgreSQL restart at that
    /// transaction's beginning and re-deliver all of it. Every record in one
    /// transaction therefore shares one position — the point after which the
    /// transaction is genuinely behind us.
    Commit(Lsn),
    /// Begin, Origin, Type, and the update/delete/truncate messages we
    /// deliberately do not translate.
    Ignored,
}

fn decode_message(data: &[u8], relations: &HashMap<u32, Vec<String>>) -> Option<Decoded> {
    let mut reader = Reader::new(data);
    match reader.u8()? {
        b'R' => {
            let relid = reader.u32()?;
            let _namespace = reader.cstring()?;
            let _relname = reader.cstring()?;
            let _replica_identity = reader.u8()?;
            let ncols = reader.u16()? as usize;
            let mut columns = Vec::with_capacity(ncols);
            for _ in 0..ncols {
                let _flags = reader.u8()?;
                columns.push(reader.cstring()?);
                let _type_oid = reader.u32()?;
                let _type_mod = reader.u32()?;
            }
            Some(Decoded::Relation(relid, columns))
        }
        b'C' => {
            let _flags = reader.u8()?;
            let _commit_lsn = reader.u64()?;
            let end_lsn = reader.u64()?;
            Some(Decoded::Commit(Lsn(end_lsn)))
        }
        b'I' => {
            let relid = reader.u32()?;
            // 'N' introduces the new tuple.
            if reader.u8()? != b'N' {
                return Some(Decoded::Ignored);
            }
            let columns = relations.get(&relid)?;
            Some(Decoded::Insert(relid, parse_tuple(&mut reader, columns)?))
        }
        // An envelope table is append-only, so an update, delete or truncate
        // is not a mesh write. Translating one into a `c` would put a change
        // on the topic that the mesh never made.
        _ => Some(Decoded::Ignored),
    }
}

fn tuple_to_envelope(tuple: &Tuple) -> Option<Envelope> {
    let text = |k: &str| tuple.get(k).and_then(|v| v.clone());
    let id = text("id")?;
    let created_at_ms: i64 = text("created_at_ms")?.parse().ok()?;
    let deleted = matches!(text("deleted").as_deref(), Some("t") | Some("true"));
    let authorized_tokens: Vec<String> = serde_json::from_str(&text("authorized_tokens")?).ok()?;
    let payload: Stash = serde_json::from_str(&text("payload")?).ok()?;
    Some(Envelope {
        id,
        payload,
        created_at: chrono::DateTime::from_timestamp_millis(created_at_ms).unwrap_or_default(),
        deleted,
        authorized_tokens,
    })
}

// ── the feed ─────────────────────────────────────────────────────────────

struct PgFeed {
    pool: PgPool,
    listener: PgListener,
    slot: String,
    publication: String,
    entity: String,
    heartbeat: Duration,
    relations: HashMap<u32, Vec<String>>,
    buffer: VecDeque<ChangeRecord>,
    /// Highest LSN handed to the connector. The heartbeat may never advance
    /// past this unless it is also durable.
    yielded: Option<Lsn>,
    durable: Arc<Mutex<Option<Lsn>>>,
}

impl PgFeed {
    /// Read everything the slot holds up to `target`, **without consuming it**.
    async fn peek(&mut self, target: Lsn) -> Result<Vec<(Lsn, ChangeRecord)>, CdcError> {
        let rows = sqlx::query(
            "SELECT lsn::text AS lsn, data FROM pg_logical_slot_peek_binary_changes( \
                 $1, $2::pg_lsn, NULL, \
                 VARIADIC ARRAY['proto_version', '1', 'publication_names', $3]) ",
        )
        .bind(&self.slot)
        .bind(target.text())
        .bind(&self.publication)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CdcError::Backend(anyhow::anyhow!("peeking at slot '{}': {e}", self.slot)))?;

        let mut out = Vec::new();
        // Inserts seen since the last Begin, awaiting their Commit. An insert
        // is only emitted once its transaction has committed *and* we know the
        // LSN to resume past — so a transaction truncated by `upto_lsn` is
        // simply left for the next peek rather than published half-decoded.
        let mut pending: Vec<(Envelope, i64)> = Vec::new();
        for row in &rows {
            let lsn_text: String = row
                .try_get("lsn")
                .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading lsn: {e}")))?;
            let data: Vec<u8> = row
                .try_get("data")
                .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading pgoutput data: {e}")))?;
            if Lsn::parse(&lsn_text).is_none() {
                return Err(CdcError::Backend(anyhow::anyhow!(
                    "unparseable LSN {lsn_text} from slot '{}'",
                    self.slot
                )));
            }

            match decode_message(&data, &self.relations) {
                Some(Decoded::Relation(relid, columns)) => {
                    self.relations.insert(relid, columns);
                }
                Some(Decoded::Insert(_, tuple)) => {
                    let Some(envelope) = tuple_to_envelope(&tuple) else {
                        continue;
                    };
                    let ts_ms = envelope.created_at.timestamp_millis();
                    pending.push((envelope, ts_ms));
                }
                Some(Decoded::Commit(end_lsn)) => {
                    for (envelope, ts_ms) in pending.drain(..) {
                        out.push((
                            end_lsn,
                            ChangeRecord::new(
                                Op::Create,
                                envelope,
                                SourceInfo {
                                    connector: CONNECTOR.to_string(),
                                    entity: self.entity.clone(),
                                    ts_ms,
                                    position: Some(end_lsn.text()),
                                    snapshot: Snapshot::False,
                                },
                            ),
                        ));
                    }
                }
                Some(Decoded::Ignored) | None => {}
            }
        }
        Ok(out)
    }

    /// Release WAL up to `upto` — destructive, so every caller must have
    /// established that nothing before `upto` is still needed.
    async fn advance(&self, upto: Lsn) -> Result<(), CdcError> {
        sqlx::query("SELECT pg_replication_slot_advance($1, $2::pg_lsn)")
            .bind(&self.slot)
            .bind(upto.text())
            .execute(&self.pool)
            .await
            .map_err(|e| {
                CdcError::Backend(anyhow::anyhow!("advancing slot '{}': {e}", self.slot))
            })?;
        Ok(())
    }

    /// The furthest point an idle tick may release WAL to.
    ///
    /// If everything handed to the connector is durable, `target` — the whole
    /// point of the heartbeat, since that is what frees WAL generated by
    /// *other* tables. Otherwise only as far as the connector has confirmed,
    /// so a heartbeat can never discard a record still in flight.
    async fn idle_advance_limit(&self, target: Lsn) -> Lsn {
        let durable = *self.durable.lock().await;
        match (self.yielded, durable) {
            (None, _) => target,
            (Some(yielded), Some(durable)) if durable >= yielded => target,
            (Some(_), Some(durable)) => durable,
            (Some(_), None) => Lsn(0),
        }
    }
}

#[async_trait]
impl CommitSource for PostgresSource {
    fn connector(&self) -> &'static str {
        CONNECTOR
    }

    fn entity(&self) -> &str {
        &self.entity
    }

    /// Told that everything through `position` is on the topic and committed.
    /// Only now may the slot let go of it.
    async fn durable_through(&self, position: &str) -> Result<(), CdcError> {
        let Some(lsn) = Lsn::parse(position) else {
            return Ok(());
        };
        {
            let mut durable = self.durable.lock().await;
            if durable.is_none_or(|d| lsn > d) {
                *durable = Some(lsn);
            }
        }
        sqlx::query("SELECT pg_replication_slot_advance($1, $2::pg_lsn)")
            .bind(&self.slot)
            .bind(lsn.text())
            .execute(&self.pool)
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("advancing slot: {e}")))?;
        Ok(())
    }

    async fn changes(&self, from: Resume, mode: SnapshotMode) -> Result<ChangeStream, CdcError> {
        // A resume position is only meaningful if the slot that produced it
        // still exists. A dropped slot means every change since is gone from
        // the server — the single most dangerous state this connector can find
        // itself in, and one that would otherwise look like a clean cold start.
        if let Resume::At(position) = &from {
            if !self.slot_exists().await? {
                return Err(CdcError::UnusablePosition {
                    connector: CONNECTOR,
                    position: position.clone(),
                    reason: format!(
                        "replication slot '{}' no longer exists, so every change after \
                         {position} has been discarded by PostgreSQL and cannot be recovered \
                         from the WAL",
                        self.slot
                    ),
                });
            }
            let Some(lsn) = Lsn::parse(position) else {
                return Err(CdcError::UnusablePosition {
                    connector: CONNECTOR,
                    position: position.clone(),
                    reason: "not a PostgreSQL LSN".into(),
                });
            };
            // An LSN ahead of the server's own write position means this
            // offset file belongs to a different (or restored) cluster.
            if lsn > self.current_wal_lsn().await? {
                return Err(CdcError::UnusablePosition {
                    connector: CONNECTOR,
                    position: position.clone(),
                    reason: "LSN is ahead of the server's current WAL position; the database \
                             has been restored or this offset belongs to another cluster"
                        .into(),
                });
            }
            // A stored position is the connector telling us it made
            // everything up to `lsn` durable — the same statement
            // `durable_through` makes. So honour it the same way and move the
            // slot up to it; otherwise the slot stays wherever it was and the
            // resume replays records the connector already published, which
            // is safe but is not what `Resume::At` promises.
            self.durable_through(position).await?;
        }

        let mut listener = PgListener::connect(&self.conn).await.map_err(|e| {
            // No listener means no edge. Fail rather than fall back to a
            // timer — a connector whose latency silently becomes the heartbeat
            // interval is a connector nobody deployed.
            CdcError::NoFeed {
                connector: CONNECTOR,
                reason: format!("opening a LISTEN connection: {e}"),
            }
        })?;
        listener
            .listen(&self.channel)
            .await
            .map_err(|e| CdcError::NoFeed {
                connector: CONNECTOR,
                reason: format!("LISTEN on '{}': {e}", self.channel),
            })?;

        // ── the snapshot ──
        let mut snapshot_records: Vec<Result<ChangeRecord, CdcError>> = Vec::new();
        if matches!(from, Resume::Cold) && mode.snapshots_on_cold_start() {
            // The position the snapshot's final record carries: where the slot
            // sits *before* the snapshot ran. Resuming there replays anything
            // written during the snapshot and skips nothing.
            let resume_at = self.confirmed_flush_lsn().await?;

            let rows = sqlx::query(&format!(
                "SELECT id, created_at_ms, deleted, authorized_tokens, payload \
                 FROM {} ORDER BY created_at_ms, id",
                self.table
            ))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("snapshotting: {e}")))?;

            let last = rows.len().saturating_sub(1);
            for (i, row) in rows.iter().enumerate() {
                let get = |k: &str| row.try_get::<String, _>(k).ok();
                let mut tuple: Tuple = HashMap::new();
                tuple.insert("id".into(), get("id"));
                tuple.insert(
                    "created_at_ms".into(),
                    row.try_get::<i64, _>("created_at_ms")
                        .ok()
                        .map(|v| v.to_string()),
                );
                tuple.insert(
                    "deleted".into(),
                    row.try_get::<bool, _>("deleted").ok().map(|v| {
                        if v {
                            "t".into()
                        } else {
                            "f".into()
                        }
                    }),
                );
                tuple.insert("authorized_tokens".into(), get("authorized_tokens"));
                tuple.insert("payload".into(), get("payload"));

                let Some(envelope) = tuple_to_envelope(&tuple) else {
                    continue;
                };
                let ts_ms = envelope.created_at.timestamp_millis();
                let (flag, position) = if i == last {
                    (Snapshot::Last, resume_at.clone())
                } else {
                    (Snapshot::True, None)
                };
                snapshot_records.push(Ok(ChangeRecord::new(
                    Op::Read,
                    envelope,
                    SourceInfo {
                        connector: CONNECTOR.to_string(),
                        entity: self.entity.clone(),
                        ts_ms,
                        position,
                        snapshot: flag,
                    },
                )));
            }
        }

        let feed = PgFeed {
            pool: self.pool.clone(),
            listener,
            slot: self.slot.clone(),
            publication: self.publication.clone(),
            entity: self.entity.clone(),
            heartbeat: self.heartbeat,
            relations: HashMap::new(),
            buffer: VecDeque::new(),
            yielded: None,
            durable: self.durable.clone(),
        };

        let live = stream::unfold(feed, |mut feed| async move {
            loop {
                if let Some(record) = feed.buffer.pop_front() {
                    return Some((Ok(record), feed));
                }

                // ORDER IS THE CORRECTNESS ARGUMENT: capture the target
                // first, then peek up to it. Reversed, a write landing
                // between an empty peek and the target read would be skipped
                // by the advance below.
                let target = match feed_current_wal_lsn(&feed).await {
                    Ok(lsn) => lsn,
                    Err(e) => return Some((Err(e), feed)),
                };

                match feed.peek(target).await {
                    Err(e) => return Some((Err(e), feed)),
                    Ok(records) => {
                        // Skip what this feed has already handed over.
                        //
                        // `peek` is deliberately non-consuming, so it returns
                        // the same records on every call until the slot is
                        // advanced — and the slot is only advanced once the
                        // connector confirms durability, which may be several
                        // commit intervals away. Without this filter the feed
                        // re-delivers the same batch on every cycle, which is
                        // permitted by at-least-once but is a spin, not a
                        // feed. Duplicates belong at a restart, not in the
                        // steady state.
                        //
                        // Exact because positions are commit end LSNs, which
                        // increase monotonically across transactions.
                        let fresh: Vec<_> = records
                            .into_iter()
                            .filter(|(lsn, _)| feed.yielded.is_none_or(|seen| *lsn > seen))
                            .collect();
                        if !fresh.is_empty() {
                            for (lsn, record) in fresh {
                                feed.yielded = Some(lsn);
                                feed.buffer.push_back(record);
                            }
                            continue;
                        }
                    }
                }

                // Nothing for us at or before `target`. THE HEARTBEAT: release
                // WAL that other tables generated, clamped to what the
                // connector has actually made durable.
                let limit = feed.idle_advance_limit(target).await;
                if limit > Lsn(0) {
                    if let Err(e) = feed.advance(limit).await {
                        return Some((Err(e), feed));
                    }
                }

                // Park on the edge. The heartbeat tick is a floor on how long
                // WAL can stay pinned, not a poll — a notification wakes this
                // immediately, and a missed notification costs latency only,
                // because the slot still holds everything.
                tokio::select! {
                    _ = feed.listener.recv() => {}
                    _ = tokio::time::sleep(feed.heartbeat) => {}
                }
            }
        });

        Ok(Box::pin(stream::iter(snapshot_records).chain(live)))
    }
}

async fn feed_current_wal_lsn(feed: &PgFeed) -> Result<Lsn, CdcError> {
    let text: String = sqlx::query_scalar("SELECT pg_current_wal_lsn()::text")
        .fetch_one(&feed.pool)
        .await
        .map_err(|e| CdcError::Backend(anyhow::anyhow!("reading pg_current_wal_lsn: {e}")))?;
    Lsn::parse(&text)
        .ok_or_else(|| CdcError::Backend(anyhow::anyhow!("unparseable current WAL LSN {text}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsns_round_trip_and_order_numerically() {
        let a = Lsn::parse("0/16B3748").unwrap();
        let b = Lsn::parse("0/16B3749").unwrap();
        assert!(a < b);
        assert_eq!(a.text(), "0/16B3748");

        // The one a string comparison gets wrong: a higher segment with a
        // shorter low half.
        let low = Lsn::parse("0/FFFFFFFF").unwrap();
        let high = Lsn::parse("1/0").unwrap();
        assert!(
            high > low,
            "LSN ordering must be numeric, not lexicographic"
        );
    }

    #[test]
    fn malformed_lsns_are_rejected() {
        for bad in ["", "16B3748", "0/", "/1", "0/XYZ", "z/1"] {
            assert!(Lsn::parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn identifiers_must_be_plain() {
        assert!(validate_identifier("envelopes", "table name").is_ok());
        for bad in ["", "2legit", "env; DROP TABLE x", "env elopes", "\"env\""] {
            assert!(validate_identifier(bad, "table name").is_err(), "{bad:?}");
        }
    }

    /// A hand-built Relation + Insert pair, byte for byte as pgoutput frames
    /// them. Pins the parser against the wire format rather than against
    /// itself.
    #[test]
    fn a_relation_then_an_insert_decodes_into_an_envelope() {
        let mut relation = vec![b'R'];
        relation.extend_from_slice(&42u32.to_be_bytes());
        relation.extend_from_slice(b"public\0");
        relation.extend_from_slice(b"envelopes\0");
        relation.push(b'd');
        relation.extend_from_slice(&5u16.to_be_bytes());
        for name in [
            "id",
            "created_at_ms",
            "deleted",
            "authorized_tokens",
            "payload",
        ] {
            relation.push(0);
            relation.extend_from_slice(name.as_bytes());
            relation.push(0);
            relation.extend_from_slice(&25u32.to_be_bytes());
            relation.extend_from_slice(&(-1i32).to_be_bytes());
        }

        let mut relations = HashMap::new();
        match decode_message(&relation, &relations).unwrap() {
            Decoded::Relation(relid, columns) => {
                assert_eq!(relid, 42);
                assert_eq!(columns[0], "id");
                assert_eq!(columns[4], "payload");
                relations.insert(relid, columns);
            }
            _ => panic!("expected a Relation message"),
        }

        let mut insert = vec![b'I'];
        insert.extend_from_slice(&42u32.to_be_bytes());
        insert.push(b'N');
        insert.extend_from_slice(&5u16.to_be_bytes());
        for value in [
            "evt-1",
            "1751892345123",
            "f",
            r#"["farm"]"#,
            r#"{"eggs":3}"#,
        ] {
            insert.push(b't');
            insert.extend_from_slice(&(value.len() as u32).to_be_bytes());
            insert.extend_from_slice(value.as_bytes());
        }

        match decode_message(&insert, &relations).unwrap() {
            Decoded::Insert(relid, tuple) => {
                assert_eq!(relid, 42);
                let envelope = tuple_to_envelope(&tuple).expect("a decodable envelope");
                assert_eq!(envelope.id, "evt-1");
                assert_eq!(envelope.payload["eggs"], serde_json::json!(3));
                assert_eq!(envelope.authorized_tokens, vec!["farm".to_string()]);
                assert!(!envelope.deleted);
                assert_eq!(envelope.created_at.timestamp_millis(), 1751892345123);
            }
            _ => panic!("expected an Insert message"),
        }
    }

    /// A NULL column must decode as absent, not as an empty string that then
    /// parses into a bogus envelope.
    #[test]
    fn a_null_column_yields_no_envelope_rather_than_a_wrong_one() {
        let mut relations = HashMap::new();
        relations.insert(
            7u32,
            vec![
                "id".to_string(),
                "created_at_ms".to_string(),
                "deleted".to_string(),
                "authorized_tokens".to_string(),
                "payload".to_string(),
            ],
        );

        let mut insert = vec![b'I'];
        insert.extend_from_slice(&7u32.to_be_bytes());
        insert.push(b'N');
        insert.extend_from_slice(&5u16.to_be_bytes());
        insert.push(b't');
        insert.extend_from_slice(&5u32.to_be_bytes());
        insert.extend_from_slice(b"evt-1");
        insert.push(b'n'); // created_at_ms IS NULL
        for value in ["f", r#"[]"#, r#"{}"#] {
            insert.push(b't');
            insert.extend_from_slice(&(value.len() as u32).to_be_bytes());
            insert.extend_from_slice(value.as_bytes());
        }

        match decode_message(&insert, &relations).unwrap() {
            Decoded::Insert(_, tuple) => {
                assert!(tuple_to_envelope(&tuple).is_none());
            }
            _ => panic!("expected an Insert message"),
        }
    }

    /// The resumable position is the Commit message's **end LSN** — the third
    /// field, not the second. `commit_lsn` points *at* the commit record;
    /// resuming there re-decodes the transaction that ends with it. Pinned
    /// byte-for-byte because the two fields are adjacent Int64s and swapping
    /// them produces a connector that quietly replays its last transaction on
    /// every restart.
    #[test]
    fn a_commit_message_yields_the_end_lsn_not_the_commit_lsn() {
        let mut commit = vec![b'C'];
        commit.push(0); // flags
        commit.extend_from_slice(&0x0000_0000_016A_C358u64.to_be_bytes()); // commit_lsn
        commit.extend_from_slice(&0x0000_0000_016A_C400u64.to_be_bytes()); // end_lsn
        commit.extend_from_slice(&0i64.to_be_bytes()); // timestamp

        match decode_message(&commit, &HashMap::new()).unwrap() {
            Decoded::Commit(lsn) => {
                assert_eq!(lsn.text(), "0/16AC400", "must be end_lsn");
                assert_ne!(lsn.text(), "0/16AC358", "must NOT be commit_lsn");
            }
            _ => panic!("expected a Commit message"),
        }
    }

    /// Updates, deletes and truncates on an append-only envelope table are not
    /// mesh writes and must not be translated into `c` records.
    #[test]
    fn non_insert_messages_are_ignored() {
        let relations = HashMap::new();
        for tag in *b"BUDTOY" {
            let mut message = vec![tag];
            message.extend_from_slice(&[0u8; 16]);
            assert!(
                matches!(decode_message(&message, &relations), Some(Decoded::Ignored)),
                "message type {} must be ignored",
                tag as char
            );
        }
    }
}
