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
//! identical, only the cost changes.
//!
//! # Indexed fields: two phases, and why the second one is not optional
//!
//! With an [`IndexPlan`] attached, a template filtering on `payload.farmId`
//! runs
//!
//! ```text
//! phase 1   Query meshql_ix_farmId:  ix_farmId = :value AND sk < :cutoff
//!             → candidate ids
//! phase 2   query_latest(id) for each distinct candidate
//!             → resolved versions, then the pipeline above, unchanged
//! ```
//!
//! Phase 1 replaces *only* the `Scan`. Everything after it — resolve, drop
//! tombstones, match, filter by tokens, order, limit — is the same code on the
//! same envelopes, which is why the certification suites pass identically with
//! and without indexes.
//!
//! **Phase 2 cannot be skipped, and no projection buys it back.** A GSI holds
//! the *versions* whose indexed value matched, which is not the set of records
//! whose *resolved* version matches: an id that used to be `kind = tool` stays
//! in the `tool` partition of the index forever. The tempting shortcut is an
//! `ALL`-projection index that resolves latest-per-id *inside* the index
//! results and re-checks the predicate there. It is unsound, and it was
//! measured to be unsound against real data:
//!
//! ```text
//! id "mover":   v1 (kind = tool)  →  v2 (kind = widget)
//! id "stayer":  v1 (kind = tool)
//!
//! index-only shortcut  →  {stayer, mover}     ✗  mover is a widget now
//! two-phase            →  {stayer}            ✓
//! ```
//!
//! `mover`'s v2 lives in the `widget` partition, so the `tool` query cannot see
//! it and **no amount of re-checking inside the result set can find the
//! error**. That is the same class of bug
//! `test_searcher_auth_latest_version_controls_visibility` exists to catch, and
//! `tests/searcher_cert.rs::the_index_cannot_resurrect_a_superseded_version`
//! pins it on the shipped path. It is also why the projection is `KEYS_ONLY`:
//! a wider one costs more to write and is never read.
//!
//! # No silent fallback
//!
//! With a plan attached, a template naming a payload field the plan does not
//! cover is an **error**, not a scan. A scan at a million versions is 45
//! seconds and $0.0156 a call; degrading into one quietly is how a deployment
//! comes to believe it is indexed when it is not.
//!
//! Two shapes are *not* errors, because neither is a degradation:
//!
//! - `{}` — `getAll` is an irreducible `Scan`, by construction. Visibility is
//!   `authorized_tokens`, a **list** attribute, and a GSI key must be a scalar,
//!   so token-visibility cannot be indexed at all. See
//!   `docs/cost-model-dynamodb.md` §8.
//! - a key that is neither `id` nor `payload.…` — it resolves no path, so it
//!   matches nothing on every meshql backend. With a plan attached that is
//!   answered from the plan, with no request at all: empty is what the scan
//!   would have returned, so returning it for free changes nothing but the
//!   bill. (Configuration is refused at startup for the same shape — see
//!   [`crate::index`] — so a running deployment should never reach this.)

use async_trait::async_trait;
use aws_sdk_dynamodb::Client;
use handlebars::Handlebars;
use meshql_core::{Envelope, MeshqlError, Result, RootConfig, Searcher, Stash};
use serde_json::{json, Value};

use crate::index::{self, IndexPlan, Key};
use crate::metering::CapacityMeter;
use crate::{matcher, store};

pub struct DynamoSearcher {
    client: Client,
    table: String,
    handlebars: Handlebars<'static>,
    /// `None` unless an operator asked for metering. See [`crate::metering`].
    meter: Option<std::sync::Arc<CapacityMeter>>,
    /// The indexed payload fields. Empty means "no indexes", which is the
    /// unindexed constructors' behaviour: every non-`id` search is a `Scan`.
    plan: IndexPlan,
    /// Segments for the `Scan` paths that remain. See
    /// [`store::scan_latest_segmented`].
    scan_segments: i32,
}

impl DynamoSearcher {
    /// `endpoint: None` → real AWS from the ambient config. `endpoint:
    /// Some(url)` → DynamoDB Local (see [`store::make_client`]).
    ///
    /// **No indexes.** Every search without an `"id"` is a full table `Scan`,
    /// which is `O(total versions)` in both money and serialised round trips.
    /// That is fine for a small table and unservable for a large one; see
    /// [`Self::indexed`] and `docs/cost-model-dynamodb.md`.
    pub async fn new(endpoint: Option<&str>, table: &str) -> Result<Self> {
        let client = store::make_client(endpoint).await;
        Self::new_with_client(client, table).await
    }

