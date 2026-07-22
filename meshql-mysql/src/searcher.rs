use crate::query::build_where;
use async_trait::async_trait;
use handlebars::Handlebars;
use meshql_core::{MeshqlError, Result, Searcher, Stash};
use sqlx::MySqlPool;
use sqlx::Row;

pub struct MysqlSearcher {
    pool: MySqlPool,
    table: String,
    handlebars: Handlebars<'static>,
}

impl MysqlSearcher {
    pub async fn new(database_url: &str) -> Result<Self> {
        Self::new_with_table(database_url, "envelopes").await
    }

    pub async fn new_with_table(database_url: &str, table: &str) -> Result<Self> {
        let pool = MySqlPool::connect(database_url)
            .await
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(false);

        Ok(Self {
            pool,
            table: table.to_string(),
            handlebars,
        })
    }

    fn render_template(&self, template: &str, args: &Stash) -> Result<String> {
        self.handlebars
            .render_template(template, &serde_json::Value::Object(args.clone()))
            .map_err(|e| MeshqlError::Template(e.to_string()))
    }

    async fn execute_query(
        &self,
        query_json: &str,
        creds: &[String],
        at: i64,
        limit: Option<i64>,
    ) -> Result<Vec<Stash>> {
        let json_val: serde_json::Value =
            serde_json::from_str(query_json).map_err(|e| MeshqlError::Parse(e.to_string()))?;

        let obj = json_val
            .as_object()
            .ok_or_else(|| MeshqlError::Parse("Query must be a JSON object".to_string()))?;

        let where_part = build_where(obj);
        let table = &self.table;

        let dynamic_where = if where_part.clause.is_empty() {
            String::new()
        } else {
            format!("AND {}", where_part.clause)
        };

        // Visibility filtering happens in Rust after the fetch, so LIMIT can
        // only be pushed into SQL for a "*" caller (who sees every row);
        // otherwise an invisible row could consume the limit and shadow a
        // visible match.
        let wildcard = creds.iter().any(|t| t == "*");
        let sql_limit = if wildcard { limit } else { None };

        let limit_clause = if sql_limit.is_some() {
            "LIMIT ?".to_string()
        } else {
            String::new()
        };

        let sql = format!(
            r#"WITH latest AS (
                SELECT id, created_at_ms, deleted, authorized_tokens, payload,
                       ROW_NUMBER() OVER (PARTITION BY id ORDER BY created_at_ms DESC) AS rn
                FROM `{table}` WHERE created_at_ms <= ?
            )
            SELECT id, created_at_ms, deleted, authorized_tokens, payload
            FROM latest WHERE rn = 1 AND deleted = 0
            {dynamic_where}
            {limit_clause}"#
        );

        let mut q = sqlx::query(&sql).bind(at);
        for val in &where_part.values {
            q = q.bind(val.as_str());
        }
        if let Some(lim) = sql_limit {
            q = q.bind(lim);
        }

        let rows = q
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
            let tokens_json: String = r
                .try_get("authorized_tokens")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;
            let payload_json: String = r
                .try_get("payload")
                .map_err(|e| MeshqlError::Storage(e.to_string()))?;

            let authorized_tokens: Vec<String> = serde_json::from_str(&tokens_json)
                .map_err(|e| MeshqlError::Parse(e.to_string()))?;
            if !meshql_core::tokens_visible_to(&authorized_tokens, creds) {
                continue;
            }

            let mut stash: Stash = serde_json::from_str(&payload_json)
                .map_err(|e| MeshqlError::Parse(e.to_string()))?;

            // Merge id and createdAt into the stash so callers can find by id
            // field and GraphQL schemas can opt into an as-of timestamp.
            stash.insert("id".to_string(), serde_json::Value::String(env_id));
            let created_at = chrono::DateTime::from_timestamp_millis(created_at_ms)
                .unwrap_or_default()
                .to_rfc3339();
            stash.insert(
                "createdAt".to_string(),
                serde_json::Value::String(created_at),
            );

            results.push(stash);
        }

        if let Some(lim) = limit {
            results.truncate(lim.max(0) as usize);
        }

        Ok(results)
    }
}

#[async_trait]
impl Searcher for MysqlSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Option<Stash>> {
        let query_json = self.render_template(template, args)?;
        let results = self.execute_query(&query_json, creds, at, Some(1)).await?;
        Ok(results.into_iter().next())
    }

    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Vec<Stash>> {
        let query_json = self.render_template(template, args)?;
        let limit = args.get("limit").and_then(|v| v.as_i64());
        self.execute_query(&query_json, creds, at, limit).await
    }
}
