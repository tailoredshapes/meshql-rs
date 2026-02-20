use async_trait::async_trait;
use chrono::{DateTime, Utc};
use merkql::broker::BrokerRef;
use merkql::consumer::{ConsumerConfig, OffsetReset};
use merkql::record::ProducerRecord;
use meshql_core::{Envelope, MeshqlError, Repository, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::convert;

pub struct MerksqlRepository {
    broker: BrokerRef,
    topic: String,
    merksql: Arc<Mutex<merksql::MerkSql>>,
}

impl MerksqlRepository {
    pub fn new(
        broker: BrokerRef,
        topic: impl Into<String>,
        merksql: Arc<Mutex<merksql::MerkSql>>,
    ) -> Self {
        let topic = topic.into();
        // Register the table in merksql for this topic.
        // We use a generic schema since payload fields are dynamic.
        // The actual filtering happens in Rust after reading raw records.
        {
            let mut engine = merksql.lock().unwrap();
            let sql = format!(
                "CREATE TABLE {} (_id VARCHAR KEY, _data VARCHAR, _created_at BIGINT, _deleted BOOLEAN, _tokens VARCHAR) WITH (KAFKA_TOPIC='{}')",
                topic, topic
            );
            // Ignore errors if already registered
            let _ = engine.execute(&sql);
        }
        Self {
            broker,
            topic,
            merksql,
        }
    }

    /// Read all envelopes from the topic by scanning with a fresh consumer.
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
                if let Some(env) = convert::flat_json_to_envelope(&json) {
                    envelopes.push(env);
                }
            }
        }
        Ok(envelopes)
    }

    /// Find the latest version of an envelope by ID, filtered by created_at ms <= cutoff_ms.
    fn latest_for_id(envelopes: &[Envelope], id: &str, cutoff_ms: i64) -> Option<Envelope> {
        envelopes
            .iter()
            .filter(|env| env.id == id && env.created_at.timestamp_millis() <= cutoff_ms)
            .max_by_key(|env| env.created_at.timestamp_millis())
            .cloned()
    }

    /// Get the latest non-deleted version of each unique ID.
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
        let flat = convert::envelope_to_flat_json(envelope);
        let value =
            serde_json::to_string(&flat).map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let record = ProducerRecord::new(&self.topic, Some(envelope.id.clone()), value);
        producer
            .send(&record)
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Get the shared MerkSql instance.
    pub fn merksql(&self) -> &Arc<Mutex<merksql::MerkSql>> {
        &self.merksql
    }

    /// Get the topic name.
    pub fn topic(&self) -> &str {
        &self.topic
    }
}

#[async_trait]
impl Repository for MerksqlRepository {
    async fn create(&self, envelope: Envelope, _tokens: &[String]) -> Result<Envelope> {
        self.write_envelope(&envelope)?;
        Ok(envelope)
    }

    async fn read(
        &self,
        id: &str,
        _tokens: &[String],
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        let cutoff_ms = at.unwrap_or_else(Utc::now).timestamp_millis();
        let cutoff_ms = if at.is_none() {
            cutoff_ms + 1
        } else {
            cutoff_ms
        };
        let envelopes = self.read_all_envelopes()?;
        let result = Self::latest_for_id(&envelopes, id, cutoff_ms);
        Ok(result.filter(|env| !env.deleted))
    }

    async fn list(&self, _tokens: &[String]) -> Result<Vec<Envelope>> {
        let envelopes = self.read_all_envelopes()?;
        Ok(Self::latest_per_id_not_deleted(&envelopes))
    }

    async fn remove(&self, id: &str, tokens: &[String]) -> Result<bool> {
        let current = self.read(id, tokens, None).await?;
        match current {
            None => Ok(false),
            Some(env) => {
                let deleted_env = Envelope {
                    id: env.id,
                    payload: env.payload,
                    created_at: Utc::now(),
                    deleted: true,
                    authorized_tokens: env.authorized_tokens,
                };
                self.write_envelope(&deleted_env)?;
                Ok(true)
            }
        }
    }

    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        tokens: &[String],
    ) -> Result<Vec<Envelope>> {
        let mut results = Vec::new();
        for env in envelopes {
            results.push(self.create(env, tokens).await?);
        }
        Ok(results)
    }

    async fn read_many(&self, ids: &[String], tokens: &[String]) -> Result<Vec<Envelope>> {
        let mut results = Vec::new();
        for id in ids {
            if let Some(env) = self.read(id, tokens, None).await? {
                results.push(env);
            }
        }
        Ok(results)
    }

    async fn remove_many(
        &self,
        ids: &[String],
        tokens: &[String],
    ) -> Result<HashMap<String, bool>> {
        let mut results = HashMap::new();
        for id in ids {
            let ok = self.remove(id, tokens).await?;
            results.insert(id.clone(), ok);
        }
        Ok(results)
    }
}
