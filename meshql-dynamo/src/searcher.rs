//! `Searcher` over DynamoDB.
//!
//! # Order of operations
//!
//! The one thing that is easy to get wrong and expensive to get wrong:
//!
//! ```text
//! resolve latest version per id at-or-before the cutoff
//!   → drop tombstones
//!   → match the predicate
//!   → filter by tokens
//!   → order (meshql_core::envelope_order)
//!   → limit
//! ```
//!
//! Pushing the payload predicate into a DynamoDB `FilterExpression` and taking
//! whatever comes back as "the record" resolves the *wrong version* whenever an
//! older version matches the predicate and the current one does not — a story
//! whose `visibility` changed, say. That is what
//! `test_searcher_auth_latest_version_controls_visibility` and
//! `test_searcher_ordering_uses_resolved_version_position` exist to catch, and
//! it is why version resolution happens in [`crate::store::scan_latest`] with
//! `sk < :hi` as the *only* pushed-down condition.
//!
//! Similarly, the limit is applied last, after visibility: an invisible envelope
//! must never consume a limit slot and shadow a visible match
//! (`test_searcher_auth_restricted_caller_sees_only_intersecting`).
//!
//! # The one safe pushdown
//!
//! If the rendered template carries an `"id"` key, the single id is resolved
//! with the same `query` the repository's `read` uses instead of scanning the
//! table, and the remaining conditions are applied to the result. `pk` is the
//! hash key, so the query returns the true latest version — the semantics are
//! identical, only the cost changes. Nothing else is pushed down: an adapter
//! with capabilities the others lack invites templates that only run on one
//! backend.

use async_trait::async_trait;
use aws_sdk_dynamodb::Client;
use handlebars::Handlebars;
use meshql_core::{Envelope, MeshqlError, Result, Searcher, Stash};
use serde_json::{json, Value};

use crate::{matcher, store};

pub struct DynamoSearcher {
    client: Client,
    table: String,
    handlebars: Handlebars<'static>,
}

impl DynamoSearcher {
    /// `endpoint: None` → real AWS from the ambient config. `endpoint:
    /// Some(url)` → DynamoDB Local (see [`store::make_client`]).
    pub async fn new(endpoint: Option<&str>, table: &str) -> Result<Self> {
        let client = store::make_client(endpoint).await;
        Self::new_with_client(client, table).await
    }

    /// Share one client (and one table) with a [`crate::DynamoRepository`].
    pub async fn new_with_client(client: Client, table: &str) -> Result<Self> {
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(false);
        let searcher = Self {
            client,
            table: table.to_string(),
            handlebars,
        };
        searcher.ensure_table().await?;
        Ok(searcher)
    }

