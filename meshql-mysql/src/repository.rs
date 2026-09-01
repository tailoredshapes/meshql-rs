use async_trait::async_trait;
use chrono::{DateTime, Utc};
use meshql_core::versions::{version_order, version_token, VersionRef};
use meshql_core::{envelope_visible_to, Envelope, MeshqlError, Repository, Result, Stash};
use sqlx::MySqlPool;
use sqlx::Row;
use std::collections::HashMap;

pub struct MysqlRepository {
    pool: MySqlPool,
    table: String,
}

impl MysqlRepository {
    pub async fn new(database_url: &str) -> Result<Self> {
        Self::new_with_table(database_url, "envelopes").await
    }

    pub async fn new_with_table(database_url: &str, table: &str) -> Result<Self> {
        let pool = MySqlPool::connect(database_url)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let create_sql = format!(
            r#"CREATE TABLE IF NOT EXISTS `{table}` (
                id VARCHAR(255) NOT NULL,
                created_at_ms BIGINT NOT NULL,
                deleted TINYINT(1) NOT NULL DEFAULT 0,
                authorized_tokens TEXT NOT NULL,
                payload TEXT NOT NULL,
                INDEX idx_id (id),
                INDEX idx_id_ts (id, created_at_ms)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"#
        );

        sqlx::query(&create_sql)
            .execute(&pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        Ok(Self {
            pool,
            table: table.to_string(),
        })
    }

    /// Every version of one document, ordered by the shared `version_order`.
    /// MySQL cannot sort on the token, which is derived from content, so the
    /// order is applied in memory — the same way every other adapter does it,
    /// which is what makes them agree.
    async fn all_versions(&self, id: &str) -> Result<Vec<Envelope>> {
        let table = &self.table;
        let sql = format!(
            r#"SELECT id, created_at_ms, deleted, authorized_tokens, payload
               FROM `{table}` WHERE id = ?"#
        );
        let rows = sqlx::query(&sql)
            .bind(id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let env_id: String = r
                .try_get("id")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let created_at_ms: i64 = r
                .try_get("created_at_ms")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let deleted_flag: i8 = r
                .try_get("deleted")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let tokens_json: String = r
                .try_get("authorized_tokens")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let payload_json: String = r
                .try_get("payload")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            out.push(Self::row_to_envelope(
                env_id,
                created_at_ms,
                deleted_flag,
                tokens_json,
                payload_json,
            )?);
        }
        out.sort_by(version_order);
        Ok(out)
    }

    fn row_to_envelope(
        env_id: String,
        created_at_ms: i64,
        deleted_flag: i8,
        tokens_json: String,
        payload_json: String,
    ) -> Result<Envelope> {
        let created_at = DateTime::from_timestamp_millis(created_at_ms)
            .ok_or_else(|| MeshqlError::Parse(format!("Invalid timestamp: {created_at_ms}")))?;

        let authorized_tokens: Vec<String> =
            serde_json::from_str(&tokens_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        let payload: Stash =
            serde_json::from_str(&payload_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        Ok(Envelope {
            id: env_id,
            payload,
            created_at,
            deleted: deleted_flag != 0,
            authorized_tokens,
        })
    }
}

#[async_trait]
impl Repository for MysqlRepository {
    async fn create(&self, mut envelope: Envelope, tokens: &[String]) -> Result<Envelope> {
        if envelope.id.is_empty() {
            envelope.id = uuid::Uuid::new_v4().to_string();
        }
        envelope.authorized_tokens = tokens.to_vec();

        let table = &self.table;
        let sql = format!(
            "INSERT INTO `{table}` (id, created_at_ms, deleted, authorized_tokens, payload) VALUES (?, ?, ?, ?, ?)"
        );

        let created_at_ms = envelope.created_at.timestamp_millis();
        let deleted_flag: i8 = if envelope.deleted { 1 } else { 0 };
        let tokens_json = serde_json::to_string(&envelope.authorized_tokens)
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let payload_json = serde_json::to_string(&envelope.payload)
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        sqlx::query(&sql)
            .bind(&envelope.id)
            .bind(created_at_ms)
            .bind(deleted_flag)
            .bind(&tokens_json)
            .bind(&payload_json)
            .execute(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        Ok(envelope)
    }

    async fn read(
        &self,
        id: &str,
        tokens: &[String],
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        let cutoff_ms = at.unwrap_or_else(Utc::now).timestamp_millis() + 1;

        let table = &self.table;
        let sql = format!(
            r#"SELECT id, created_at_ms, deleted, authorized_tokens, payload
               FROM `{table}`
               WHERE id = ? AND created_at_ms <= ?
               ORDER BY created_at_ms DESC
               LIMIT 1"#
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
                let env_id: String = r
                    .try_get("id")
                    .map_err(|e| MeshqlError::Storage(e.to_string()))?;
                let created_at_ms: i64 = r
                    .try_get("created_at_ms")
                    .map_err(|e| MeshqlError::Storage(e.to_string()))?;
                let deleted_flag: i8 = r
                    .try_get("deleted")
                    .map_err(|e| MeshqlError::Storage(e.to_string()))?;
                let tokens_json: String = r
                    .try_get("authorized_tokens")
                    .map_err(|e| MeshqlError::Storage(e.to_string()))?;
                let payload_json: String = r
                    .try_get("payload")
                    .map_err(|e| MeshqlError::Storage(e.to_string()))?;

                let env = Self::row_to_envelope(
                    env_id,
                    created_at_ms,
                    deleted_flag,
                    tokens_json,
                    payload_json,
                )?;

                // Visibility is decided on the resolved version — filtering in
                // SQL before picking the latest row would resurface an older
                // visible version of a now-restricted envelope.
                if env.deleted || !envelope_visible_to(&env, tokens) {
                    Ok(None)
                } else {
                    Ok(Some(env))
                }
            }
        }
    }

    async fn list(&self, tokens: &[String]) -> Result<Vec<Envelope>> {
        let table = &self.table;
        let sql = format!(
            r#"SELECT e.id, e.created_at_ms, e.deleted, e.authorized_tokens, e.payload
               FROM `{table}` e
               INNER JOIN (
                   SELECT id, MAX(created_at_ms) AS max_ts FROM `{table}` GROUP BY id
               ) m ON e.id = m.id AND e.created_at_ms = m.max_ts
               WHERE e.deleted = 0"#
        );

        let rows = sqlx::query(&sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        for r in rows {
            let env_id: String = r
                .try_get("id")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let created_at_ms: i64 = r
                .try_get("created_at_ms")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let deleted_flag: i8 = r
                .try_get("deleted")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let tokens_json: String = r
                .try_get("authorized_tokens")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let payload_json: String = r
                .try_get("payload")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;

            let env = Self::row_to_envelope(
                env_id,
                created_at_ms,
                deleted_flag,
                tokens_json,
                payload_json,
            )?;
            if envelope_visible_to(&env, tokens) {
                results.push(env);
            }
        }

        Ok(results)
    }

    async fn remove(&self, id: &str, tokens: &[String]) -> Result<bool> {
        let current = self.read(id, tokens, None).await?;
        match current {
            None => Ok(false),
            Some(mut env) => {
                env.deleted = true;
                env.created_at = Utc::now();
                let table = &self.table;
                let sql = format!(
                    "INSERT INTO `{table}` (id, created_at_ms, deleted, authorized_tokens, payload) VALUES (?, ?, ?, ?, ?)"
                );

                let created_at_ms = env.created_at.timestamp_millis();
                let tokens_json = serde_json::to_string(&env.authorized_tokens)
                    .map_err(|e| MeshqlError::Storage(e.to_string()))?;
                let payload_json = serde_json::to_string(&env.payload)
                    .map_err(|e| MeshqlError::Storage(e.to_string()))?;

                sqlx::query(&sql)
                    .bind(&env.id)
                    .bind(created_at_ms)
                    .bind(1i8)
                    .bind(&tokens_json)
                    .bind(&payload_json)
                    .execute(&self.pool)
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
        let mut results = Vec::with_capacity(envelopes.len());
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
    async fn list_versions(&self, id: &str, tokens: &[String]) -> Result<Vec<VersionRef>> {
        let envelopes = self.all_versions(id).await?;
        Ok(envelopes
            .iter()
            .map(|e| {
                if envelope_visible_to(e, tokens) {
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
        tokens: &[String],
    ) -> Result<Option<Envelope>> {
        for env in self.all_versions(id).await? {
            if version_token(&env) != token {
                continue;
            }
            // Unauthorized is not absent: the listing already reported it.
            if !envelope_visible_to(&env, tokens) {
                return Err(MeshqlError::Unauthorized);
            }
            return Ok(Some(env));
        }
        Ok(None)
    }
}
