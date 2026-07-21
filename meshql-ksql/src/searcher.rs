use async_trait::async_trait;
use handlebars::Handlebars;
use meshql_core::{envelope_visible_to, MeshqlError, Result, Searcher, Stash};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::client::ConfluentClient;
use crate::config::KsqlConfig;
use crate::converters::{envelope_to_stash, row_to_envelope};
use crate::query::build_where;

pub struct KsqlSearcher {
    client: Arc<ConfluentClient>,
    table_name: String,
}

impl KsqlSearcher {
    pub fn new(client: Arc<ConfluentClient>, entity: &str) -> Self {
        Self {
            client,
            table_name: KsqlConfig::table_name(entity),
        }
    }

    /// Render a Handlebars template with the given args, then parse as JSON query object.
    fn render_template(
        &self,
        template: &str,
        args: &Stash,
    ) -> Result<serde_json::Map<String, serde_json::Value>> {
        let mut hbs = Handlebars::new();
        hbs.set_strict_mode(false);
        let rendered = hbs
            .render_template(template, args)
            .map_err(|e| MeshqlError::Template(e.to_string()))?;
        let query_obj: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&rendered).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        Ok(query_obj)
    }
}

#[async_trait]
impl Searcher for KsqlSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        _at: i64,
    ) -> Result<Option<Stash>> {
        let query_obj = self.render_template(template, args)?;
        let where_part = build_where(&query_obj);

        // authorized_tokens is a double-encoded JSON string in the row, so
        // visibility can't be expressed in the ksqlDB WHERE clause. Fetch all
        // matches (no LIMIT 1: a restricted row must not shadow a visible
        // match) and post-filter.
        let query = if where_part.clause.is_empty() {
            format!("SELECT * FROM {} WHERE deleted = false;", self.table_name)
        } else {
            format!(
                "SELECT * FROM {} WHERE {} AND deleted = false;",
                self.table_name, where_part.clause
            )
        };

        debug!("KsqlSearcher.find() - Query: {}", query);

        match self.client.pull_query(&query).await {
            Ok(rows) => Ok(rows.iter().find_map(|row| match row_to_envelope(row) {
                Ok(env) if !env.deleted && envelope_visible_to(&env, creds) => {
                    Some(envelope_to_stash(&env))
                }
                Ok(_) => None,
                Err(e) => {
                    warn!("Failed to parse row: {}", e);
                    None
                }
            })),
            Err(e) => {
                warn!("KsqlSearcher.find() query failed: {}", e);
                Ok(None)
            }
        }
    }

    async fn find_all(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        _at: i64,
    ) -> Result<Vec<Stash>> {
        let query_obj = self.render_template(template, args)?;
        let where_part = build_where(&query_obj);

        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .map(|v| v as usize);

        let query = if where_part.clause.is_empty() {
            format!("SELECT * FROM {} WHERE deleted = false;", self.table_name)
        } else {
            format!(
                "SELECT * FROM {} WHERE {} AND deleted = false;",
                self.table_name, where_part.clause
            )
        };

        debug!("KsqlSearcher.find_all() - Query: {}", query);

        match self.client.pull_query(&query).await {
            Ok(rows) => {
                let mut results: Vec<Stash> = rows
                    .iter()
                    .filter_map(|row| match row_to_envelope(row) {
                        Ok(env) if !env.deleted && envelope_visible_to(&env, creds) => {
                            Some(envelope_to_stash(&env))
                        }
                        Ok(_) => None,
                        Err(e) => {
                            warn!("Failed to parse row: {}", e);
                            None
                        }
                    })
                    .collect();

                // limit applies after visibility filtering, so restricted rows
                // don't consume slots a visible row should fill
                if let Some(lim) = limit {
                    results.truncate(lim);
                }

                Ok(results)
            }
            Err(e) => {
                warn!("KsqlSearcher.find_all() query failed: {}", e);
                Ok(Vec::new())
            }
        }
    }
}
