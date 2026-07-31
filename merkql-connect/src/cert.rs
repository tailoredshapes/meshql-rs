//! The certification suite: the contract that makes the three sources
//! interchangeable.
//!
//! A consumer reads `op`, `source.position` and `after`. It must not care
//! whether the record came from a SQLite rowid scan, a Mongo change stream or
//! a Postgres replication slot. That interchangeability is the entire point of
//! the Debezium-shaped envelope, and it is only real if it is tested the same
//! way for every backend — so each backend's integration test drives
//! [`certify`] rather than writing its own approximation of the contract.

use crate::record::Op;
use crate::source::{ChangeStream, CommitSource, Resume, SnapshotMode};
use crate::ChangeRecord;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use meshql_core::{Envelope, Stash};
use std::time::Duration;

/// What a backend must supply to be certified.
#[async_trait]
pub trait CertStore: Send + Sync {
    /// Commit an envelope through the store's ordinary write path — the same
    /// path a restlette uses. Not through the connector.
    async fn write(&self, envelope: Envelope) -> Result<()>;

    /// A freshly opened source over that store.
    async fn source(&self) -> Result<Box<dyn CommitSource>>;
}

fn envelope(id: &str) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("marker".to_string(), serde_json::json!(id));
    Envelope::new(id, payload, vec!["cert".to_string()])
}

/// Pull exactly `n` records off a stream, or fail with what did arrive.
pub async fn take(
    stream: &mut ChangeStream,
    n: usize,
    within: Duration,
) -> Result<Vec<ChangeRecord>> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + within;
    while out.len() < n {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(Ok(record))) => out.push(record),
            Ok(Some(Err(e))) => bail!("stream error after {} records: {e}", out.len()),
            Ok(None) => bail!("stream ended after {} of {n} records", out.len()),
            Err(_) => bail!(
                "timed out waiting for record {} of {n}; got ids {:?}",
                out.len() + 1,
                out.iter().map(|r| r.key()).collect::<Vec<_>>()
            ),
        }
    }
    Ok(out)
}

/// Assert nothing arrives within `within`. Used where an *extra* record would
/// be the bug.
async fn expect_quiet(stream: &mut ChangeStream, within: Duration) -> Result<()> {
    match tokio::time::timeout(within, stream.next()).await {
        Err(_) => Ok(()),
        Ok(Some(Ok(r))) => bail!("expected no further records, got {:?}", r.key()),
        Ok(Some(Err(e))) => bail!("expected no further records, got error: {e}"),
        Ok(None) => Ok(()),
    }
}

const PATIENCE: Duration = Duration::from_secs(20);

/// Run the whole contract against one backend.
pub async fn certify(store: &dyn CertStore) -> Result<()> {
    certify_snapshot_then_stream(store)
        .await
        .context("snapshot-then-stream")?;
    certify_positions_are_present_and_distinct(store)
        .await
        .context("native positions")?;
    certify_resume_delivers_only_what_follows(store)
        .await
        .context("resume")?;
    certify_never_mode_skips_history(store)
        .await
        .context("snapshot_mode = never")?;
    Ok(())
}

