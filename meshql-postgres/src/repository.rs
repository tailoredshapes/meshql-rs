use async_trait::async_trait;
use chrono::{DateTime, Utc};
use meshql_core::{Envelope, MeshqlError, Repository, Result};
use sqlx::{PgPool, Row};
use std::collections::HashMap;

pub struct PostgresRepository {
    pub pool: PgPool,
    pub table: String,
}

impl PostgresRepository {
    /// Create a new repository using the default table name `envelopes`.
    pub async fn new(database_url: &str) -> Result<Self> {
        Self::new_with_table(database_url, "envelopes").await
    }

    /// Create a new repository with a custom table name (useful for test isolation).
    pub async fn new_with_table(database_url: &str, table: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let repo = Self {
            pool,
            table: table.to_string(),
        };
        repo.init_schema().await?;
        Ok(repo)
    }

    async fn init_schema(&self) -> Result<()> {
        let create_table = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                id TEXT NOT NULL,
                created_at_ms BIGINT NOT NULL,
                deleted BOOLEAN NOT NULL DEFAULT FALSE,
                authorized_tokens TEXT NOT NULL,
                payload TEXT NOT NULL
            )",
            self.table
        );
        sqlx::query(&create_table)
            .execute(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let create_index = format!(
            "CREATE INDEX IF NOT EXISTS idx_{}_id ON {}(id)",
            self.table, self.table
        );
        sqlx::query(&create_index)
            .execute(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        Ok(())
    }

    fn row_to_envelope(row: &sqlx::postgres::PgRow) -> Result<Envelope> {
        let id: String = row
            .try_get("id")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let created_at_ms: i64 = row
            .try_get("created_at_ms")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let deleted: bool = row
            .try_get("deleted")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let tokens_json: String = row
            .try_get("authorized_tokens")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let payload_json: String = row
            .try_get("payload")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let created_at =
            DateTime::from_timestamp_millis(created_at_ms).unwrap_or_default();
        let authorized_tokens: Vec<String> =
            serde_json::from_str(&tokens_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        let payload: meshql_core::Stash =
            serde_json::from_str(&payload_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        Ok(Envelope {
            id,
            payload,
            created_at,
            deleted,
            authorized_tokens,
        })
    }
}

#[async_trait]
impl Repository for PostgresRepository {
    async fn create(&self, envelope: Envelope, tokens: &[String]) -> Result<Envelope> {
        let mut env = envelope;
        if env.id.is_empty() {
            env.id = uuid::Uuid::new_v4().to_string();
        }
        env.authorized_tokens = tokens.to_vec();

        let created_at_ms = env.created_at.timestamp_millis();
        let tokens_json =
            serde_json::to_string(&env.authorized_tokens).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        let payload_json =
            serde_json::to_string(&env.payload).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        let sql = format!(
            "INSERT INTO {} (id, created_at_ms, deleted, authorized_tokens, payload) VALUES ($1, $2, $3, $4, $5)",
            self.table
        );
        sqlx::query(&sql)
            .bind(&env.id)
            .bind(created_at_ms)
            .bind(env.deleted)
            .bind(&tokens_json)
            .bind(&payload_json)
            .execute(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        Ok(env)
    }

    async fn read(
        &self,
        id: &str,
        _tokens: &[String],
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        let cutoff_ms = match at {
            Some(t) => t.timestamp_millis(),
            None => Utc::now().timestamp_millis() + 1,
        };

        let sql = format!(
            "SELECT id, created_at_ms, deleted, authorized_tokens, payload
             FROM {} WHERE id = $1 AND created_at_ms <= $2
             ORDER BY created_at_ms DESC LIMIT 1",
            self.table
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(cutoff_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        match row {
            None => Ok(None),
            Some(r) => {
                let env = Self::row_to_envelope(&r)?;
                if env.deleted {
                    Ok(None)
                } else {
                    Ok(Some(env))
                }
            }
        }
    }

    async fn list(&self, _tokens: &[String]) -> Result<Vec<Envelope>> {
        let sql = format!(
            "SELECT DISTINCT ON (id) id, created_at_ms, deleted, authorized_tokens, payload
             FROM {} WHERE deleted = FALSE
             ORDER BY id, created_at_ms DESC",
            self.table
        );
        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(Self::row_to_envelope(&row)?);
        }
        Ok(results)
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
                self.create(deleted_env, tokens).await?;
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
            let deleted = self.remove(id, tokens).await?;
            results.insert(id.clone(), deleted);
        }
        Ok(results)
    }
}
