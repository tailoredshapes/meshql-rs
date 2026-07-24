use async_trait::async_trait;
use handlebars::Handlebars;
use meshql_core::{
    envelope_order, envelope_visible_to, Envelope, MeshqlError, Result, Searcher, Stash,
};
use serde_json::Value;
use std::collections::HashMap;
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

    /// Parse pull-query rows into the visible envelopes, in canonical result
    /// order (`meshql_core::envelope_order`).
    ///
    /// ksqlDB pull queries accept no ORDER BY, and Kafka gives no ordering
    /// across partitions, so the sort has to happen client-side. Rows that fail
    /// to parse are dropped with a warning, as before.
    fn ordered_visible(rows: &[HashMap<String, Value>], creds: &[String]) -> Vec<Envelope> {
        let mut envelopes: Vec<Envelope> = rows
            .iter()
            .filter_map(|row| match row_to_envelope(row) {
                Ok(env) if !env.deleted && envelope_visible_to(&env, creds) => Some(env),
                Ok(_) => None,
                Err(e) => {
                    warn!("Failed to parse row: {}", e);
                    None
                }
            })
            .collect();
        envelopes.sort_by(envelope_order);
        envelopes
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
            // First in canonical result order, not first in whatever order
            // ksqlDB happened to return the rows.
            Ok(rows) => Ok(Self::ordered_visible(&rows, creds)
                .first()
                .map(envelope_to_stash)),
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
                let mut results: Vec<Stash> = Self::ordered_visible(&rows, creds)
                    .iter()
                    .map(envelope_to_stash)
                    .collect();

                // limit applies after ordering and after visibility filtering,
                // so it truncates a meaningful prefix and restricted rows don't
                // consume slots a visible row should fill
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
