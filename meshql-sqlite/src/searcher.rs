use crate::query::build_where;
use async_trait::async_trait;
use handlebars::Handlebars;
use meshql_core::{Envelope, MeshqlError, Result, Searcher, Stash};
use serde_json::json;
use sqlx::{Row, SqlitePool};

pub struct SqliteSearcher {
    pool: SqlitePool,
    handlebars: Handlebars<'static>,
}

impl SqliteSearcher {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = SqlitePool::connect(database_url)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        Self::init_schema(&pool).await?;
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(false);
        Ok(Self { pool, handlebars })
    }

    pub async fn new_with_pool(pool: SqlitePool) -> Result<Self> {
        Self::init_schema(&pool).await?;
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(false);
        Ok(Self { pool, handlebars })
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

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_envelopes_id ON envelopes(id)",
        )
        .execute(pool)
        .await
        .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        Ok(())
    }

    fn render_template(&self, template: &str, args: &Stash) -> Result<String> {
        self.handlebars
            .render_template(template, &serde_json::Value::Object(args.clone()))
            .map_err(|e| MeshqlError::Template(e.to_string()))
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

        let created_at =
            chrono::DateTime::from_timestamp_millis(created_at_ms).unwrap_or_default();
        let authorized_tokens: Vec<String> =
            serde_json::from_str(&tokens_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        let payload: meshql_core::Stash =
            serde_json::from_str(&payload_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        Ok(Envelope {
            id,
            payload,
            created_at,
            deleted: deleted_i != 0,
            authorized_tokens,
        })
    }

    async fn execute_query(
        &self,
        template: &str,
        args: &Stash,
        _creds: &[String],
        at: i64,
        limit: Option<i64>,
    ) -> Result<Vec<Stash>> {
        let query_json = self.render_template(template, args)?;

        let query_val: serde_json::Value =
            serde_json::from_str(&query_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        let query_obj = query_val
            .as_object()
            .ok_or_else(|| MeshqlError::Parse("Query template must produce a JSON object".to_string()))?;

        let where_part = build_where(query_obj);

        let cutoff_ms = at + 1;

        let base_sql = "
WITH latest AS (
    SELECT id, created_at_ms, deleted, authorized_tokens, payload,
           ROW_NUMBER() OVER (PARTITION BY id ORDER BY created_at_ms DESC, rowid DESC) AS rn
    FROM envelopes WHERE created_at_ms <= ?
)
SELECT id, created_at_ms, deleted, authorized_tokens, payload
FROM latest WHERE rn = 1 AND deleted = 0"
            .to_string();

        let sql = if where_part.clause.is_empty() {
            if let Some(lim) = limit {
                format!("{} LIMIT {}", base_sql, lim)
            } else {
                base_sql
            }
        } else {
            let with_where = format!("{} AND {}", base_sql, where_part.clause);
            if let Some(lim) = limit {
                format!("{} LIMIT {}", with_where, lim)
            } else {
                with_where
            }
        };

        let mut q = sqlx::query(&sql).bind(cutoff_ms);
        for val in &where_part.values {
            q = q.bind(val);
        }

        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            let env = Self::row_to_envelope(&row)?;
            let mut stash = env.payload.clone();
            stash.insert("id".to_string(), json!(env.id));
            results.push(stash);
        }

        Ok(results)
    }
}

#[async_trait]
impl Searcher for SqliteSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Option<Stash>> {
        let mut results = self.execute_query(template, args, creds, at, Some(1)).await?;
        Ok(results.pop())
    }

    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Vec<Stash>> {
        let limit = args.get("limit").and_then(|v| v.as_i64());
        self.execute_query(template, args, creds, at, limit).await
    }
}
