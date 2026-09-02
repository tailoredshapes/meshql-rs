//! The portable, poll-based ChangeSource: one `find_all` per poll, diffed
//! against kept state by payload hash. Works against any certified
//! Searcher+Repository pair.
//!
//! Why payload hash: `find_all` rows are payload + `"id"` only — no
//! Envelope metadata. Commit time and the authorization mark are recovered by
//! a point `Repository::read` per *changed* envelope (a handful per poll, not
//! N+1 over the table).
//!
//! Known blind spots (inherent to the row surface):
//! - A byte-identical payload rewrite is not notified (hash unchanged) —
//!   no refetch would show anything new.
//! - An authorization-only change with an identical payload is likewise
//!   undetectable; a stale mark is kept until the next payload change or
//!   delete.
//! - Within one poll, events across *different* ids follow `find_all` row
//!   order; per-id ordering by `created_at` holds, which is what the
//!   idempotent-refetch contract needs.
//!
//! The poll itself runs under an explicit system session. A tail is a caller
//! outside any request, and there is no unset session — "no session means
//! allow" is exactly the bypass the plugin-owns-authorization design removes,
//! so the tail names what it is instead of leaving the question open.

use crate::{ChangeEvent, ChangeSource};
use async_trait::async_trait;
use meshql_core::{AuthMark, Repository, Searcher, Stash, SystemSession};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

struct Known {
    payload_hash: u64,
    /// The last authorization mark seen for this id. A deletion tombstone can
    /// arrive carrying nothing, so this is what a subscriber's session is
    /// asked about when deciding who hears about the delete.
    auth: AuthMark,
}

pub struct SearcherTail {
    entity: String,
    searcher: Arc<dyn Searcher>,
    repository: Arc<dyn Repository>,
    state: tokio::sync::Mutex<HashMap<String, Known>>,
}