/// Rows that existed before the connector started arrive as `op: r` with
/// `source.snapshot == true`; rows written afterwards arrive as `op: c` with
/// `snapshot == false`; and **nothing is lost across the handover**.
///
/// The handover is the part worth testing, because the naive implementation
/// (snapshot the table, then start watching) drops anything committed between
/// the two — the classic CDC gap, invisible until a projection is quietly
/// wrong.
pub async fn certify_snapshot_then_stream(store: &dyn CertStore) -> Result<()> {
    store.write(envelope("pre-1")).await?;
    store.write(envelope("pre-2")).await?;

    let source = store.source().await?;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .map_err(anyhow::Error::from)?;

    let snapshot = take(&mut stream, 2, PATIENCE).await?;
    for record in &snapshot {
        if record.op != Op::Read {
            bail!(
                "snapshot record {:?} has op {:?}, want `r`",
                record.key(),
                record.op
            );
        }
        if !record.source.snapshot.is_snapshot() {
            bail!("snapshot record {:?} is not flagged snapshot", record.key());
        }
    }
    let mut ids: Vec<String> = snapshot.iter().filter_map(|r| r.key()).collect();
    ids.sort();
    if ids != vec!["pre-1".to_string(), "pre-2".to_string()] {
        bail!("snapshot delivered {ids:?}, want the two pre-existing rows");
    }

    store.write(envelope("live-1")).await?;
    let live = take(&mut stream, 1, PATIENCE).await?;
    if live[0].op != Op::Create {
        bail!("live record has op {:?}, want `c`", live[0].op);
    }
    if live[0].source.snapshot.is_snapshot() {
        bail!("a live record must not be flagged snapshot");
    }
    if live[0].key().as_deref() != Some("live-1") {
        bail!("live record is {:?}, want live-1", live[0].key());
    }

    // The payload must survive intact — a connector that delivers ids but
    // empty envelopes passes every structural assertion above and is useless.
    let after = live[0]
        .after
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("a live record must carry `after`"))?;
    if after.payload.get("marker") != Some(&serde_json::json!("live-1")) {
        bail!("the committed payload did not survive: {:?}", after.payload);
    }
    if after.authorized_tokens != vec!["cert".to_string()] {
        bail!(
            "authorized_tokens did not survive: {:?}",
            after.authorized_tokens
        );
    }
    Ok(())
}

/// Every record names the store's own position, and no two records share one.
/// The position is what the offset store persists and what a consumer dedupes
/// against; a missing or repeated one silently breaks both.
pub async fn certify_positions_are_present_and_distinct(store: &dyn CertStore) -> Result<()> {
    let source = store.source().await?;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .map_err(anyhow::Error::from)?;

    for i in 0..3 {
        store.write(envelope(&format!("pos-{i}"))).await?;
    }
    let records = take(&mut stream, 3, PATIENCE).await?;

    let mut positions = Vec::new();
    for record in &records {
        let position = record
            .position()
            .ok_or_else(|| anyhow::anyhow!("record {:?} carries no position", record.key()))?;
        positions.push(position.to_string());
    }
    let mut sorted = positions.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != positions.len() {
        bail!("positions repeat: {positions:?}");
    }
    Ok(())
}

/// Resuming from a committed position delivers what follows it and not what
/// precedes it. This is the property a restart depends on.
pub async fn certify_resume_delivers_only_what_follows(store: &dyn CertStore) -> Result<()> {
    let source = store.source().await?;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .map_err(anyhow::Error::from)?;

    store.write(envelope("before-restart")).await?;
    let first = take(&mut stream, 1, PATIENCE).await?;
    let position = first[0]
        .position()
        .ok_or_else(|| anyhow::anyhow!("no position to resume from"))?
        .to_string();
    drop(stream);

    // Write while nothing is watching — the whole point of a durable position.
    store.write(envelope("during-downtime")).await?;

    let resumed = store.source().await?;
    let mut stream = resumed
        .changes(Resume::At(position.clone()), SnapshotMode::Initial)
        .await
        .map_err(anyhow::Error::from)?;

    let after = take(&mut stream, 1, PATIENCE).await?;
    if after[0].key().as_deref() != Some("during-downtime") {
        bail!(
            "resuming from {position:?} delivered {:?}; want the write that happened \
             while the connector was down, and nothing before it",
            after[0].key()
        );
    }
    Ok(())
}

/// `snapshot_mode = never` starts at the live tail: history is not replayed.
pub async fn certify_never_mode_skips_history(store: &dyn CertStore) -> Result<()> {
    store.write(envelope("history-1")).await?;

    let source = store.source().await?;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .map_err(anyhow::Error::from)?;

    // Nothing from before must arrive...
    expect_quiet(&mut stream, Duration::from_millis(750)).await?;

    // ...but the feed must be live, not merely silent. A connector that
    // delivers nothing at all would pass the assertion above.
    store.write(envelope("after-start")).await?;
    let live = take(&mut stream, 1, PATIENCE).await?;
    if live[0].key().as_deref() != Some("after-start") {
        bail!("expected the post-start write, got {:?}", live[0].key());
    }
    Ok(())
}