    pub async fn ensure_table(&self) -> Result<()> {
        store::ensure_table(&self.client, &self.table).await
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    fn render_template(&self, template: &str, args: &Stash) -> Result<Value> {
        let rendered = self
            .handlebars
            .render_template(template, &Value::Object(args.clone()))
            .map_err(|e| MeshqlError::Template(e.to_string()))?;
        serde_json::from_str(&rendered).map_err(|e| MeshqlError::Parse(e.to_string()))
    }

    async fn execute_query(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
        limit: Option<i64>,
    ) -> Result<Vec<Stash>> {
        let query = self.render_template(template, args)?;
        if query.as_object().is_none() {
            return Err(MeshqlError::Parse(
                "Query template must produce a JSON object".to_string(),
            ));
        }

        let cutoff = store::cutoff_nanos_from_millis(at);

        let candidates = match pushdown_id(&query) {
            Some(Pushdown::Id(id)) => {
                match store::query_latest(&self.client, &self.table, &id, cutoff).await? {
                    Some(env) if !env.deleted => vec![env],
                    _ => Vec::new(),
                }
            }
            // An `"id"` condition whose value is not a string can never equal a
            // record's id, so there is nothing to fetch.
            Some(Pushdown::Impossible) => Vec::new(),
            None => store::scan_latest(&self.client, &self.table, cutoff).await?,
        };

        Ok(select(candidates, &query, creds, limit))
    }
}

enum Pushdown {
    Id(String),
    Impossible,
}

/// Spot the one condition that can be answered by a key `query` instead of a
/// table `scan`.
fn pushdown_id(query: &Value) -> Option<Pushdown> {
    match query.as_object()?.get("id")? {
        Value::String(s) => Some(Pushdown::Id(s.clone())),
        _ => Some(Pushdown::Impossible),
    }
}

/// Match, filter, order and truncate — the whole predicate half of a search,
/// with no I/O in it.
///
/// `candidates` must already be the resolved-latest, tombstone-free set in
/// canonical order (which is what [`store::scan_latest`] returns).
///
/// Kept pure and separate so the fail-empty guard on an unrecognised template
/// key is testable without a DynamoDB.
pub(crate) fn select(
    candidates: Vec<Envelope>,
    query: &Value,
    creds: &[String],
    limit: Option<i64>,
) -> Vec<Stash> {
    let mut matched: Vec<Envelope> = candidates
        .into_iter()
        .filter(|env| matcher::matches(&matcher::record_json(env), query))
        .filter(|env| meshql_core::envelope_visible_to(env, creds))
        .collect();

    // The pushdown path returns a single envelope, so it is trivially ordered;
    // the scan path arrives ordered. Sorting again is cheap and means the
    // canonical order is a property of this function rather than of its callers.
    matched.sort_by(meshql_core::envelope_order);

    let mut results: Vec<Stash> = matched.iter().map(envelope_to_stash).collect();

    // Last, after visibility filtering.
    if let Some(lim) = limit {
        results.truncate(lim.max(0) as usize);
    }
    results
}

/// A result is the resolved envelope's payload with `id` and `createdAt` merged
/// in — additions, not a replacement. `deleted` is never exposed: results
/// already exclude tombstoned and superseded versions.
fn envelope_to_stash(env: &Envelope) -> Stash {
    let mut stash = env.payload.clone();
    stash.insert("id".to_string(), json!(env.id));
    stash.insert("createdAt".to_string(), json!(env.created_at.to_rfc3339()));
    stash
}

#[async_trait]
impl Searcher for DynamoSearcher {
    async fn find(
        &self,
        template: &str,
        args: &Stash,
        creds: &[String],
        at: i64,
    ) -> Result<Option<Stash>> {
        let results = self
            .execute_query(template, args, creds, at, Some(1))
            .await?;
        Ok(results.into_iter().next())
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn at(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(ms).unwrap()
    }

    fn env(id: &str, kind: &str, ms: i64, tokens: &[&str]) -> Envelope {
        let mut payload = Stash::new();
        payload.insert("kind".to_string(), json!(kind));
        payload.insert("name".to_string(), json!(format!("{id}-name")));
        Envelope {
            id: id.to_string(),
            payload,
            created_at: at(ms),
            deleted: false,
            authorized_tokens: tokens.iter().map(|t| t.to_string()).collect(),
        }
    }

    fn corpus() -> Vec<Envelope> {
        vec![
            env("a", "tool", 1_000, &["*"]),
            env("b", "tool", 2_000, &["*"]),
            env("c", "widget", 3_000, &["*"]),
        ]
    }

    fn star() -> Vec<String> {
        vec!["*".to_string()]
    }

    fn ids(results: &[Stash]) -> Vec<String> {
        results
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// The guard `sociallymeshy/docs/architecture.md` §2.3 asks for: a payload
    /// field written bare, without the `payload.` prefix, must return **zero**
    /// results — not every record, which is how the SQL adapters fail.
    #[test]
    fn a_bare_payload_key_returns_nothing_not_everything() {
        let bare = select(corpus(), &json!({"kind": "tool"}), &star(), None);
        assert!(
            bare.is_empty(),
            "an unrecognised template key must fail empty, like merkql — not wide, \
             like the SQL adapters. Got {:?}",
            ids(&bare)
        );

        // ...and the correctly-prefixed form does work, so the test above is
        // about the prefix and not about the data.
        let prefixed = select(corpus(), &json!({"payload.kind": "tool"}), &star(), None);
        assert_eq!(ids(&prefixed), vec!["a", "b"]);
    }

    #[test]
    fn one_bad_condition_poisons_the_whole_query() {
        // Not "the bad condition is skipped and the good one applies" — the
        // whole query returns nothing. That is the merkql semantic.
        let results = select(
            corpus(),
            &json!({"payload.kind": "tool", "kind": "tool"}),
            &star(),
            None,
        );
        assert!(results.is_empty(), "got {:?}", ids(&results));
    }

    #[test]
    fn empty_query_matches_everything_in_canonical_order() {
        let results = select(corpus(), &json!({}), &star(), None);
        assert_eq!(ids(&results), vec!["a", "b", "c"]);
    }

    #[test]
    fn results_carry_id_and_created_at_alongside_the_payload() {
        let results = select(corpus(), &json!({"id": "a"}), &star(), None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["id"], json!("a"));
        assert_eq!(results[0]["createdAt"], json!(at(1_000).to_rfc3339()));
        assert_eq!(results[0]["kind"], json!("tool"));
        assert!(
            !results[0].contains_key("deleted"),
            "`deleted` must never be exposed in a result"
        );
    }

    #[test]
    fn the_limit_is_applied_after_visibility_filtering() {
        // `invisible` sorts first and matches the predicate, but alice cannot see
        // it. A limit applied before the visibility filter would return nothing.
        let candidates = vec![
            env("invisible", "tool", 1_000, &["bob"]),
            env("visible", "tool", 2_000, &["alice"]),
        ];
        let results = select(
            candidates,
            &json!({"payload.kind": "tool"}),
            &["alice".to_string()],
            Some(1),
        );
        assert_eq!(ids(&results), vec!["visible"]);
    }

    #[test]
    fn ordering_is_created_at_then_id() {
        // Same millisecond, inserted in reverse id order: the tiebreaker is id.
        let candidates = vec![
            env("tie-b", "t", 5_000, &["*"]),
            env("tie-a", "t", 5_000, &["*"]),
        ];
        let results = select(candidates, &json!({}), &star(), None);
        assert_eq!(ids(&results), vec!["tie-a", "tie-b"]);

        // ...but created_at is the *primary* key, so an id that sorts first must
        // still come last if it was written last. Sorting by id alone passes the
        // case above and fails this one.
        let candidates = vec![
            env("zzz", "t", 1_000, &["*"]),
            env("aaa", "t", 9_000, &["*"]),
        ];
        let results = select(candidates, &json!({}), &star(), None);
        assert_eq!(ids(&results), vec!["zzz", "aaa"]);
    }

    #[test]
    fn a_non_string_id_condition_is_impossible_rather_than_a_scan() {
        assert!(matches!(
            pushdown_id(&json!({"id": "x"})),
            Some(Pushdown::Id(_))
        ));
        assert!(matches!(
            pushdown_id(&json!({"id": 7})),
            Some(Pushdown::Impossible)
        ));
        assert!(pushdown_id(&json!({"payload.kind": "tool"})).is_none());
        assert!(pushdown_id(&json!({})).is_none());
    }
}