    /// Share one client (and one table) with a [`crate::DynamoRepository`].
    pub async fn new_with_client(client: Client, table: &str) -> Result<Self> {
        Self::build(client, table, IndexPlan::default()).await
    }

    /// A searcher whose indexes are **derived from the configuration it will
    /// serve**, provisioning whatever the templates in `config` need.
    ///
    /// The deployment declares nothing extra: the `RootConfig` the graphlette
    /// already has is the whole input. Because the index set and the queries
    /// come from the same object, they cannot drift apart.
    ///
    /// Fails at startup — not at the first query, and never by falling back to
    /// a scan — when a template cannot be served from an index, or when the
    /// derived set exceeds DynamoDB's 20-index limit. See [`crate::index`].
    ///
    /// The repository over the same table **must** be given the same plan, or
    /// its writes carry no promoted attributes and the searches that use them
    /// return silently incomplete results. [`crate::DynamoCollection`] builds
    /// both from one config so that cannot happen; opening a table whose
    /// indexes disagree with the handle's plan is refused
    /// ([`store::ensure_indexed_table`]).
    pub async fn indexed(endpoint: Option<&str>, table: &str, config: &RootConfig) -> Result<Self> {
        let client = store::make_client(endpoint).await;
        Self::indexed_with_client(client, table, config).await
    }

    /// [`Self::indexed`], sharing a client.
    pub async fn indexed_with_client(
        client: Client,
        table: &str,
        config: &RootConfig,
    ) -> Result<Self> {
        Self::build(client, table, IndexPlan::derive(config)?).await
    }

    /// A searcher over an explicit plan.
    ///
    /// Prefer [`Self::indexed`]. Pass the config through
    /// [`IndexPlan::verify_covers`] if you build a plan by hand — a plan that
    /// does not cover the queries it serves is exactly the drift derivation
    /// exists to remove.
    pub async fn with_plan(client: Client, table: &str, plan: IndexPlan) -> Result<Self> {
        Self::build(client, table, plan).await
    }

    async fn build(client: Client, table: &str, plan: IndexPlan) -> Result<Self> {
        let mut handlebars = Handlebars::new();
        handlebars.set_strict_mode(false);
        let searcher = Self {
            client,
            table: table.to_string(),
            handlebars,
            meter: None,
            plan,
            scan_segments: 1,
        };
        searcher.ensure_table().await?;
        Ok(searcher)
    }

    /// The indexed fields.
    pub fn plan(&self) -> &IndexPlan {
        &self.plan
    }

    /// Split the `Scan` paths that remain — `getAll`, and an unindexed
    /// searcher's every non-`id` query — across `segments` concurrent workers.
    ///
    /// Latency only, and cheaper than expected. Capacity is charged on bytes
    /// examined and the segments partition the same bytes, so RRU is invariant
    /// in the segment count — measured at 0.012% drift across a 64× range at
    /// V = 1,000,000, **and measured invariant at four segments on a
    /// three-item table too**, which was not expected: a serial `Scan` is
    /// already charged per partition, so four segments re-partition a rounding
    /// that was being paid anyway. Sixteen segments does cost 4.8× more on that
    /// table.
    ///
    /// The default is one anyway, and the reason is not cost: four round trips
    /// buy nothing on a table small enough to fit in one page, and the 2.63×
    /// wall-clock win only exists above the ~58 MB/s consumer-side ceiling. The
    /// deployments that want this are running an export, and an export is the
    /// only thing that should be scanning a large table. See
    /// [`store::scan_latest_segmented`].
    pub fn with_scan_segments(mut self, segments: i32) -> Self {
        self.scan_segments = segments.max(1);
        self
    }

    /// Account every request this searcher makes against `meter`.
    ///
    /// This is the one worth attaching. A search without an `"id"` key is a
    /// paginated `Scan`, and the meter is what turns "a scan, presumably
    /// expensive" into a number an operator can put in a budget.
    pub fn with_meter(mut self, meter: std::sync::Arc<CapacityMeter>) -> Self {
        self.meter = Some(meter);
        self
    }

    /// The attached meter, if any.
    pub fn meter(&self) -> Option<&std::sync::Arc<CapacityMeter>> {
        self.meter.as_ref()
    }

    fn meter_ref(&self) -> Option<&CapacityMeter> {
        self.meter.as_deref()
    }

