//! Workers — the merkql → fold → noun half of the pipeline.
//!
//! A worker owns one projector and one noun repository. It consumes the shared
//! `egg-events` topic, folds events into domain state, and writes the resulting
//! noun. Because a worker rebuilds its noun from the full log, `run_once` is
//! simultaneously "catch up" and "replay from scratch" — there is only one code
//! path, which is what makes a projection safe to drop and rebuild.

use crate::events::{EventRecord, TOPIC};
use crate::projectors::Projector;
use merkql::broker::{Broker, BrokerRef};
use merkql::consumer::{ConsumerConfig, OffsetReset};
use meshql_core::{Envelope, Repository};
use std::sync::Arc;
use std::time::Duration;

pub struct Worker {
    broker: BrokerRef,
    projector: Box<dyn Projector>,
    repo: Arc<dyn Repository>,
    tokens: Vec<String>,
}

impl Worker {
    pub fn new(broker: BrokerRef, projector: Box<dyn Projector>, repo: Arc<dyn Repository>) -> Self {
        // Workers run as system principals; nouns they write inherit "*" (public
        // read) here for the example. A real deployment would scope tokens.
        Self { broker, projector, repo, tokens: vec!["*".to_string()] }
    }

    pub fn noun(&self) -> &'static str {
        self.projector.noun()
    }

    /// Rebuild the noun from the entire event log, then write only the nouns
    /// whose payload changed. Idempotent: running it again over the same log is
    /// a no-op. This is both steady-state catch-up and full replay.
    pub async fn run_once(&mut self) -> anyhow::Result<usize> {
        self.projector.reset();

        let mut consumer = Broker::consumer(
            &self.broker,
            ConsumerConfig {
                group_id: uuid::Uuid::new_v4().to_string(),
                auto_commit: false,
                offset_reset: OffsetReset::Earliest,
            },
        );
        consumer
            .subscribe(&[TOPIC])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        loop {
            let batch = consumer
                .poll(Duration::from_millis(50))
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            if batch.is_empty() {
                break;
            }
            for rec in batch {
                let v: serde_json::Value = match serde_json::from_str(&rec.value) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some(ev) = EventRecord::from_json(&v) else { continue };
                if self.projector.consumes(&ev.verb) {
                    self.projector.apply(&ev);
                }
            }
        }

        let mut written = 0;
        for up in self.projector.snapshot() {
            let current = self.repo.read(&up.id, &self.tokens, None).await?;
            let unchanged = current.map(|e| e.payload == up.payload).unwrap_or(false);
            if !unchanged {
                let env = Envelope::new(up.id, up.payload, self.tokens.clone());
                self.repo.create(env, &self.tokens).await?;
                written += 1;
            }
        }
        Ok(written)
    }

    /// Poll-and-rebuild forever. Each tick catches the noun up to the log.
    pub async fn run_forever(mut self, interval: Duration) {
        loop {
            if let Err(e) = self.run_once().await {
                eprintln!("[worker {}] {e}", self.projector.noun());
            }
            tokio::time::sleep(interval).await;
        }
    }
}
