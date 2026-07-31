//! `Repository` over DynamoDB.

use async_trait::async_trait;
use aws_sdk_dynamodb::Client;
use chrono::{DateTime, Utc};
use meshql_core::{envelope_visible_to, Envelope, MeshqlError, Repository, Result};
use std::collections::HashMap;

use crate::metering::{return_consumed_capacity, CapacityMeter, Op};
use crate::store;

pub struct DynamoRepository {
    client: Client,
    table: String,
    /// `None` unless an operator asked for metering. See [`crate::metering`].
    meter: Option<std::sync::Arc<CapacityMeter>>,
}

impl DynamoRepository {
    /// `endpoint: None` → real AWS from the ambient config. `endpoint:
    /// Some(url)` → DynamoDB Local (see [`store::make_client`]).
    ///
    /// Creates the table if it is absent and waits for `ACTIVE`.
    pub async fn new(endpoint: Option<&str>, table: &str) -> Result<Self> {
        let client = store::make_client(endpoint).await;
        Self::new_with_client(client, table).await
    }

    /// Share one client (and one table) between a repository and a searcher.
    pub async fn new_with_client(client: Client, table: &str) -> Result<Self> {
        let repo = Self {
            client,
            table: table.to_string(),
            meter: None,
        };
        repo.ensure_table().await?;
        Ok(repo)
    }

    /// Account every request this repository makes against `meter`.
    ///
    /// Opt-in and additive: without it no request sets `ReturnConsumedCapacity`
    /// and there is nothing to pay for. Share one meter with a
    /// [`crate::DynamoSearcher`] over the same table to get the whole
    /// collection's bill in one report.
    pub fn with_meter(mut self, meter: std::sync::Arc<CapacityMeter>) -> Self {
        self.meter = Some(meter);
        self
    }

    /// The attached meter, if any.
    pub fn meter(&self) -> Option<&std::sync::Arc<CapacityMeter>> {
        self.meter.as_ref()
    }

    fn meter_ref(&self) -> Option<&CapacityMeter> {
        self.meter.as_deref()
    }

    pub async fn ensure_table(&self) -> Result<()> {
        store::ensure_table(&self.client, &self.table).await
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn table(&self) -> &str {
        &self.table
    }
}

#[async_trait]
impl Repository for DynamoRepository {
    async fn create(&self, envelope: Envelope, tokens: &[String]) -> Result<Envelope> {
        let mut env = envelope;
        if env.id.is_empty() {
            env.id = uuid::Uuid::new_v4().to_string();
        }
        env.authorized_tokens = tokens.to_vec();

        let put = self
            .client
            .put_item()
            .table_name(&self.table)
            .set_item(Some(store::envelope_to_item(&env)))
            .set_return_consumed_capacity(return_consumed_capacity(self.meter_ref()))
            .send()
            .await
            .map_err(|e| {
                MeshqlError::Storage(format!(
                    "put_item {} pk={}: {}",
                    self.table,
                    env.id,
                    store::describe_sdk_error(&e)
                ))
            })?;

        if let Some(m) = self.meter_ref() {
            m.record(Op::PutItem, put.consumed_capacity());
        }

        Ok(env)
    }

    async fn read(
        &self,
        id: &str,
        tokens: &[String],
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        let cutoff = match at {
            Some(t) => store::cutoff_nanos_from_millis(t.timestamp_millis()),
            None => store::now_cutoff_nanos(),
        };

        // Resolve the version *first*, then judge it. Deciding visibility before
        // resolving would let an older visible version of a now-restricted
        // record resurface — see
        // `test_repository_auth_latest_version_controls_visibility`.
        match store::query_latest(&self.client, &self.table, id, cutoff, self.meter_ref()).await? {
            None => Ok(None),
            Some(env) => {
                if env.deleted || !envelope_visible_to(&env, tokens) {
                    Ok(None)
                } else {
                    Ok(Some(env))
                }
            }
        }
    }

    async fn list(&self, tokens: &[String]) -> Result<Vec<Envelope>> {
        // No `at` on `list`, so the cutoff is now.
        let resolved = store::scan_latest(
            &self.client,
            &self.table,
            store::now_cutoff_nanos(),
            self.meter_ref(),
        )
        .await?;
        Ok(resolved
            .into_iter()
            .filter(|env| envelope_visible_to(env, tokens))
            .collect())
    }

    async fn remove(&self, id: &str, tokens: &[String]) -> Result<bool> {
        match self.read(id, tokens, None).await? {
            None => Ok(false),
            Some(env) => {
                // The tombstone carries the *resolved version's* tokens, not the
                // caller's. Stamping the caller's would change visibility as a
                // side effect of a delete, and
                // `test_repository_auth_denies_non_intersecting` depends on the
                // record still being bob's after alice's refused attempts.
                let tokens_of_record = env.authorized_tokens.clone();
                let tombstone = Envelope {
                    id: env.id,
                    payload: env.payload,
                    created_at: Utc::now(),
                    deleted: true,
                    authorized_tokens: tokens_of_record.clone(),
                };
                self.create(tombstone, &tokens_of_record).await?;
                Ok(true)
            }
        }
    }

    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        tokens: &[String],
    ) -> Result<Vec<Envelope>> {
        // A loop, not BatchWriteItem: the batch API caps at 25 items and cannot
        // express "a distinct sort key per item" any more cheaply than N puts.
        let mut results = Vec::with_capacity(envelopes.len());
        for env in envelopes {
            results.push(self.create(env, tokens).await?);
        }
        Ok(results)
    }

    async fn read_many(&self, ids: &[String], tokens: &[String]) -> Result<Vec<Envelope>> {
        // One `query` per id, run concurrently. BatchGetItem is not usable here:
        // it needs the full primary key, and the sort key of the resolved version
        // is exactly what we are trying to find out.
        let reads = ids.iter().map(|id| self.read(id, tokens, None));
        let mut results = Vec::with_capacity(ids.len());
        for found in futures::future::join_all(reads).await {
            if let Some(env) = found? {
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
            results.insert(id.clone(), self.remove(id, tokens).await?);
        }
        Ok(results)
    }
}
