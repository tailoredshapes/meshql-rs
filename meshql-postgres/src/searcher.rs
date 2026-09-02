use crate::query::build_where;
use async_trait::async_trait;
use handlebars::Handlebars;
use meshql_core::{AuthMark, Envelope, MeshqlError, Operation, Result, Searcher, Session, Stash};
use serde_json::json;
use sqlx::{PgPool, Row};

pub struct PostgresSearcher {
    pool: PgPool,
    handlebars: Handlebars<'static>,
    table: String,
}

impl PostgresSearcher {
    /// Create a new searcher using the default table name `envelopes`.
    pub async fn new(database_url: &str) -> Result<Self> {
        Self::new_with_table(database_url, "envelopes").await
    }

    /// Create a new searcher with a custom table name (useful for test isolation).
    pub async fn new_with_table(database_url: &str, table: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(false);
        Ok(Self {
            pool,
            handlebars,
            table: table.to_string(),
        })
    }

    fn render_template(&self, template: &str, args: &Stash) -> Result<String> {
        self.handlebars
            .render_template(template, &serde_json::Value::Object(args.clone()))
            .map_err(|e| MeshqlError::Template(e.to_string()))
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

        let created_at = chrono::DateTime::from_timestamp_millis(created_at_ms).unwrap_or_default();
        let auth: AuthMark =
            serde_json::from_str(&tokens_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        let payload: Stash =
            serde_json::from_str(&payload_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        Ok(Envelope {
            id,
            payload,
            created_at,
            deleted,
            auth,
        })
    }

    async fn execute_query(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
        limit: Option<i64>,
    ) -> Result<Vec<Stash>> {
        let query_json = self.render_template(template, args)?;

        let query_val: serde_json::Value =
            serde_json::from_str(&query_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        let query_obj = query_val.as_object().ok_or_else(|| {
            MeshqlError::Parse("Query template must produce a JSON object".to_string())
        })?;

        // $1 = cutoff_ms, dynamic params start at $2
        let where_part = build_where(query_obj, 2);

        let cutoff_ms = at + 1;

        let base_sql = format!(
            "WITH latest AS (
    SELECT id, created_at_ms, deleted, authorized_tokens, payload,
           ROW_NUMBER() OVER (PARTITION BY id ORDER BY created_at_ms DESC) AS rn
    FROM {} WHERE created_at_ms <= $1
)
SELECT id, created_at_ms, deleted, authorized_tokens, payload
FROM latest WHERE rn = 1 AND deleted = FALSE",
            self.table
        );

        // Authorization is fetch-then-ask: the mark is opaque, so it cannot
        // become a WHERE clause, and the limit is applied to the *authorized*
        // rows further down. Nothing is pushed into SQL — the caller's limit
        // must never be consumed by a row the caller is not entitled to see.

        let mut sql = if where_part.clause.is_empty() {
            base_sql
        } else {
            format!("{} AND {}", base_sql, where_part.clause)
        };

        // Canonical result ordering (meshql_core::envelope_order): the resolved
        // version's created_at, then the envelope id, so the limit applied after
        // authorization truncates a meaningful prefix. `COLLATE "C"` forces byte
        // ordering — the database's default collation is locale-dependent and
        // would disagree with the other adapters.
        sql.push_str(" ORDER BY created_at_ms ASC, id COLLATE \"C\" ASC");

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
            if !session.is_authorized(Operation::Read, &env) {
                continue;
            }
            let mut stash = env.payload;
            stash.insert("id".to_string(), json!(env.id));
            stash.insert("createdAt".to_string(), json!(env.created_at.to_rfc3339()));
            results.push(stash);
        }

        if let Some(lim) = limit {
            results.truncate(lim.max(0) as usize);
        }

        Ok(results)
    }
}

#[async_trait]
impl Searcher for PostgresSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Option<Stash>> {
        let mut results = self
            .execute_query(template, args, session, at, Some(1))
            .await?;
        Ok(results.pop())
    }

    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Vec<Stash>> {
        let limit = args.get("limit").and_then(|v| v.as_i64());
        self.execute_query(template, args, session, at, limit).await
    }
}
