use async_trait::async_trait;
use chrono::{DateTime, Utc};
use meshql_core::versions::{version_order, version_token, VersionRef};
use meshql_core::{
    AuthMark, Envelope, MeshqlError, Operation, Repository, Result, Session, SystemSession,
};
use sqlx::{PgPool, Row};
use std::collections::HashMap;

/// Columns written per envelope row by `create` and `create_many`.
///
/// `MAX_ROWS_PER_INSERT` is derived from this rather than guessed, so adding a
/// column to the envelope table re-chunks the batch insert automatically
/// instead of silently pushing it past the bind-parameter ceiling.
const INSERT_COLUMNS: usize = 5;

/// PostgreSQL's extended query protocol carries the parameter count as an
/// unsigned 16-bit integer, so one statement can bind at most 65535 values.
/// Exceeding it fails the whole statement.
const MAX_BIND_PARAMS: usize = 65535;

/// Rows a single multi-row `INSERT` may carry before it must be split.
///
/// Public so tests can size a batch that provably crosses the boundary from
/// the same constant the implementation uses — a hard-coded number in a test
/// stops proving anything the moment the column count changes.
pub const MAX_ROWS_PER_INSERT: usize = MAX_BIND_PARAMS / INSERT_COLUMNS;

pub struct PostgresRepository {
    pub pool: PgPool,
    pub table: String,
}

/// An envelope with its columns already rendered.
///
/// `create_many` assigns and serializes every envelope before it writes any of
/// them, so the rows it binds and the envelopes it hands back cannot drift
/// apart, and a serialization failure aborts before touching the database.
struct PreparedRow {
    envelope: Envelope,
    created_at_ms: i64,
    tokens_json: String,
    payload_json: String,
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

        let created_at = DateTime::from_timestamp_millis(created_at_ms).unwrap_or_default();
        let auth: AuthMark =
            serde_json::from_str(&tokens_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        let payload: meshql_core::Stash =
            serde_json::from_str(&payload_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        Ok(Envelope {
            id,
            payload,
            created_at,
            deleted,
            auth,
        })
    }
}

#[async_trait]
impl Repository for PostgresRepository {
    async fn create(&self, envelope: Envelope, session: &dyn Session) -> Result<Envelope> {
        // The plugin owns the mark. Storage hands it the envelope and persists
        // whatever comes back, verbatim, in the same INSERT as the payload — so
        // authorization can never become a dual write.
        let mut env = session.stamp(envelope);
        if env.id.is_empty() {
            env.id = uuid::Uuid::new_v4().to_string();
        }

        let created_at_ms = env.created_at.timestamp_millis();
        let tokens_json =
            serde_json::to_string(&env.auth).map_err(|e| MeshqlError::Parse(e.to_string()))?;
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
        session: &dyn Session,
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
                // Visibility is decided on the resolved version — filtering in
                // SQL before picking the latest row would resurface an older
                // visible version of a now-restricted envelope.
                if env.deleted || !session.is_authorized(Operation::Read, &env) {
                    Ok(None)
                } else {
                    Ok(Some(env))
                }
            }
        }
    }

    async fn list(&self, session: &dyn Session) -> Result<Vec<Envelope>> {
        // `remove` appends a tombstone version, so the latest row per id has to
        // be resolved *before* deleted rows are dropped — filtering first would
        // discard the tombstone and resurface the previous, non-deleted version.
        let sql = format!(
            "SELECT id, created_at_ms, deleted, authorized_tokens, payload
             FROM (
                 SELECT DISTINCT ON (id) id, created_at_ms, deleted, authorized_tokens, payload
                 FROM {}
                 ORDER BY id, created_at_ms DESC
             ) latest
             WHERE deleted = FALSE",
            self.table
        );
        let rows = sqlx::query(&sql)
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
                // change feed can still say who was entitled to hear about
                // the deletion. Re-stamping it would answer for the plugin.
                self.create(deleted_env, &SystemSession).await?;
                Ok(true)
            }
        }
    }

    /// Writes the whole batch with one multi-row `INSERT` per chunk instead of
    /// one round trip per envelope. A bulk load of millions of rows over a
    /// network pays for those round trips and nothing else.
    ///
    /// Deliberately not wrapped in a transaction. The caller's ordering rule is
    /// append-then-commit-the-position, so a partial batch replays in full: a
    /// duplicate row is cheap, a gap is permanent. Atomicity would buy nothing
    /// and would hold a write transaction open across a multi-megabyte load.
    /// What the caller *does* need, and what this guarantees, is that a failure
    /// surfaces as `Err` — never as a short success.
    async fn create_many(
        &self,
        envelopes: Vec<Envelope>,
        session: &dyn Session,
    ) -> Result<Vec<Envelope>> {
        // `push_values` with no rows emits `INSERT INTO t (..) VALUES` and
        // nothing else — a syntax error. An empty batch is a no-op.
        if envelopes.is_empty() {
            return Ok(Vec::new());
        }

        // Same assignments `create` makes, in the same order: the plugin's
        // stamp, then a generated id for an empty one.
        let mut prepared: Vec<PreparedRow> = Vec::with_capacity(envelopes.len());
        for env in envelopes {
            let mut env = session.stamp(env);
            if env.id.is_empty() {
                env.id = uuid::Uuid::new_v4().to_string();
            }

            let created_at_ms = env.created_at.timestamp_millis();
            let tokens_json =
                serde_json::to_string(&env.auth).map_err(|e| MeshqlError::Parse(e.to_string()))?;
            let payload_json = serde_json::to_string(&env.payload)
                .map_err(|e| MeshqlError::Parse(e.to_string()))?;

            prepared.push(PreparedRow {
                envelope: env,
                created_at_ms,
                tokens_json,
                payload_json,
            });
        }

        for chunk in prepared.chunks(MAX_ROWS_PER_INSERT) {
            let mut builder = sqlx::QueryBuilder::new(format!(
                "INSERT INTO {} (id, created_at_ms, deleted, authorized_tokens, payload) ",
                self.table
            ));
            builder.push_values(chunk, |mut row, prepared: &PreparedRow| {
                row.push_bind(prepared.envelope.id.as_str())
                    .push_bind(prepared.created_at_ms)
                    .push_bind(prepared.envelope.deleted)
                    .push_bind(prepared.tokens_json.as_str())
                    .push_bind(prepared.payload_json.as_str());
            });
            builder
                .build()
                .execute(&self.pool)
                .await
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        }

        // Input order, with the assignments the rows were written with.
        Ok(prepared.into_iter().map(|p| p.envelope).collect())
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
    async fn list_versions(&self, id: &str, session: &dyn Session) -> Result<Vec<VersionRef>> {
        let sql = format!(
            "SELECT id, created_at_ms, deleted, authorized_tokens, payload
             FROM {} WHERE id = $1",
            self.table
        );
        let rows = sqlx::query(&sql)
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
        let sql = format!(
            "SELECT id, created_at_ms, deleted, authorized_tokens, payload
             FROM {} WHERE id = $1",
            self.table
        );
        let rows = sqlx::query(&sql)
            .bind(id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        for row in &rows {
            let env = Self::row_to_envelope(row)?;
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
