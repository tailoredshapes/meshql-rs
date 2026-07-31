//! MongoDB CDC: `collection.watch()`, and the resume token *is* the cursor.
//!
//! # The resume token replaces a hand-rolled position
//!
//! Every change-stream event carries a resume token, and `watch()` accepts one
//! as `resume_after` / `start_after`. That is a durable, server-defined cursor
//! with exactly the semantics the [`crate::OffsetStore`] wants, so the
//! connector stores the token verbatim and never invents a position of its
//! own. Anything else — an `_id`, a `createdAt` watermark — would be a second
//! source of truth that can disagree with the one the server honours.
//!
//! # Requirements
//!
//! Change streams need a replica set (or a sharded cluster); they do not exist
//! on a standalone `mongod`. A single-node replica set is enough, and is what
//! the tests run.
//!
//! # Snapshot-then-stream
//!
//! The change stream is opened **before** the snapshot query runs, and the
//! token captured at that moment becomes the position for the snapshot's final
//! record. So:
//!
//! - anything written before the stream opened is covered by the snapshot;
//! - anything written after it is covered by the stream;
//! - anything written *during* the snapshot is covered by **both** — a
//!   duplicate, which at-least-once permits and an idempotent fold absorbs.
//!
//! Opening the stream second would invert that overlap into a gap, which is
//! the failure this ordering exists to prevent.
//!
//! # An expired token is never a silent restart
//!
//! A resume token ages out when the oplog rolls past it (server error 286,
//! `ChangeStreamHistoryLost`), and a token from a rebuilt database is simply
//! unknown. Either way the driver refuses it, and this module reports
//! [`CdcError::UnusablePosition`] rather than quietly reopening at the live
//! tail — which would skip every change between the token and now.

use crate::record::{ChangeRecord, Op, Snapshot, SourceInfo};
use crate::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use async_trait::async_trait;
use bson::Document;
use futures::stream::{self, StreamExt};
use mongodb::change_stream::event::{ChangeStreamEvent, OperationType, ResumeToken};
use mongodb::options::FullDocumentType;
use mongodb::{Client, Collection};

const CONNECTOR: &str = "mongodb";

pub struct MongoSource {
    collection: Collection<Document>,
    entity: String,
}

impl MongoSource {
    pub async fn open(
        uri: &str,
        database: &str,
        collection: &str,
        entity: &str,
    ) -> Result<Self, CdcError> {
        let client = Client::with_uri_str(uri)
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("connecting to {uri}: {e}")))?;
        Ok(Self {
            collection: client.database(database).collection::<Document>(collection),
            entity: entity.to_string(),
        })
    }

    /// A resume token, round-tripped through the offset file as JSON.
    ///
    /// Stored verbatim rather than reduced to `_data`: the token is the
    /// server's opaque value and the driver is the only thing entitled to
    /// interpret it.
    fn encode_token(token: &ResumeToken) -> Result<String, CdcError> {
        serde_json::to_string(token)
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("encoding resume token: {e}")))
    }

    fn decode_token(position: &str) -> Result<ResumeToken, CdcError> {
        serde_json::from_str(position).map_err(|e| CdcError::UnusablePosition {
            connector: CONNECTOR,
            position: position.to_string(),
            reason: format!("not a MongoDB resume token: {e}"),
        })
    }

    fn record(
        &self,
        document: &Document,
        op: Op,
        snapshot: Snapshot,
        position: Option<String>,
    ) -> Option<ChangeRecord> {
        let envelope = meshql_mongo::converters::document_to_envelope(document)?;
        let ts_ms = envelope.created_at.timestamp_millis();
        Some(ChangeRecord::new(
            op,
            envelope,
            SourceInfo {
                connector: CONNECTOR.to_string(),
                entity: self.entity.clone(),
                ts_ms,
                position,
                snapshot,
            },
        ))
    }

    /// Did the **server** reject something, as opposed to the connection
    /// failing underneath us?
    ///
    /// The distinction is the whole of the resume policy, and getting it wrong
    /// is costly in both directions: calling a transient network blip an
    /// unusable position re-snapshots an entire collection, while calling a
    /// rejected token transient retries a doomed open forever and delivers
    /// nothing while looking healthy.
    ///
    /// A `Command` error is the server having considered the request and said
    /// no. When we are *resuming*, the token is the only thing we contributed
    /// that the server could be objecting to — a cold start on the same source
    /// and the same collection succeeds — so a command error on a resuming open
    /// is a rejected token, whatever code it carries.
    ///
    /// Enumerating codes was tried and is not enough: 286
    /// (`ChangeStreamHistoryLost`) is the documented one, but a token that is
    /// well-formed JSON and corrupt inside comes back as 50811
    /// (`KeyString format error`), and a token from a different cluster can
    /// come back as 40415 or 9. Each of those is just as unusable, and each
    /// would otherwise be reported as an opaque backend failure that
    /// `snapshot_mode = when_needed` could not recover from.
    fn is_server_rejection(e: &mongodb::error::Error) -> bool {
        matches!(e.kind.as_ref(), mongodb::error::ErrorKind::Command(_))
    }

    /// The narrower, documented case: the oplog has rolled past the token.
    /// Used mid-stream, where there is no cold-start control to lean on.
    fn is_history_lost(e: &mongodb::error::Error) -> bool {
        use mongodb::error::ErrorKind;
        let code = match e.kind.as_ref() {
            ErrorKind::Command(c) => Some(c.code),
            _ => None,
        };
        matches!(code, Some(286) | Some(280))
            || e.to_string().contains("ChangeStreamHistoryLost")
            || e.to_string()
                .contains("resume point may no longer be in the oplog")
    }
}

