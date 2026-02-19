use crate::query::build_where;
use async_trait::async_trait;
use handlebars::Handlebars;
use meshql_core::{MeshqlError, Result, Searcher, Stash};
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
            let id: String = row
                .try_get("id")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let payload_json: String = row
                .try_get("payload")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;

            let mut stash: Stash = serde_json::from_str(&payload_json)
                .map_err(|e| MeshqlError::Parse(e.to_string()))?;
            stash.insert("id".to_string(), json!(id));
            results.push(stash);
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
        creds: &[String],
        at: i64,
    ) -> Result<Option<Stash>> {
        let mut results = self
            .execute_query(template, args, creds, at, Some(1))
            .await?;
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