    pub async fn ensure_table(&self) -> Result<()> {
        store::ensure_indexed_table(&self.client, &self.table, &self.plan).await
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

        let candidates = match access_for(&self.plan, &self.table, &query, template)? {
            Access::Id(id) => {
                match store::query_latest(&self.client, &self.table, &id, cutoff, self.meter_ref())
                    .await?
                {
                    Some(env) if !env.deleted => vec![env],
                    _ => Vec::new(),
                }
            }
            // Nothing this query could match, known before any I/O. Distinct
            // from an empty result: no request is made at all.
            Access::Nothing => Vec::new(),
            Access::Index(conditions) => self.two_phase(&conditions, cutoff).await?,
            Access::Scan => {
                store::scan_latest_segmented(
                    &self.client,
                    &self.table,
                    cutoff,
                    self.meter_ref(),
                    self.scan_segments,
                )
                .await?
            }
        };

        Ok(select(candidates, &query, creds, limit))
    }

    /// Phase 1 then phase 2. The result is the *resolved* version of every
    /// candidate with the tombstones dropped — exactly the shape
    /// [`store::scan_latest`] returns, so [`select`] cannot tell which path
    /// produced it. That is what makes the certification suites pass
    /// identically indexed and unindexed.
    async fn two_phase(
        &self,
        conditions: &[(String, String)],
        cutoff: i64,
    ) -> Result<Vec<Envelope>> {
        let mut candidates: Option<std::collections::HashSet<String>> = None;

        // Several indexed conditions intersect, and the intersection is sound
        // rather than merely selective: a record whose *resolved* version
        // satisfies every condition has that one version present in every one
        // of those indexes, so it survives. What the intersection removes is
        // ids that could only have matched on a superseded version — which
        // phase 2 would have spent half a read unit each to reject anyway.
        for (field, value) in conditions {
            let found = store::query_index_candidates(
                &self.client,
                &self.table,
                field,
                value,
                cutoff,
                self.meter_ref(),
            )
            .await?;
            candidates = Some(match candidates {
                None => found,
                Some(existing) => existing.intersection(&found).cloned().collect(),
            });
            if candidates.as_ref().is_some_and(|c| c.is_empty()) {
                return Ok(Vec::new());
            }
        }

        store::resolve_candidates(
            &self.client,
            &self.table,
            candidates.unwrap_or_default(),
            cutoff,
            self.meter_ref(),
        )
        .await
    }
}

/// How a rendered query will be answered — decided before any I/O, so that
/// "this one would have been a `Scan`" is a value a test can assert on rather
/// than a bill someone notices later.
///
/// Free-standing, and takes the plan rather than a searcher, so that every
/// guard below is checkable without a DynamoDB.
fn access_for(plan: &IndexPlan, table: &str, query: &Value, template: &str) -> Result<Access> {
    let object = match query.as_object() {
        Some(o) => o,
        None => return Ok(Access::Nothing),
    };

    // The one safe pushdown, and the cheapest path there is: a single `Query`
    // on the base table's hash key. It needs no index and it beats one.
    if let Some(id) = object.get("id") {
        return Ok(match id {
            Value::String(s) => Access::Id(s.clone()),
            // An `"id"` condition whose value is not a string can never equal a
            // record's id.
            _ => Access::Nothing,
        });
    }

    let mut conditions = Vec::new();
    for (key, value) in object {
        match index::classify(key) {
            Key::Id => {} // handled above
            // Resolves to no path, so it matches nothing on every meshql
            // backend — answerable from the template, with no request.
            Key::Unmatchable => return Ok(Access::Nothing),
            Key::Payload(field) => {
                if plan.is_empty() {
                    continue; // no plan at all: this searcher scans
                }
                if !plan.covers(field) {
                    return Err(MeshqlError::Validation(format!(
                        "template filters on payload field {field:?}, which has no index on \
                         table {table:?} (indexed: {}). Serving it means a full table Scan — \
                         O(every version ever written): 45 seconds and $0.0156 a call at a \
                         million versions — so it is refused rather than degraded into one. \
                         Derive the searcher's plan from the same RootConfig the graphlette \
                         uses. Template: {template}",
                        plan.describe(),
                    )));
                }
                match value {
                    Value::String(s) => conditions.push((field.to_string(), s.clone())),
                    _ => {
                        return Err(MeshqlError::Validation(format!(
                            "template filters indexed payload field {field:?} on a \
                             non-string value ({value}). Promoted index attributes are \
                             strings, so this could only be served by a full table Scan. \
                             Quote the placeholder. Template: {template}"
                        )))
                    }
                }
            }
        }
    }

    if conditions.is_empty() {
        // `{}`, or a searcher with no plan. getAll is irreducible: token
        // visibility is a list attribute and a GSI key must be a scalar.
        Ok(Access::Scan)
    } else {
        Ok(Access::Index(conditions))
    }
}

