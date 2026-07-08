//! The portable, poll-based ChangeSource: one `find_all` per poll, diffed
//! against kept state by payload hash. Works against any certified
//! Searcher+Repository pair.
//!
//! Why payload hash: `find_all` rows are payload + `"id"` only — no
//! Envelope metadata. Commit time and tokens are recovered by a point
//! `Repository::read` per *changed* envelope (a handful per poll, not
//! N+1 over the table).
//!
//! Backend caveat (see spec): the `["*"]` poll relies on searchers letting
//! a wildcard caller see everything. All backends except Mongo currently
//! do; on Mongo this tail is correct only under NoAuth until the adapter
//! aligns with the meshql-core convention.

use crate::{ChangeEvent, ChangeSource};
use async_trait::async_trait;
use meshql_core::{Repository, Searcher, Stash};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

struct Known {
    payload_hash: u64,
    tokens: Vec<String>,
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
    fn hash_row(row: &Stash) -> u64 {
        let mut h = DefaultHasher::new();
        serde_json::to_string(row)
            .expect("Stash is always serializable")
            .hash(&mut h);
        h.finish()
    }

    /// Point-read the envelope to recover commit time + tokens, and emit.
    /// If the envelope vanished between find_all and this read (delete
    /// race), emit a delete with last-known tokens instead.
    async fn emit_changed(
        &self,
        id: &str,
        last_known_tokens: Option<Vec<String>>,
        now_ms: i64,
        state: &mut HashMap<String, Known>,
        row_hash: u64,
        out: &mut Vec<ChangeEvent>,
    ) -> anyhow::Result<()> {
        let wildcard = ["*".to_string()];
        match self
            .repository
            .read(id, &wildcard, None)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
        {
            Some(env) => {
                state.insert(
                    id.to_string(),
                    Known {
                        payload_hash: row_hash,
                        tokens: env.authorized_tokens.clone(),
                    },
                );
                out.push(ChangeEvent {
                    entity: self.entity.clone(),
                    id: id.to_string(),
                    created_at: env.created_at.timestamp_millis(),
                    deleted: false,
                    authorized_tokens: env.authorized_tokens,
                });
            }
            None => {
                // Deleted between the list and the read.
                state.remove(id);
                out.push(ChangeEvent {
                    entity: self.entity.clone(),
                    id: id.to_string(),
                    created_at: now_ms,
                    deleted: true,
                    authorized_tokens: last_known_tokens.unwrap_or_default(),
                });
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ChangeSource for SearcherTail {
    fn entity(&self) -> &str {
        &self.entity
    }

    async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let rows: Vec<Stash> = self
            .searcher
            .find_all("{}", &Stash::new(), &["*".to_string()], now_ms)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        let mut state = self.state.lock().await;
        let mut out = Vec::new();
        let mut present: HashSet<String> = HashSet::new();

        for row in &rows {
            let Some(id) = row.get("id").and_then(|v| v.as_str()).map(String::from) else {
                continue;
            };
            present.insert(id.clone());
            let row_hash = Self::hash_row(row);
            match state.get(&id) {
                None => {
                    self.emit_changed(&id, None, now_ms, &mut state, row_hash, &mut out)
                        .await?;
                }
                Some(known) if known.payload_hash != row_hash => {
                    let last = known.tokens.clone();
                    self.emit_changed(&id, Some(last), now_ms, &mut state, row_hash, &mut out)
                        .await?;
                }
                Some(_) => {} // unchanged
            }
        }

        // Disappearances are deletes (tombstones are invisible to find_all).
        let gone: Vec<String> = state
            .keys()
            .filter(|id| !present.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            let known = state.remove(&id).expect("key just listed");
            out.push(ChangeEvent {
                entity: self.entity.clone(),
                id,
                created_at: now_ms,
                deleted: true,
                authorized_tokens: known.tokens,
            });
        }

        Ok(out)
    }
}
