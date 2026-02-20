use async_trait::async_trait;
use chrono::{DateTime, Utc};
use meshql_core::{Envelope, MeshqlError, Repository, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::client::ConfluentClient;
use crate::config::KsqlConfig;
use crate::converters::{envelope_to_kafka_value, row_to_envelope};

pub struct KsqlRepository {
    client: Arc<ConfluentClient>,
    topic: String,
    stream_name: String,
    table_name: String,
    max_retries: u32,
    retry_delay_ms: u64,
}

impl KsqlRepository {
    pub fn new(client: Arc<ConfluentClient>, entity: &str, config: &KsqlConfig) -> Self {
        Self {
            client,
            topic: KsqlConfig::topic_name(entity),
            stream_name: KsqlConfig::stream_name(entity),
            table_name: KsqlConfig::table_name(entity),
            max_retries: config.max_retries,
            retry_delay_ms: config.retry_delay_ms,
        }
    }

    /// Run DDL to create the ksqlDB stream and materialized table.
    /// Idempotent — uses IF NOT EXISTS.
    pub async fn initialize(&self) -> anyhow::Result<()> {
        let create_stream = format!(
            "CREATE STREAM IF NOT EXISTS {} (\
             id VARCHAR KEY, \
             payload VARCHAR, \
             created_at BIGINT, \
             deleted BOOLEAN, \
             authorized_tokens VARCHAR\
             ) WITH (KAFKA_TOPIC='{}', VALUE_FORMAT='JSON');",
            self.stream_name, self.topic
        );

        let create_table = format!(
            "CREATE TABLE IF NOT EXISTS {} AS \
             SELECT id, \
             LATEST_BY_OFFSET(payload) AS payload, \
             LATEST_BY_OFFSET(created_at) AS created_at, \
             LATEST_BY_OFFSET(deleted) AS deleted, \
             LATEST_BY_OFFSET(authorized_tokens) AS authorized_tokens \
             FROM {} GROUP BY id EMIT CHANGES;",
            self.table_name, self.stream_name
        );

        info!(
            "Initializing ksqlDB stream and table for topic: {}",
            self.topic
        );
        self.client.execute_statement(&create_stream).await?;
        self.client.execute_statement(&create_table).await?;
        self.wait_for_table_ready().await;
        Ok(())
    }

    async fn wait_for_table_ready(&self) {
        for i in 0..self.max_retries {
            if self.client.is_table_ready(&self.table_name).await {
                info!("ksqlDB table {} is ready", self.table_name);
                return;
            }
            debug!(
                "Waiting for table {} (attempt {}/{})",
                self.table_name,
                i + 1,
                self.max_retries
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(self.retry_delay_ms)).await;
        }
        warn!(
            "ksqlDB table {} may not be ready after waiting",
            self.table_name
        );
    }

    fn escape_id(id: &str) -> String {
        id.replace('\'', "''")
    }
}

#[async_trait]
impl Repository for KsqlRepository {
    async fn create(&self, envelope: Envelope, _tokens: &[String]) -> Result<Envelope> {
        let kafka_value = envelope_to_kafka_value(&envelope);

        self.client
            .produce_record(&self.topic, &envelope.id, &kafka_value)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        Ok(envelope)
    }

    async fn read(
        &self,
        id: &str,
        _tokens: &[String],
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        let escaped_id = Self::escape_id(id);

        if at.is_some() {
            // Temporal read: query the STREAM for all versions, filter client-side.
            // ksqlDB stream pull queries may not support key-based lookup on
            // Confluent Cloud, so we fall back to TABLE read (latest only).
            let query = format!(
                "SELECT * FROM {} WHERE id = '{}';",
                self.table_name, escaped_id
            );

            for _ in 0..self.max_retries {
                match self.client.pull_query(&query).await {
                    Ok(rows) if !rows.is_empty() => {
                        let env = row_to_envelope(&rows[0])
                            .map_err(|e| MeshqlError::Parse(e.to_string()))?;
                        if env.deleted {
                            return Ok(None);
                        }
                        return Ok(Some(env));
                    }
                    Ok(_) => {
                        tokio::time::sleep(tokio::time::Duration::from_millis(self.retry_delay_ms))
                            .await;
                    }
                    Err(e) => {
                        debug!("Read query not ready: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_millis(self.retry_delay_ms))
                            .await;
                    }
                }
            }
            Ok(None)
        } else {
            // Current read: query the TABLE for latest state.
            let query = format!(
                "SELECT * FROM {} WHERE id = '{}';",
                self.table_name, escaped_id
            );

            for _ in 0..self.max_retries {
                match self.client.pull_query(&query).await {
                    Ok(rows) if !rows.is_empty() => {
                        let env = row_to_envelope(&rows[0])
                            .map_err(|e| MeshqlError::Parse(e.to_string()))?;
                        if env.deleted {
                            return Ok(None);
                        }
                        return Ok(Some(env));
                    }
                    Ok(_) => {
                        tokio::time::sleep(tokio::time::Duration::from_millis(self.retry_delay_ms))
                            .await;
                    }
                    Err(e) => {
                        debug!("Read query not ready: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_millis(self.retry_delay_ms))
                            .await;
                    }
                }
            }
            Ok(None)
        }
    }

    async fn list(&self, _tokens: &[String]) -> Result<Vec<Envelope>> {
        let query = format!("SELECT * FROM {} WHERE deleted = false;", self.table_name);

        for _ in 0..self.max_retries {
            match self.client.pull_query(&query).await {
                Ok(rows) if !rows.is_empty() => {
                    let mut envelopes = Vec::new();
                    for row in &rows {
                        match row_to_envelope(row) {
                            Ok(env) if !env.deleted => envelopes.push(env),
                            Ok(_) => {} // skip deleted
                            Err(e) => {
                                warn!("Failed to parse row: {}", e);
                            }
                        }
                    }
                    return Ok(envelopes);
                }
                Ok(_) => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(self.retry_delay_ms))
                        .await;
                }
                Err(e) => {
                    debug!("List query not ready: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_millis(self.retry_delay_ms))
                        .await;
                }
            }
        }

        Ok(Vec::new())
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
                let kafka_value = envelope_to_kafka_value(&deleted_env);
                self.client
                    .produce_record(&self.topic, id, &kafka_value)
                    .await
                    .map_err(|e| MeshqlError::Storage(e.to_string()))?;
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