/// How a rendered query will be answered.
#[derive(Debug, PartialEq, Eq)]
enum Access {
    /// One `Query` on the base table's hash key.
    Id(String),
    /// No request at all — the query cannot match anything.
    Nothing,
    /// Two-phase, one `(field, value)` per indexed equality condition.
    Index(Vec<(String, String)>),
    /// A full table `Scan`: `getAll`, or a searcher with no plan.
    Scan,
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

    // ---- how a query will be answered, decided before any I/O ----

    fn unindexed() -> IndexPlan {
        IndexPlan::default()
    }

    fn indexed(fields: &[&str]) -> IndexPlan {
        IndexPlan::from_fields(fields.iter().copied()).unwrap()
    }

    fn access(plan: &IndexPlan, query: Value) -> Result<Access> {
        access_for(plan, "t", &query, "<template>")
    }

    #[test]
    fn an_id_condition_is_one_query_indexed_or_not() {
        for plan in [unindexed(), indexed(&["kind"])] {
            assert_eq!(
                access(&plan, json!({"id": "x"})).unwrap(),
                Access::Id("x".into())
            );
        }
    }

    #[test]
    fn a_non_string_id_condition_is_impossible_rather_than_a_scan() {
        assert_eq!(
            access(&unindexed(), json!({"id": 7})).unwrap(),
            Access::Nothing
        );
    }

    #[test]
    fn with_no_plan_a_payload_condition_is_a_scan() {
        assert_eq!(
            access(&unindexed(), json!({"payload.kind": "tool"})).unwrap(),
            Access::Scan
        );
    }

    /// The heart of it: an indexed field becomes a two-phase query, and
    /// **never** silently a scan.
    #[test]
    fn an_indexed_field_is_a_two_phase_query() {
        assert_eq!(
            access(&indexed(&["kind"]), json!({"payload.kind": "tool"})).unwrap(),
            Access::Index(vec![("kind".into(), "tool".into())])
        );
    }

    #[test]
    fn every_indexed_condition_narrows_the_candidate_set() {
        let got = access(
            &indexed(&["kind", "zone"]),
            json!({"payload.kind": "tool", "payload.zone": "north"}),
        )
        .unwrap();
        assert_eq!(
            got,
            Access::Index(vec![
                ("kind".into(), "tool".into()),
                ("zone".into(), "north".into()),
            ])
        );
    }

    /// Guard 1. Not a scan. Not a warning. An error that names the field and
    /// the template, because the alternative is a deployment that believes it
    /// is indexed and is quietly paying `O(V)` per search.
    #[test]
    fn an_unindexed_field_is_refused_and_the_message_names_it() {
        let err = access(&indexed(&["kind"]), json!({"payload.zone": "north"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("zone"), "name the field: {err}");
        assert!(err.contains("<template>"), "name the template: {err}");
        assert!(err.contains("Scan"), "say what it refused to do: {err}");
        assert!(err.contains('t'), "name the table: {err}");
    }

    /// ...including when one condition of several is unindexed. A partial index
    /// would still return the right answer — phase 2 re-matches everything — so
    /// the temptation to allow it is real, and the reason not to is that the
    /// query's cost would then depend on which condition happened to be
    /// indexed.
    #[test]
    fn one_unindexed_condition_among_several_is_still_refused() {
        assert!(access(
            &indexed(&["kind"]),
            json!({"payload.kind": "tool", "payload.zone": "north"}),
        )
        .is_err());
    }

    #[test]
    fn a_non_string_filter_value_on_an_indexed_field_is_refused() {
        let err = access(&indexed(&["count"]), json!({"payload.count": 3}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("count"), "{err}");
        assert!(err.contains("Quote the placeholder"), "{err}");
    }

    /// Guard's exception: `getAll` is irreducible and stays a scan. Token
    /// visibility is a list attribute; a GSI key must be scalar.
    #[test]
    fn get_all_stays_a_scan_even_with_a_full_plan() {
        assert_eq!(
            access(&indexed(&["kind", "zone"]), json!({})).unwrap(),
            Access::Scan
        );
    }

    /// An unrecognised key matches nothing on every backend, so with a plan in
    /// hand it is answered from the plan — no request, no scan, same answer.
    #[test]
    fn a_bare_payload_key_costs_nothing_at_all() {
        assert_eq!(
            access(&indexed(&["kind"]), json!({"kind": "tool"})).unwrap(),
            Access::Nothing
        );
        // ...and the same is true unindexed, where it used to provoke a full
        // scan whose result was empty by construction.
        assert_eq!(
            access(&unindexed(), json!({"kind": "tool"})).unwrap(),
            Access::Nothing
        );
    }
}
