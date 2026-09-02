use async_trait::async_trait;
use handlebars::Handlebars;
use meshql_core::{
    envelope_order, Envelope, MeshqlError, Operation, Result, Searcher, Session, Stash,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

use crate::client::ConfluentClient;
use crate::config::KsqlConfig;
use crate::converters::{envelope_to_stash, row_to_envelope};
use crate::query::{build_where, ids_clause, matches_query};

pub struct KsqlSearcher {
    client: Arc<ConfluentClient>,
    table_name: String,
    stream_name: String,
}

impl KsqlSearcher {
    pub fn new(client: Arc<ConfluentClient>, entity: &str) -> Self {
        Self {
            client,
            table_name: KsqlConfig::table_name(entity),
            stream_name: KsqlConfig::stream_name(entity),
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
    fn ordered_visible(rows: &[HashMap<String, Value>], session: &dyn Session) -> Vec<Envelope> {
        let mut envelopes: Vec<Envelope> = rows
            .iter()
            .filter_map(|row| match row_to_envelope(row) {
                Ok(env) if !env.deleted && session.is_authorized(Operation::Read, &env) => {
                    Some(env)
                }
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

    /// Is this a read of the past?
    ///
    /// Any cutoff before the present is, and there is no tolerance for "near
    /// enough to now". A graphlette stamps the originating request time and
    /// carries it down, so an ordinary live query is already a few milliseconds
    /// in the past — and answering it from the materialized table would let a
    /// write that landed after the request started leak into the result, which
    /// is precisely the mosaic that carrying the timestamp exists to prevent.
    /// A tolerance here reintroduced that, and the certification caught it.
    ///
    /// The cost is that nearly every query resolves from the log rather than
    /// the `LATEST_BY_OFFSET` table. That is the honest cost of a table that
    /// keeps one version per id: it can answer "latest", and meshql never asks
    /// for "latest" — it asks for "as of". The table stays for a cutoff at or
    /// after the present, where the two questions coincide.
    fn is_temporal(at_ms: i64) -> bool {
        at_ms < chrono::Utc::now().timestamp_millis()
    }

    /// Every envelope visible at `at_ms`, matching `query_obj`, in canonical
    /// result order.
    ///
    /// The materialized table is a `LATEST_BY_OFFSET` rollup: it holds exactly
    /// one version per id and so can only ever answer "now". `at` used to be
    /// ignored outright here, which made every as-of query silently return the
    /// present. The history is in the stream, so a temporal search reads that.
    ///
    /// Two phases, because the predicate has to apply to the version that
    /// *resolves*, not to any version that ever existed. Phase one pushes the
    /// predicate down to find candidate ids — an id whose resolved version
    /// matches must have at least one matching version, so this narrows without
    /// losing anything. Phase two pulls those ids' full histories, resolves each
    /// at the cutoff, and re-applies the predicate to what it got.
    async fn resolved_at(
        &self,
        query_obj: &serde_json::Map<String, serde_json::Value>,
        session: &dyn Session,
        at_ms: i64,
    ) -> Result<Vec<Envelope>> {
        let where_part = build_where(query_obj);
        let candidates = if where_part.clause.is_empty() {
            format!("SELECT * FROM {};", self.stream_name)
        } else {
            format!(
                "SELECT * FROM {} WHERE {};",
                self.stream_name, where_part.clause
            )
        };

        let rows = match self.client.pull_query_from_earliest(&candidates).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("KsqlSearcher temporal candidate query failed: {}", e);
                return Ok(Vec::new());
            }
        };

        let mut ids: Vec<String> = rows
            .iter()
            .filter_map(|row| row_to_envelope(row).ok().map(|e| e.id))
            .collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        // Resolve in chunks: the id predicate is an OR chain, and a very wide
        // one is worth splitting rather than sending as a single statement.
        let mut resolved: Vec<Envelope> = Vec::new();
        for chunk in ids.chunks(50) {
            let query = format!(
                "SELECT * FROM {} WHERE {};",
                self.stream_name,
                ids_clause(chunk)
            );
            let history = match self.client.pull_query_from_earliest(&query).await {
                Ok(rows) => rows,
                Err(e) => {
                    warn!("KsqlSearcher temporal history query failed: {}", e);
                    continue;
                }
            };

            let mut latest: HashMap<String, Envelope> = HashMap::new();
            for row in &history {
                let env = match row_to_envelope(row) {
                    Ok(env) => env,
                    Err(e) => {
                        warn!("Failed to parse row: {}", e);
                        continue;
                    }
                };
                if env.created_at.timestamp_millis() > at_ms {
                    continue;
                }
                match latest.get(&env.id) {
                    Some(seen) if envelope_order(seen, &env) != std::cmp::Ordering::Less => {}
                    _ => {
                        latest.insert(env.id.clone(), env);
                    }
                }
            }

            resolved.extend(latest.into_values().filter(|env| {
                !env.deleted
                    && matches_query(env, query_obj)
                    && session.is_authorized(Operation::Read, env)
            }));
        }

        resolved.sort_by(envelope_order);
        Ok(resolved)
    }
}

#[async_trait]
impl Searcher for KsqlSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        session: &dyn Session,
        at: i64,
    ) -> Result<Option<Stash>> {
        let query_obj = self.render_template(template, args)?;

        if Self::is_temporal(at) {
            return Ok(self
                .resolved_at(&query_obj, session, at)
                .await?
                .first()
                .map(envelope_to_stash));
        }

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
            Ok(rows) => Ok(Self::ordered_visible(&rows, session)
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
        session: &dyn Session,
        at: i64,
    ) -> Result<Vec<Stash>> {
        let query_obj = self.render_template(template, args)?;

        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .map(|v| v as usize);

        if Self::is_temporal(at) {
            let mut results: Vec<Stash> = self
                .resolved_at(&query_obj, session, at)
                .await?
                .iter()
                .map(envelope_to_stash)
                .collect();
            if let Some(lim) = limit {
                results.truncate(lim);
            }
            return Ok(results);
        }

        let where_part = build_where(&query_obj);

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
                let mut results: Vec<Stash> = Self::ordered_visible(&rows, session)
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
