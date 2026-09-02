use async_trait::async_trait;
use chrono::{DateTime, Utc};
use meshql_core::versions::{version_order, version_token, VersionRef};
use meshql_core::{
    AuthMark, Envelope, MeshqlError, Operation, Repository, Result, Session, SystemSession,
};
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;

pub struct SqliteRepository {
    pub pool: SqlitePool,
}

impl SqliteRepository {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = SqlitePool::connect(database_url)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        Self::init_schema(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn new_with_pool(pool: SqlitePool) -> Result<Self> {
        Self::init_schema(&pool).await?;
        Ok(Self { pool })
    }

    async fn init_schema(pool: &SqlitePool) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS envelopes (
                id TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                deleted INTEGER NOT NULL DEFAULT 0,
                authorized_tokens TEXT NOT NULL,
                payload TEXT NOT NULL
            )",
        )
        .execute(pool)
        .await
        .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_envelopes_id ON envelopes(id)")
            .execute(pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        Ok(())
    }

    fn row_to_envelope(row: &sqlx::sqlite::SqliteRow) -> Result<Envelope> {
        let id: String = row
            .try_get("id")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let created_at_ms: i64 = row
            .try_get("created_at_ms")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let deleted_i: i64 = row
            .try_get("deleted")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let tokens_json: String = row
            .try_get("authorized_tokens")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let payload_json: String = row
            .try_get("payload")
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let created_at = DateTime::from_timestamp_millis(created_at_ms).unwrap_or_default();
        let auth: AuthMark =
            serde_json::from_str(&tokens_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        let payload: meshql_core::Stash =
            serde_json::from_str(&payload_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        Ok(Envelope {
            id,
            payload,
            created_at,
            deleted: deleted_i != 0,
            auth,
        })
    }
}

#[async_trait]
impl Repository for SqliteRepository {
    async fn create(&self, envelope: Envelope, session: &dyn Session) -> Result<Envelope> {
        // The plugin owns the mark. Storage hands it the envelope and
        // persists whatever comes back, verbatim, in the same INSERT as the
        // payload — so authorization can never become a dual write.
        let mut env = session.stamp(envelope);
        if env.id.is_empty() {
            env.id = uuid::Uuid::new_v4().to_string();
        }

        let created_at_ms = env.created_at.timestamp_millis();
        let deleted_i: i64 = if env.deleted { 1 } else { 0 };
        let tokens_json =
            serde_json::to_string(&env.auth).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        let payload_json =
            serde_json::to_string(&env.payload).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        sqlx::query(
            "INSERT INTO envelopes (id, created_at_ms, deleted, authorized_tokens, payload) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&env.id)
        .bind(created_at_ms)
        .bind(deleted_i)
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
        session: &dyn Session,
        at: Option<DateTime<Utc>>,
    ) -> Result<Option<Envelope>> {
        let cutoff_ms = match at {
            Some(t) => t.timestamp_millis(),
            None => Utc::now().timestamp_millis() + 1,
        };

        // Ordering cannot be left to SQL. The tiebreak used to be `rowid DESC`,
        // which lets a storage engine's physical row id decide which version
        // resolves. It has no equivalent on Postgres or Mongo, which is why
        // those two break ties not at all and resolve nondeterministically when
        // two versions share a millisecond. The version token is derived from
        // content, so every adapter can apply the same second key and agree.
        //
        // `id` is indexed, so this reads one document's history rather than the
        // table.
        let rows = sqlx::query(
            "SELECT id, created_at_ms, deleted, authorized_tokens, payload
             FROM envelopes WHERE id = ? AND created_at_ms <= ?",
        )
        .bind(id)
        .bind(cutoff_ms)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let newest = rows
            .iter()
            .map(Self::row_to_envelope)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .max_by(version_order);

        match newest {
            None => Ok(None),
            Some(env) => {
                if env.deleted || !session.is_authorized(Operation::Read, &env) {
                    Ok(None)
                } else {
                    Ok(Some(env))
                }
            }
        }
    }

    async fn list(&self, session: &dyn Session) -> Result<Vec<Envelope>> {
        let rows = sqlx::query(
            "WITH latest AS (
                SELECT id, created_at_ms, deleted, authorized_tokens, payload,
                       ROW_NUMBER() OVER (PARTITION BY id ORDER BY created_at_ms DESC, rowid DESC) AS rn
                FROM envelopes
            )
            SELECT id, created_at_ms, deleted, authorized_tokens, payload
            FROM latest WHERE rn = 1 AND deleted = 0",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            let env = Self::row_to_envelope(&row)?;
            if session.is_authorized(Operation::Read, &env) {
                results.push(env);
            }
        }
        Ok(results)
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
                // A tombstone keeps the mark of the record it buries, so the
                // change feed can still say who is entitled to hear about the
                // deletion. Re-stamping it would answer for the plugin.
                self.create(deleted_env, &SystemSession).await?;
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
            let deleted = self.remove(id, session).await?;
            results.insert(id.clone(), deleted);
        }
        Ok(results)
    }
    /// Every version of one document, oldest first.
    ///
    /// Ordering does not use `rowid`. That is what `read` does today, and it is
    /// a storage engine's physical row id leaking into a resolution rule — it
    /// cannot port to Postgres or Mongo, which is why those two break ties not
    /// at all. Sorting in memory by `version_order` gives every adapter the
    /// same answer.
    async fn list_versions(&self, id: &str, session: &dyn Session) -> Result<Vec<VersionRef>> {
        let rows = sqlx::query(
            "SELECT id, created_at_ms, deleted, authorized_tokens, payload
             FROM envelopes WHERE id = ?",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut envelopes: Vec<Envelope> = rows
            .iter()
            .map(Self::row_to_envelope)
            .collect::<Result<Vec<_>>>()?;
        envelopes.sort_by(version_order);

        Ok(envelopes
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
        let rows = sqlx::query(
            "SELECT id, created_at_ms, deleted, authorized_tokens, payload
             FROM envelopes WHERE id = ?",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        for row in &rows {
            let env = Self::row_to_envelope(row)?;
            if version_token(&env) != token {
                continue;
            }
            // Unauthorized is not the same as absent: the listing already told
            // the caller this version exists.
            if !session.is_authorized(Operation::Read, &env) {
                return Err(MeshqlError::Unauthorized);
            }
            return Ok(Some(env));
        }
        Ok(None)
    }
}
