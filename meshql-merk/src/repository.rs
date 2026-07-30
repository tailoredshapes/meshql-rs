//! A create-only `Repository`.
//!
//! `create` and `create_many` append. Every other trait method returns an
//! error. See the crate docs for why that is a hard failure rather than a slow
//! path, and [`crate::log`] for why it is structural rather than a convention.

use crate::conversion::{envelope_key, envelope_to_value};
use crate::log::AppendOnlyLog;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use merk_object::backend::Backend;
use merk_object::broker::BrokerRef;
use meshql_core::{Envelope, MeshqlError, Repository, Result};
use std::collections::HashMap;

/// Why a read is refused, in the words a caller will see in a 500.
///
/// One string, one place, so the six refusing methods cannot drift into six
/// different explanations — and so a test can assert on it.
pub const READ_REFUSED: &str = "meshql-merk is create-only: reading an event meshlette from the log \
would scan the topic from offset zero, which on object storage means downloading the entire log per \
query. Read the projection instead (meshql-dynamo).";

fn refuse(method: &str) -> MeshqlError {
    MeshqlError::Storage(format!("{method}: {READ_REFUSED}"))
}

pub struct MerkRepository<B: Backend> {
    log: AppendOnlyLog<B>,
}

impl<B: Backend> MerkRepository<B> {
    /// The constructor convention the other adapters use, minus a `db`: a log
    /// location has no database inside it, and the topic is the collection.
    ///
    /// The broker is borrowed, not stored. What is stored is an
    /// [`AppendOnlyLog`], which holds a producer and nothing else.
    pub fn new(broker: &BrokerRef<B>, topic: impl Into<String>) -> Self {
        Self {
            log: AppendOnlyLog::new(broker, topic),
        }
    }

    pub fn topic(&self) -> &str {
        self.log.topic()
    }

    /// Stamp the caller's tokens and mint an id if the caller supplied none,
    /// exactly as every other adapter's `create` does.
    ///
    /// Note what this deliberately does *not* do: refuse an empty or `"*"`
    /// token set. `meshql-core`'s visibility predicate treats both as public, so
    /// a record written with either is permanently world-readable, and the
    /// sociallymeshy design refuses such a write with a 500. That check belongs
    /// in the gateway, not here — `meshql-cert`'s authorization suite certifies
    /// that a record written without credentials *stays public*, so an adapter
    /// that refused it would fail certification and stop being interchangeable.
    fn prepare(mut envelope: Envelope, tokens: &[String]) -> Envelope {
        if envelope.id.is_empty() {
            envelope.id = uuid::Uuid::new_v4().to_string();
        }
        envelope.authorized_tokens = tokens.to_vec();
        envelope
    }
}

#[async_trait]
impl<B: Backend> Repository for MerkRepository<B> {
    async fn create(&self, envelope: Envelope, tokens: &[String]) -> Result<Envelope> {
        let env = Self::prepare(envelope, tokens);
        self.log
            .append(envelope_key(&env), envelope_to_value(&env)?)
            .await?;
        Ok(env)
    }

    /// One append per partition touched, not one per record: `send_batch`
    /// coalesces everything routing to the same partition into a single request.
    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        tokens: &[String],
    ) -> Result<Vec<Envelope>> {
        let prepared: Vec<Envelope> = envelopes
            .into_iter()
            .map(|e| Self::prepare(e, tokens))
            .collect();

        let mut entries = Vec::with_capacity(prepared.len());
        for env in &prepared {
            entries.push((envelope_key(env), envelope_to_value(env)?));
        }
        self.log.append_batch(entries).await?;
        Ok(prepared)
    }

    async fn read(
        &self,
        _id: &str,
        _tokens: &[String],
        _at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        Err(refuse("read"))
    }

    async fn list(&self, _tokens: &[String]) -> Result<Vec<Envelope>> {
        Err(refuse("list"))
    }

    async fn read_many(&self, _ids: &[String], _tokens: &[String]) -> Result<Vec<Envelope>> {
        Err(refuse("read_many"))
    }

    /// Refused for two reasons that both hold: it is a read-modify-write, and an
    /// event meshlette has no delete. Correction is a new event.
    async fn remove(&self, _id: &str, _tokens: &[String]) -> Result<bool> {
        Err(refuse("remove"))
    }

    async fn remove_many(
        &self,
        _ids: &[String],
        _tokens: &[String],
    ) -> Result<HashMap<String, bool>> {
        Err(refuse("remove_many"))
    }
}
