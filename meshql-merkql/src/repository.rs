use async_trait::async_trait;
use chrono::{DateTime, Utc};
use merkql::broker::BrokerRef;
use merkql::consumer::{ConsumerConfig, OffsetReset};
use merkql::record::ProducerRecord;
use meshql_core::versions::{version_order, version_token, VersionRef};
use meshql_core::{Envelope, MeshqlError, Operation, Repository, Result, Session, SystemSession};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

pub struct MerkqlRepository {
    broker: BrokerRef,
    topic: String,
}

impl MerkqlRepository {
    pub fn new(broker: BrokerRef, topic: impl Into<String>) -> Self {
        Self {
            broker,
            topic: topic.into(),
        }
    }

    /// Read all envelopes from the topic.
    fn read_all_envelopes(&self) -> Result<Vec<Envelope>> {
        let mut consumer = merkql::broker::Broker::consumer(
            &self.broker,
            ConsumerConfig {
                group_id: uuid::Uuid::new_v4().to_string(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer
            .subscribe(&[&self.topic])
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut envelopes = Vec::new();
        loop {
            let batch = consumer
                .poll(Duration::from_millis(50))
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            if batch.is_empty() {
                break;
            }
            for rec in batch {
                let json: Value = serde_json::from_str(&rec.value)
                    .map_err(|e| MeshqlError::Parse(e.to_string()))?;
                let env: Envelope =
                    serde_json::from_value(json).map_err(|e| MeshqlError::Parse(e.to_string()))?;
                envelopes.push(env);
            }
        }
        Ok(envelopes)
    }

    /// Find the latest version of an envelope by ID, filtered by created_at milliseconds <= cutoff_ms.
    /// Uses millisecond precision to avoid sub-millisecond precision issues.
    fn latest_for_id(envelopes: &[Envelope], id: &str, cutoff_ms: i64) -> Option<Envelope> {
        envelopes
            .iter()
            .filter(|env| env.id == id && env.created_at.timestamp_millis() <= cutoff_ms)
            .max_by_key(|env| env.created_at.timestamp_millis())
            .cloned()
    }

    /// Get the latest non-deleted version of each unique ID (no temporal filter — uses max created_at).
    fn latest_per_id_not_deleted(envelopes: &[Envelope]) -> Vec<Envelope> {
        let mut latest: HashMap<String, Envelope> = HashMap::new();
        for env in envelopes {
            let entry = latest.entry(env.id.clone()).or_insert_with(|| env.clone());
            if env.created_at >= entry.created_at {
                *entry = env.clone();
            }
        }
        latest.into_values().filter(|env| !env.deleted).collect()
    }

    fn write_envelope(&self, envelope: &Envelope) -> Result<()> {
        let producer = merkql::broker::Broker::producer(&self.broker);
        let value =
            serde_json::to_string(envelope).map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let record = ProducerRecord::new(&self.topic, Some(envelope.id.clone()), value);
        producer
            .send(&record)
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl Repository for MerkqlRepository {
    async fn create(&self, envelope: Envelope, session: &dyn Session) -> Result<Envelope> {
        // The plugin owns the mark. Storage hands it the envelope and
        // persists whatever comes back, verbatim, in the same write as the
        // payload — so authorization can never become a dual write.
        let envelope = session.stamp(envelope);
        self.write_envelope(&envelope)?;
        Ok(envelope)
    }

    async fn read(
        &self,
        id: &str,
        session: &dyn Session,
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        // Convert optional cutoff to milliseconds; use current time if None
        let cutoff_ms = at.unwrap_or_else(Utc::now).timestamp_millis();
        // Add 1ms to handle sub-millisecond precision when reading "now"
        // (records created at the same millisecond should be included)
        let cutoff_ms = if at.is_none() {
            cutoff_ms + 1
        } else {
            cutoff_ms
        };
        let envelopes = self.read_all_envelopes()?;
        let result = Self::latest_for_id(&envelopes, id, cutoff_ms);
        // Return None if deleted or not visible to the caller. Visibility is
        // decided on the resolved version — filtering earlier would resurface
        // an older visible version of a now-restricted envelope.
        Ok(result.filter(|env| !env.deleted && session.is_authorized(Operation::Read, env)))
    }

    async fn list(&self, session: &dyn Session) -> Result<Vec<Envelope>> {
        let envelopes = self.read_all_envelopes()?;
        Ok(Self::latest_per_id_not_deleted(&envelopes)
            .into_iter()
            .filter(|env| session.is_authorized(Operation::Read, env))
            .collect())
    }

    async fn remove(&self, id: &str, session: &dyn Session) -> Result<bool> {
        // Resolve the record first, then ask the plugin about `Remove`
        // specifically. Reusing the authorized `read` would silently make
        // remove a synonym for read.
        let current = self.read(id, &SystemSession, None).await?;
        match current {
            None => Ok(false),
            Some(env) if !session.is_authorized(Operation::Remove, &env) => Ok(false),
            Some(env) => {
                let deleted_env = Envelope {
                    id: env.id,
                    payload: env.payload,
                    created_at: Utc::now(),
                    deleted: true,
                    auth: env.auth,
                };
                self.write_envelope(&deleted_env)?;
                Ok(true)
            }
        }
    }

    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        session: &dyn Session,
    ) -> Result<Vec<Envelope>> {
        let mut results = Vec::new();
        for env in envelopes {
            results.push(self.create(env, session).await?);
        }
        Ok(results)
    }

    async fn read_many(&self, ids: &[String], session: &dyn Session) -> Result<Vec<Envelope>> {
        let mut results = Vec::new();
        for id in ids {
            if let Some(env) = self.read(id, session, None).await? {
                results.push(env);
            }
        }
        Ok(results)
    }

    async fn remove_many(
        &self,
        ids: &[String],
        session: &dyn Session,
    ) -> Result<HashMap<String, bool>> {
        let mut results = HashMap::new();
        for id in ids {
            let ok = self.remove(id, session).await?;
            results.insert(id.clone(), ok);
        }
        Ok(results)
    }
    async fn list_versions(&self, id: &str, session: &dyn Session) -> Result<Vec<VersionRef>> {
        let mut mine: Vec<Envelope> = self
            .read_all_envelopes()?
            .into_iter()
            .filter(|e| e.id == id)
            .collect();
        mine.sort_by(version_order);
        Ok(mine
            .iter()
            .map(|e| {
                if session.is_authorized(Operation::Read, e) {
                    VersionRef::visible(e)
                } else {
                    VersionRef::tombstone(e)
                }
            })
            .collect())
    }

    async fn read_version(
        &self,
        id: &str,
        token: &str,
        session: &dyn Session,
    ) -> Result<Option<Envelope>> {
        for env in self
            .read_all_envelopes()?
            .into_iter()
            .filter(|e| e.id == id)
        {
            if version_token(&env) != token {
                continue;
            }
            // Unauthorized is not absent: the listing already reported it.
            if !session.is_authorized(Operation::Read, &env) {
                return Err(MeshqlError::Unauthorized);
            }
            return Ok(Some(env));
        }
        Ok(None)
    }
}
