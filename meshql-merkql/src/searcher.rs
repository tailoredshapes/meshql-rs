use async_trait::async_trait;
use handlebars::Handlebars;
use merkql::broker::BrokerRef;
use merkql::consumer::{ConsumerConfig, OffsetReset};
use meshql_core::{Envelope, MeshqlError, Result, Searcher, Stash};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

use crate::matcher;

pub struct MerkqlSearcher {
    broker: BrokerRef,
    topic: String,
}

impl MerkqlSearcher {
    pub fn new(broker: BrokerRef, topic: impl Into<String>) -> Self {
        Self {
            broker,
            topic: topic.into(),
        }
    }

    /// Render a Handlebars template with the given args Stash.
    fn render_template(&self, template: &str, args: &Stash) -> Result<serde_json::Value> {
        let mut hbs = Handlebars::new();
        hbs.set_strict_mode(false);
        let rendered = hbs
            .render_template(template, args)
            .map_err(|e| MeshqlError::Template(e.to_string()))?;
        serde_json::from_str(&rendered).map_err(|e| MeshqlError::Parse(e.to_string()))
    }

    /// Read all envelopes from the topic, returning the latest non-deleted per ID
    /// filtered by envelope.created_at milliseconds <= cutoff_ms.
    fn scan_latest(&self, cutoff_ms: i64) -> Result<Vec<(Envelope, serde_json::Value)>> {
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

        // Collect all records grouped by id → latest envelope within cutoff
        // Use millisecond comparison to avoid sub-millisecond precision issues
        let mut by_id: HashMap<String, (i64, Envelope)> = HashMap::new();

        loop {
            let batch = consumer
                .poll(Duration::from_millis(50))
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            if batch.is_empty() {
                break;
            }
            for rec in batch {
                let env: Envelope = serde_json::from_str(&rec.value)
                    .map_err(|e| MeshqlError::Parse(e.to_string()))?;

                // Use millisecond precision for cutoff comparison
                let env_ms = env.created_at.timestamp_millis();
                if env_ms > cutoff_ms {
                    continue;
                }

                let entry = by_id
                    .entry(env.id.clone())
                    .or_insert_with(|| (env_ms, env.clone()));
                if env_ms >= entry.0 {
                    *entry = (env_ms, env);
                }
            }
        }

        // Convert to (Envelope, full JSON for matching) — filter deleted
        let results: Vec<(Envelope, serde_json::Value)> = by_id
            .into_values()
            .filter(|(_, env)| !env.deleted)
            .map(|(_, env)| {
                // Build record JSON with top-level id and payload sub-object
                let record_json = json!({
                    "id": env.id,
                    "payload": env.payload,
                });
                (env, record_json)
            })
            .collect();

        Ok(results)
    }

    /// Convert an Envelope to a result Stash (payload fields + id merged in).
    fn envelope_to_stash(env: &Envelope) -> Stash {
        let mut stash = env.payload.clone();
        stash.insert("id".to_string(), json!(env.id));
        stash
    }
}

#[async_trait]
impl Searcher for MerkqlSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        _creds: &[String],
        at: i64,
    ) -> Result<Option<Stash>> {
        let query = self.render_template(template, args)?;
        let records = self.scan_latest(at)?;

        let result = records
            .into_iter()
            .find(|(_, record_json)| matcher::matches(record_json, &query))
            .map(|(env, _)| Self::envelope_to_stash(&env));

        Ok(result)
    }

    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        _creds: &[String],
        at: i64,
    ) -> Result<Vec<Stash>> {
        let query = self.render_template(template, args)?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .map(|v| v as usize);

        let records = self.scan_latest(at)?;

        let mut results: Vec<Stash> = records
            .into_iter()
            .filter(|(_, record_json)| matcher::matches(record_json, &query))
            .map(|(env, _)| Self::envelope_to_stash(&env))
            .collect();

        if let Some(lim) = limit {
            results.truncate(lim);
        }

        Ok(results)
    }
}