impl SearcherTail {
    pub fn new(
        entity: impl Into<String>,
        searcher: Arc<dyn Searcher>,
        repository: Arc<dyn Repository>,
    ) -> Self {
        Self {
            entity: entity.into(),
            searcher,
            repository,
            state: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// NB: serde_json resolves with `preserve_order` in this workspace
    /// (transitive feature unification), so serialization follows key
    /// insertion order, NOT sorted order. Still sound here: rows
    /// deserialize from stored JSON text, so the same stored version
    /// always serializes identically (no false negatives). A key-order-only
    /// difference between versions hashes as a change — a harmless extra
    /// notification (client refetch is idempotent). In-process hash only —
    /// never persisted.
    ///
    /// `createdAt` (the opt-in "honesty" as-of field some searchers now
    /// inject) is excluded: it changes on every rewrite by definition, even
    /// a byte-identical one, so hashing it would defeat the whole point of
    /// this dedup — every no-op rewrite would look like a change.
    fn hash_row(row: &Stash) -> u64 {
        let mut h = DefaultHasher::new();
        let mut row = row.clone();
        row.remove("createdAt");
        serde_json::to_string(&row)
            .expect("Stash is always serializable")
            .hash(&mut h);
        h.finish()
    }
}

#[async_trait]
impl ChangeSource for SearcherTail {
    fn entity(&self) -> &str {
        &self.entity
    }

    /// At-least-once discipline: `state` is committed only after every
    /// fallible operation in the poll has succeeded. A mid-poll error
    /// (e.g. a transient point-read failure on a networked backend)
    /// drops the buffered events AND the staged state, so the next poll
    /// re-detects and re-emits everything — events are never lost to a
    /// state map that advanced past them.
    async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let rows: Vec<Stash> = self
            .searcher
            .find_all("{}", &Stash::new(), &SystemSession, now_ms)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let mut state = self.state.lock().await;
        let mut out = Vec::new();
        let mut staged_upserts: Vec<(String, Known)> = Vec::new();
        let mut staged_removals: Vec<String> = Vec::new();
        let mut present: HashSet<String> = HashSet::new();

        for row in &rows {
            let Some(id) = row.get("id").and_then(|v| v.as_str()).map(String::from) else {
                continue;
            };
            present.insert(id.clone());
            let row_hash = Self::hash_row(row);
            let last_known_auth = match state.get(&id) {
                None => None, // create
                Some(known) if known.payload_hash != row_hash => Some(known.auth.clone()),
                Some(_) => continue, // unchanged
            };

            // Point-read to recover commit time + the mark. If the envelope
            // vanished between find_all and this read (delete race), emit a
            // delete carrying the last known mark instead.
            match self
                .repository
                .read(&id, &SystemSession, None)
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
            {
                Some(env) => {
                    staged_upserts.push((
                        id.clone(),
                        Known {
                            payload_hash: row_hash,
                            auth: env.auth.clone(),
                        },
                    ));
                    out.push(ChangeEvent {
                        entity: self.entity.clone(),
                        id,
                        created_at: env.created_at.timestamp_millis(),
                        deleted: false,
                        auth: env.auth,
                        cursor: None,
                        payload: None,
                    });
                }
                None => {
                    staged_removals.push(id.clone());
                    out.push(ChangeEvent {
                        entity: self.entity.clone(),
                        id,
                        created_at: now_ms,
                        deleted: true,
                        auth: last_known_auth.unwrap_or_default(),
                        cursor: None,
                        payload: None,
                    });
                }
            }
        }

        // Disappearances are deletes (tombstones are invisible to find_all).
        let gone: Vec<String> = state
            .keys()
            .filter(|id| !present.contains(*id))
            .cloned()
            .collect();
        for id in &gone {
            let known = state.get(id).expect("key just listed");
            out.push(ChangeEvent {
                entity: self.entity.clone(),
                id: id.clone(),
                created_at: now_ms,
                deleted: true,
                auth: known.auth.clone(),
                cursor: None,
                payload: None,
            });
        }

        // Everything fallible has succeeded — commit the new state.
        for (id, known) in staged_upserts {
            state.insert(id, known);
        }
        for id in staged_removals.into_iter().chain(gone) {
            state.remove(&id);
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshql_core::{Envelope, MeshqlError, Result as CoreResult, Session};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Searcher yielding two fixed rows (deterministic order); Repository
    /// whose `read` fails exactly once, on `row-b`. Pins the at-least-once
    /// contract for the MID-poll failure: `row-a`'s read succeeds (event
    /// buffered, state staged), then `row-b`'s read fails — the buffered
    /// row-a event is dropped with the Err, so its state must NOT have
    /// been committed, or row-a's create is lost forever. The pre-fix
    /// code committed state per-row and fails this test by emitting only
    /// row-b on the second poll.
    struct FixedSearcher;

    #[async_trait]
    impl Searcher for FixedSearcher {
        async fn find(
            &self,
            _t: &str,
            _a: &Stash,
            _session: &dyn Session,
            _at: i64,
        ) -> CoreResult<Option<Stash>> {
            unimplemented!("not used by SearcherTail")
        }
        async fn find_all(
            &self,
            _t: &str,
            _a: &Stash,
            _session: &dyn Session,
            _at: i64,
        ) -> CoreResult<Vec<Stash>> {
            let mut row_a = Stash::new();
            row_a.insert("id".to_string(), json!("row-a"));
            row_a.insert("name".to_string(), json!("henrietta"));
            let mut row_b = Stash::new();
            row_b.insert("id".to_string(), json!("row-b"));
            row_b.insert("name".to_string(), json!("clucky"));
            Ok(vec![row_a, row_b])
        }
    }

    struct FlakyReadRepo {
        fail_next: AtomicBool,
    }

    #[async_trait]
    impl Repository for FlakyReadRepo {
        async fn create(&self, _e: Envelope, _session: &dyn Session) -> CoreResult<Envelope> {
            unimplemented!()
        }
        async fn read(
            &self,
            id: &str,
            _session: &dyn Session,
            _at: Option<chrono::DateTime<chrono::Utc>>,
        ) -> CoreResult<Option<Envelope>> {
            // Fail exactly once, and only on row-b — AFTER row-a's read
            // has already succeeded within the same poll.
            if id == "row-b" && self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(MeshqlError::Storage("transient read failure".into()));
            }
            let mut payload = Stash::new();
            payload.insert("name".to_string(), json!("recovered"));
            Ok(Some(Envelope::new(id, payload, vec!["team".to_string()])))
        }
        async fn list(&self, _session: &dyn Session) -> CoreResult<Vec<Envelope>> {
            unimplemented!()
        }
        async fn remove(&self, _id: &str, _session: &dyn Session) -> CoreResult<bool> {
            unimplemented!()
        }
        async fn create_many(
            &self,
            _e: Vec<Envelope>,
            _session: &dyn Session,
        ) -> CoreResult<Vec<Envelope>> {
            unimplemented!()
        }
        async fn read_many(
            &self,
            _ids: &[String],
            _session: &dyn Session,
        ) -> CoreResult<Vec<Envelope>> {
            unimplemented!()
        }
        async fn remove_many(
            &self,
            _ids: &[String],
            _session: &dyn Session,
        ) -> CoreResult<std::collections::HashMap<String, bool>> {
            unimplemented!()
        }
        /// A test double for the change tail. Version listing is not part of what it
        /// fakes, so it reports an empty history rather than pretending to one.
        async fn list_versions(
            &self,
            _id: &str,
            _session: &dyn Session,
        ) -> meshql_core::Result<Vec<meshql_core::versions::VersionRef>> {
            Ok(Vec::new())
        }

        async fn read_version(
            &self,
            _id: &str,
            _token: &str,
            _session: &dyn Session,
        ) -> meshql_core::Result<Option<meshql_core::Envelope>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn mid_poll_read_failure_does_not_lose_earlier_events() {
        let tail = SearcherTail::new(
            "hen",
            Arc::new(FixedSearcher),
            Arc::new(FlakyReadRepo {
                fail_next: AtomicBool::new(true),
            }),
        );

        // First poll: row-a's read SUCCEEDS, then row-b's read fails →
        // poll errors, and row-a's staged state must NOT be committed
        // (its buffered event was dropped with the Err).
        assert!(tail.poll().await.is_err());

        // Second poll: both reads succeed → BOTH creates are emitted.
        // Pre-fix code committed row-a's hash during the failed poll and
        // emits only row-b here — losing row-a forever.
        let events = tail.poll().await.expect("second poll succeeds");
        let mut ids: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec!["row-a", "row-b"],
            "both events re-emitted after transient failure"
        );
        assert!(events.iter().all(|e| !e.deleted));
        assert!(events
            .iter()
            .all(|e| e.auth.as_parts() == ["team".to_string()]));
    }
}