#[async_trait]
impl CommitSource for MongoSource {
    fn connector(&self) -> &'static str {
        CONNECTOR
    }

    fn entity(&self) -> &str {
        &self.entity
    }

    async fn changes(&self, from: Resume, mode: SnapshotMode) -> Result<ChangeStream, CdcError> {
        // Open the stream FIRST — see the module docs. `full_document: Always`
        // so an event carries the document itself rather than only its key;
        // envelopes are immutable, so the "look it up afterwards" alternative
        // would be a needless second round trip.
        let mut watch = self
            .collection
            .watch()
            .full_document(FullDocumentType::UpdateLookup);

        let resuming = matches!(from, Resume::At(_));
        if let Resume::At(position) = &from {
            watch = watch.start_after(Self::decode_token(position)?);
        }

        let stream = watch.await.map_err(|e| {
            if resuming && Self::is_server_rejection(&e) {
                let position = match &from {
                    Resume::At(p) => p.clone(),
                    Resume::Cold => String::new(),
                };
                CdcError::UnusablePosition {
                    connector: CONNECTOR,
                    position,
                    reason: format!(
                        "MongoDB refused the resume token; the oplog has rolled past it, the \
                         database was rebuilt, or the token belongs to another cluster ({e})"
                    ),
                }
            } else {
                CdcError::Backend(anyhow::anyhow!("opening a change stream: {e}"))
            }
        })?;

        // The position the snapshot's last record will carry: where the stream
        // was when it opened, i.e. immediately *before* the snapshot ran.
        // Resuming from it replays anything written during the snapshot and
        // skips nothing.
        let start_token = stream.resume_token().map(|t| Self::encode_token(&t));

        // ── the snapshot ──
        let mut snapshot_records: Vec<Result<ChangeRecord, CdcError>> = Vec::new();
        if matches!(from, Resume::Cold) && mode.snapshots_on_cold_start() {
            let mut cursor = self
                .collection
                .find(bson::doc! {})
                .await
                .map_err(|e| CdcError::Backend(anyhow::anyhow!("snapshotting: {e}")))?;
            let mut documents = Vec::new();
            while let Some(next) = cursor.next().await {
                documents.push(
                    next.map_err(|e| CdcError::Backend(anyhow::anyhow!("snapshotting: {e}")))?,
                );
            }

            let last = documents.len().saturating_sub(1);
            for (i, document) in documents.iter().enumerate() {
                // Only the FINAL snapshot record carries a position, and it is
                // the pre-snapshot token. An earlier one would let a restart
                // resume the live stream from a point that has not yet been
                // snapshotted past.
                let (flag, position) = if i == last {
                    (
                        Snapshot::Last,
                        match &start_token {
                            Some(Ok(t)) => Some(t.clone()),
                            _ => None,
                        },
                    )
                } else {
                    (Snapshot::True, None)
                };
                if let Some(record) = self.record(document, Op::Read, flag, position) {
                    snapshot_records.push(Ok(record));
                }
            }
        }

        let entity = self.entity.clone();
        let live = stream::unfold(stream, move |mut stream| {
            let entity = entity.clone();
            async move {
                loop {
                    let event: ChangeStreamEvent<Document> = match stream.next().await? {
                        Ok(e) => e,
                        Err(e) => {
                            let err = if MongoSource::is_history_lost(&e) {
                                CdcError::UnusablePosition {
                                    connector: CONNECTOR,
                                    position: "<in-flight>".to_string(),
                                    reason: format!("change stream history lost mid-stream ({e})"),
                                }
                            } else {
                                CdcError::Backend(anyhow::anyhow!("change stream: {e}"))
                            };
                            return Some((Err(err), stream));
                        }
                    };

                    // Envelope collections are append-only, so an insert is the
                    // only operation that carries a committed write. Anything
                    // else (an out-of-band update, a drop) is deliberately not
                    // translated into a change record — silently inventing a
                    // `c` for it would put a write on the topic that the mesh
                    // never made.
                    if event.operation_type != OperationType::Insert {
                        continue;
                    }
                    let Some(document) = event.full_document.as_ref() else {
                        continue;
                    };
                    let position = match MongoSource::encode_token(&event.id) {
                        Ok(p) => Some(p),
                        Err(e) => return Some((Err(e), stream)),
                    };

                    let Some(envelope) = meshql_mongo::converters::document_to_envelope(document)
                    else {
                        continue;
                    };
                    let ts_ms = envelope.created_at.timestamp_millis();
                    let record = ChangeRecord::new(
                        Op::Create,
                        envelope,
                        SourceInfo {
                            connector: CONNECTOR.to_string(),
                            entity: entity.clone(),
                            ts_ms,
                            position,
                            snapshot: Snapshot::False,
                        },
                    );
                    return Some((Ok(record), stream));
                }
            }
        });

        Ok(Box::pin(stream::iter(snapshot_records).chain(live)))
    }
}
