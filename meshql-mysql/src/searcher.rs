use crate::query::build_where;
use async_trait::async_trait;
use handlebars::Handlebars;
use meshql_core::{AuthMark, Envelope, MeshqlError, Operation, Result, Searcher, Session, Stash};
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
        session: &dyn Session,
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

        // Authorization is fetch-then-ask: the mark is opaque, so it cannot
        // become a WHERE clause, and the limit is applied to the *authorized*
        // rows further down. Nothing is pushed into SQL — the caller's limit
        // must never be consumed by a row the caller is not entitled to see.

        // Canonical result ordering (meshql_core::envelope_order): the resolved
        // version's created_at, then the envelope id, so the limit applied after
        // authorization truncates a meaningful prefix. `COLLATE utf8mb4_bin`
        // forces byte ordering — MySQL's default collation is case-insensitive
        // and would disagree with the other adapters.
        let sql = format!(
            r#"WITH latest AS (
                SELECT id, created_at_ms, deleted, authorized_tokens, payload,
                       ROW_NUMBER() OVER (PARTITION BY id ORDER BY created_at_ms DESC) AS rn
                FROM `{table}` WHERE created_at_ms <= ?
            )
            SELECT id, created_at_ms, deleted, authorized_tokens, payload
            FROM latest WHERE rn = 1 AND deleted = 0
            {dynamic_where}
            ORDER BY created_at_ms ASC, id COLLATE utf8mb4_bin ASC"#
        );

        let mut q = sqlx::query(&sql).bind(at);
        for val in &where_part.values {
            q = q.bind(val.as_str());
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

            let auth: AuthMark = serde_json::from_str(&tokens_json)
                .map_err(|e| MeshqlError::Parse(e.to_string()))?;
            let mut stash: Stash = serde_json::from_str(&payload_json)
                .map_err(|e| MeshqlError::Parse(e.to_string()))?;

            // The plugin is asked about the whole envelope, not about the mark:
            // a plugin that authorizes on a payload field has nothing to say
            // about a bare token list. `deleted` is false by construction — the
            // query already resolved to the latest undeleted version.
            let created_at =
                chrono::DateTime::from_timestamp_millis(created_at_ms).unwrap_or_default();
            let envelope = Envelope {
                id: env_id.clone(),
                payload: stash.clone(),
                created_at,
                deleted: false,
                auth,
            };
            if !session.is_authorized(Operation::Read, &envelope) {
                continue;
            }

            // Merge id and createdAt into the stash so callers can find by id
            // field and GraphQL schemas can opt into an as-of timestamp.
            stash.insert("id".to_string(), serde_json::Value::String(env_id));
            stash.insert(
                "createdAt".to_string(),
                serde_json::Value::String(created_at.to_rfc3339()),
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
        session: &dyn Session,
        at: i64,
    ) -> Result<Option<Stash>> {
        let query_json = self.render_template(template, args)?;
        let results = self
            .execute_query(&query_json, session, at, Some(1))
            .await?;
        Ok(results.into_iter().next())
    }

    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Vec<Stash>> {
        let query_json = self.render_template(template, args)?;
        let limit = args.get("limit").and_then(|v| v.as_i64());
        self.execute_query(&query_json, session, at, limit).await
    }
}
