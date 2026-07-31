//! HubSpot CRM ingress: incremental search polling, and the cursor that makes
//! a shared millisecond safe.
//!
//! # This is an *ingress* connector, and that changes the job
//!
//! The SQLite, Mongo and Postgres sources read a store the mesh itself wrote:
//! the rows already *are* `meshql_core::Envelope`s, and the connector's whole
//! job is to notice them and forward them. HubSpot wrote none of this. A
//! HubSpot contact is a bag of properties belonging to somebody else's system,
//! so this module **synthesises** an envelope at the boundary — id, payload,
//! authorized tokens — and everything downstream reads the synthesis rather
//! than the CRM.
//!
//! Two consequences follow immediately, and both are load-bearing.
//!
//! ## The envelope id is namespaced by object type
//!
//! HubSpot object ids are unique **within an object type only**. Contact
//! `512` and deal `512` are unrelated records that happen to share a number.
//! meshql ids are global to a topic, and — because [`ChangeRecord::key`] is the
//! envelope id — they are also the merkql partition key. Mapping both to the
//! bare id `"512"` would make the deal look like a *new version of the
//! contact*: the read path resolves an id to its latest version, so the deal
//! would silently replace the contact and nothing anywhere would record that
//! two different records had been merged. That is not a failure a fold can
//! detect, so it must be made impossible.
//!
//! The id is therefore always `"{object_type}:{hubspot_id}"` —
//! `"contacts:512"`, `"deals:512"` — **including** when a connector captures a
//! single object type. Making it conditional would mean that adding a second
//! object type to a running connector retroactively changed the identity of
//! every record already on the topic, which is a worse failure than the one it
//! saves a few bytes on.
//!
//! ## The Debezium `source` block does not survive, so it is copied into the payload
//!
//! [`SourceInfo`] rides on the [`ChangeRecord`], and a consumer that reads the
//! topic directly can see it. A consumer that feeds the records through a
//! meshql `Repository` cannot: the repository persists the [`Envelope`] and
//! nothing else, so `op`, `ts_ms` and the whole `source` block are dropped on
//! the floor at that boundary. Anything the *domain* needs must live inside
//! `envelope.payload` or it does not exist downstream.
//!
//! So the payload carries a `_source` block mirroring the parts of `SourceInfo`
//! a domain actually asks for:
//!
//! ```json
//! {
//!   "email": "hen@farm.example",
//!   "_source": {
//!     "connector": "hubspot",
//!     "object_type": "contacts",
//!     "object_id": "512",
//!     "lastmodified_ms": 1751892345123
//!   }
//! }
//! ```
//!
//! `object_type` is what lets a downstream query say "deals only" once several
//! types share a topic; `object_id` is the un-prefixed id, so a projection can
//! link back to HubSpot without parsing our composite id; `lastmodified_ms` is
//! the watermark as a **number**, because HubSpot returns it as an ISO-8601
//! string that no adapter can compare with `<`.
//!
//! The `_source` key is written last and wins over a HubSpot property of the
//! same name. A shadowed custom property is visible and diagnosable; a missing
//! object type is neither.
//!
//! ## `created_at` is HubSpot's modification time, never ours
//!
//! [`Envelope::new`] stamps `Utc::now()`, and this module deliberately does not
//! use it. meshql resolves an id to the version with the greatest `created_at`,
//! so stamping wall-clock time would mean a re-snapshot (or any at-least-once
//! re-delivery) minted a *newer* version than a live record that arrived
//! earlier — resurrecting stale CRM state as the current one. Using
//! `hs_lastmodifieddate` makes re-delivery genuinely idempotent: the same
//! HubSpot revision always produces the same `created_at`, whatever order it
//! arrives in.
//!
//! # Why polling, and why that is not a lie
//!
//! HubSpot offers two mechanisms. **Webhooks** push, but need a publicly
//! reachable HTTPS endpoint; `merkql-connect` is deployed next to
//! infrastructure and is frequently not routable from the internet at all.
//! **Incremental search** (`POST /crm/v3/objects/{type}/search` filtered on the
//! last-modified timestamp) needs nothing but an outbound connection.
//!
//! [`crate::source`] is explicit that the trait is push-shaped and that a
//! caller driving the clock cannot express "the store will tell you". A poller
//! behind this trait is legitimate — the source still owns delivery, and the
//! connector loop still parks on `.next()` — but it must not *pretend* to be a
//! feed. It is not one: latency is bounded below by `poll_interval_ms`, and
//! `connector()` reports `"hubspot"` so nothing downstream can mistake this for
//! a log-based capture.
//!
//! # What this connector does NOT capture
//!
//! - **Hard deletes and archives.** A deleted HubSpot object simply stops
//!   appearing in search results; there is no tombstone to poll for. This
//!   source therefore never emits [`Op::Delete`], and a projection built from
//!   it will retain records that no longer exist in the CRM. A domain that
//!   needs deletions needs webhooks (`*.deletion` subscriptions) or a periodic
//!   reconciliation pass; it cannot get them from here, and pretending
//!   otherwise is how a projection quietly drifts.
//! - **Property-level history.** Only the current value of each property is
//!   read, so two modifications inside one poll interval arrive as one record.
//!   The `_source.lastmodified_ms` is the later of the two.
//! - **Associations.** Only the object's own properties are captured.
//!
//! # The search result window, and paging by watermark
//!
//! HubSpot's search endpoint refuses to page beyond roughly **10,000 results**
//! per query. A backfill of a large portal hits that wall, and the mitigation
//! is *not* a deeper offset — it is to stop the query, advance the watermark to
//! the newest record seen, and issue a fresh query from there. Results are
//! sorted ascending by the timestamp, so the new query resumes exactly where
//! the old one was cut off.
//!
//! That works unless more than a window's worth of records share timestamps
//! inside one lag window, in which case advancing the watermark makes no
//! progress and paging cannot reach the rest. There is no correct answer that
//! also delivers everything, so [`HubspotSource`] stops with an error naming
//! both knobs rather than skipping the remainder.
//!
//! # Rate limits
//!
//! HubSpot enforces per-app request limits and answers `429` with a
//! `Retry-After` header. [`Http::send`] honours the header, falls back to
//! capped exponential backoff, and — the part that matters — **a stall is never
//! an end of stream**. A [`ChangeStream`] that returns `None` tells
//! [`crate::run_connector`] the source is finished, it commits the offset and
//! returns `Ok(())`, and the process exits zero looking healthy while a
//! throttled portal keeps changing. So the retry budget expiring yields
//! `Err(CdcError::Backend)`, the connector exits non-zero, and the supervisor
//! restarts it from a committed position. Under sustained `429`s this connector
//! is *slow and then loud*, never silent.
//!
//! # Credentials
//!
//! The private-app token is read from the environment, never from the TOML —
//! the same rule `meshql-ksql`'s config applies to its Confluent keys. The
//! config names the *variable* (default `HUBSPOT_PRIVATE_APP_TOKEN`) so two
//! connectors against two portals can run on one host, and an unset variable is
//! a startup error naming the variable rather than a `401` at the first poll.

use crate::config::SourceConfig;
use crate::record::{ChangeRecord, Op, Snapshot, SourceInfo};
use crate::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use async_trait::async_trait;
use futures::stream;
use meshql_core::{Envelope, Stash};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

const CONNECTOR: &str = "hubspot";

/// HubSpot's documented search result window. Paging past it is refused by the
/// API, so the connector switches to advancing the watermark instead.
pub const SEARCH_RESULT_WINDOW: u32 = 10_000;

/// HubSpot caps a search page at 100.
const MAX_PAGE_SIZE: u32 = 100;

/// How many times a `429` (or a 5xx, or a transport error) may be retried
/// before the stream fails. With [`RETRY_BASE`] and [`RETRY_CAP`] this is a few
/// minutes of stalling — long enough to ride out a burst limit, short enough
/// that an operator finds out about a sustained one.
const MAX_RETRY_ATTEMPTS: u32 = 8;
const RETRY_BASE: Duration = Duration::from_secs(1);
const RETRY_CAP: Duration = Duration::from_secs(60);

/// Hard ceiling on the boundary-id set carried in the cursor. Evicting the
/// *oldest* entries costs re-delivery of records the connector already emitted
/// — duplicates, which at-least-once permits — whereas raising the watermark to
/// shrink the set would skip them. See [`TypeCursor::accept`].
const MAX_SEEN_IDS: usize = 50_000;

/// The property carrying an object's last-modified timestamp.
///
/// This is not uniform across HubSpot, and the inconsistency is a trap:
/// contacts use `lastmodifieddate` while every other CRM object (companies,
/// deals, tickets, custom objects) uses `hs_lastmodifieddate`. Filtering
/// contacts on `hs_lastmodifieddate` does not error — HubSpot happily returns
/// an empty result set for an unknown property — so the wrong name here
/// produces a connector that runs forever and delivers nothing.
fn timestamp_property(object_type: &str) -> &'static str {
    match object_type {
        "contacts" | "contact" => "lastmodifieddate",
        _ => "hs_lastmodifieddate",
    }
}

// ── configuration ────────────────────────────────────────────────────────

/// Everything the source needs except the secret.
///
/// Split out from [`SourceConfig`] so the token has exactly one way in — the
/// environment — and so the offline tests can build a source without touching
/// process-global state.
#[derive(Debug, Clone)]
pub struct Settings {
    /// API root. Overridden only by tests and by a proxy deployment.
    pub base_url: String,
    /// CRM object types to capture, in the order they are polled.
    pub objects: Vec<String>,
    /// The entity name reported in `source.entity` and used to name the offset
    /// file.
    pub entity: String,
    /// Copied onto every synthesised envelope. HubSpot has no notion of a
    /// meshql token, so authorisation is a property of the *connector*, not of
    /// the record — see [`HubspotSource`]'s docs.
    pub authorized_tokens: Vec<String>,
    /// Properties to request. Empty means HubSpot's default set.
    pub properties: Vec<String>,
    pub page_size: u32,
    pub poll_interval: Duration,
    /// How far behind the newest record seen the watermark is held, in
    /// milliseconds. See [`TypeCursor`] — this is the eventual-consistency
    /// guard, and setting it to zero reduces the cursor to the
    /// same-millisecond tie-break alone.
    pub index_lag: Duration,
    /// HubSpot's search result window. A field rather than a constant so the
    /// no-progress guard is testable without ten thousand fixtures; leave it at
    /// [`SEARCH_RESULT_WINDOW`] in production.
    pub search_window: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            base_url: "https://api.hubapi.com".to_string(),
            objects: vec!["contacts".to_string()],
            entity: "hubspot".to_string(),
            authorized_tokens: Vec::new(),
            properties: Vec::new(),
            page_size: MAX_PAGE_SIZE,
            poll_interval: Duration::from_secs(30),
            index_lag: Duration::from_secs(5),
            search_window: SEARCH_RESULT_WINDOW,
        }
    }
}

/// Read the private-app token out of the environment.
///
/// Loud and specific, because the alternative — discovering an absent token as
/// a `401` on the first poll — happens minutes into a run, after the connector
/// has already claimed the topic and printed a healthy-looking startup line.
pub fn token_from_env(var: &str) -> Result<String, CdcError> {
    match std::env::var(var) {
        Ok(token) if !token.trim().is_empty() => Ok(token),
        Ok(_) => Err(CdcError::Backend(anyhow::anyhow!(
            "environment variable {var} is set but empty; it must hold the HubSpot \
             private-app access token. Secrets are never read from the connector TOML."
        ))),
        Err(_) => Err(CdcError::Backend(anyhow::anyhow!(
            "environment variable {var} is not set; it must hold the HubSpot private-app \
             access token (Settings -> Integrations -> Private Apps -> Auth). Secrets are \
             never read from the connector TOML; set `token_env` in the source config to \
             point at a different variable."
        ))),
    }
}

// ── the cursor ───────────────────────────────────────────────────────────

/// One object type's position: a watermark and the ids already emitted at or
/// after it.
///
/// # Why a watermark alone is wrong
///
/// The obvious cursor is "the greatest `hs_lastmodifieddate` I have emitted",
/// resumed with `> watermark`. It is wrong, and wrong in the direction this
/// crate forbids: HubSpot stamps millisecond precision and a bulk import,
/// workflow or list-membership change writes hundreds of records in the same
/// millisecond. Resuming with `>` skips every record that shared the boundary
/// millisecond with the last one emitted — a silent gap, invisible downstream,
/// and one that only appears under exactly the load that matters.
///
/// Resuming with `>=` fixes the gap and creates a livelock: the boundary
/// records come back on every poll forever, and if enough of them share the
/// millisecond to fill the result window, the watermark can never advance past
/// them at all.
///
/// So the cursor is `>=` **plus the set of ids already emitted at or after the
/// watermark**. Boundary records are re-fetched and filtered out by id; new
/// ones at the same millisecond are emitted; the watermark advances as soon as
/// a later millisecond appears. Duplicates are avoided, and the failure mode if
/// the set is ever wrong is re-delivery rather than loss.
///
/// # Why the set is a window and not a single millisecond
///
/// HubSpot's search index is *eventually consistent*: a modification can take
/// seconds to become searchable, and records do not become searchable in
/// timestamp order. So a record stamped `T` can appear only after a record
/// stamped `T + 400` has already been emitted. A watermark that had advanced to
/// `T + 400` would filter the late one out with `>=` and lose it — the same
/// silent gap by a different route.
///
/// The watermark is therefore held `index_lag` behind the newest timestamp
/// seen, and `seen` holds every id emitted at or after it. With
/// `index_lag = 0` this degenerates *exactly* to the same-millisecond tie-break
/// above, which is why there is one mechanism here and not two.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TypeCursor {
    /// The watermark, epoch millis. The next query filters `>= ms`.
    ms: i64,
    /// The newest timestamp observed. Carried rather than recomputed from
    /// `seen`: a max over the whole set on every single result turns a busy
    /// boundary into quadratic work.
    #[serde(default)]
    high: i64,
    /// `id -> lastmodified_ms` for every record emitted at or after `ms`.
    /// Serialised as a map so the encoding is stable regardless of iteration
    /// order — an unstable cursor string rewrites the offset file on every
    /// commit and makes a resume unreproducible.
    seen: BTreeMap<String, i64>,
}

impl TypeCursor {
    /// Consider a search result. `Ok(true)` means emit it.
    ///
    /// `lag_ms` is how far behind the newest timestamp seen the watermark is
    /// held.
    fn accept(&mut self, id: &str, ts: i64, lag_ms: i64) -> Result<bool, CdcError> {
        // Results are requested `>= ms` and sorted ascending, so a timestamp
        // below the watermark means HubSpot did not honour one of those. If it
        // happened silently we would advance past records we never saw, so it
        // is reported instead. Never a silent skip.
        if ts < self.ms {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "HubSpot returned a record modified at {ts} while the query filtered \
                 `>= {}` and sorted ascending. Either the filter or the sort was not \
                 honoured, and continuing would advance the watermark past records that \
                 were never delivered.",
                self.ms
            )));
        }

        let fresh = !self.seen.contains_key(id);
        self.seen.insert(id.to_string(), ts);
        self.high = self.high.max(ts);

        // The watermark trails the newest timestamp by the lag window, and only
        // ever moves forwards — a query returning an older tail must not drag
        // it back and re-deliver everything since.
        let candidate = self.high.saturating_sub(lag_ms);
        if candidate > self.ms {
            self.ms = candidate;
            // Only when the watermark actually moved: ids below it can never be
            // re-fetched, but sweeping the whole set on every accepted result
            // would make one busy millisecond quadratic.
            self.seen.retain(|_, seen_ts| *seen_ts >= self.ms);
        }

        // A cursor is written to the offset file on every commit, so it cannot
        // be allowed to grow without bound. Evicting the OLDEST entries is the
        // safe direction: those records are still `>= ms`, so they are
        // re-fetched and re-emitted — duplicates, which folds absorb. Raising
        // `ms` to shrink the set instead would drop them for good.
        while self.seen.len() > MAX_SEEN_IDS {
            let Some(oldest) = self
                .seen
                .iter()
                .min_by_key(|(_, ts)| **ts)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.seen.remove(&oldest);
        }

        Ok(fresh)
    }
}

/// The connector's whole position: one [`TypeCursor`] per object type.
///
/// Versioned, so a future format change is an explicit
/// [`CdcError::UnusablePosition`] — subject to `snapshot_mode` like any other
/// unusable position — rather than a serde error that reads as a backend
/// failure and stops the connector with no policy applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Cursor {
    v: u8,
    types: BTreeMap<String, TypeCursor>,
}

const CURSOR_VERSION: u8 = 1;

impl Default for Cursor {
    /// A fresh cursor is a *current* cursor. Deriving this would produce `v: 0`
    /// and every cold start would encode a position its own `decode` rejects.
    fn default() -> Self {
        Self {
            v: CURSOR_VERSION,
            types: BTreeMap::new(),
        }
    }
}

impl Cursor {
    fn encode(&self) -> String {
        // Infallible in practice: the whole structure is maps of strings and
        // integers. Falling back to an empty cursor on failure would be a
        // silent rewind, so a failure surfaces as an obviously-broken position
        // that `decode` will reject.
        serde_json::to_string(self).unwrap_or_else(|e| format!("<uncodable cursor: {e}>"))
    }

    fn decode(position: &str) -> Result<Self, CdcError> {
        let mut cursor: Cursor =
            serde_json::from_str(position).map_err(|e| CdcError::UnusablePosition {
                connector: CONNECTOR,
                position: position.to_string(),
                reason: format!("not a HubSpot connector cursor: {e}"),
            })?;
        if cursor.v != CURSOR_VERSION {
            return Err(CdcError::UnusablePosition {
                connector: CONNECTOR,
                position: position.to_string(),
                reason: format!(
                    "cursor format v{}, but this build understands v{CURSOR_VERSION}",
                    cursor.v
                ),
            });
        }
        // `high` is a cache of what `seen` and `ms` already imply. Restoring it
        // from them rather than trusting the file means a hand-edited or
        // truncated cursor cannot make the watermark jump forwards, which is
        // the only direction that loses records.
        for entry in cursor.types.values_mut() {
            let observed = entry.seen.values().copied().max().unwrap_or(entry.ms);
            entry.high = observed.max(entry.ms);
        }
        Ok(cursor)
    }

    fn entry(&mut self, object_type: &str) -> &mut TypeCursor {
        self.types.entry(object_type.to_string()).or_default()
    }

    fn watermark(&self, object_type: &str) -> i64 {
        self.types.get(object_type).map(|c| c.ms).unwrap_or(0)
    }
}

// ── the HTTP client ──────────────────────────────────────────────────────

/// One page of search results, narrowed to what the feed reads.
#[derive(Debug, Default)]
struct Page {
    results: Vec<SearchResult>,
    next_after: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    id: String,
    #[serde(default)]
    properties: Stash,
    /// HubSpot's top-level modification timestamp, ISO-8601. Used only as a
    /// fallback — see [`Http::watermark_of`].
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<String>,
    #[serde(default)]
    archived: bool,
}

struct Http {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

impl Http {
    /// Issue one search page, retrying rate limits and transient failures.
    async fn search(
        &self,
        object_type: &str,
        since_ms: i64,
        after: Option<&str>,
        properties: &[String],
        limit: u32,
    ) -> Result<Page, CdcError> {
        let property = timestamp_property(object_type);
        let mut body = json!({
            "filterGroups": [{
                "filters": [{
                    "propertyName": property,
                    "operator": "GTE",
                    // HubSpot takes datetime filter values as epoch millis, as
                    // a string.
                    "value": since_ms.to_string(),
                }]
            }],
            // Ascending is not cosmetic: the cursor's whole safety argument is
            // that every record not yet emitted has a timestamp at or after the
            // last one that was. Descending, or unsorted, and the watermark
            // would advance past records still to come.
            "sorts": [{ "propertyName": property, "direction": "ASCENDING" }],
            "limit": limit,
        });
        if !properties.is_empty() {
            // Always request the timestamp property, whatever the operator
            // listed. Without it the response carries no watermark, the cursor
            // cannot advance, and the connector re-reads the same page forever.
            let mut requested: Vec<String> = properties.to_vec();
            if !requested.iter().any(|p| p == property) {
                requested.push(property.to_string());
            }
            body["properties"] = json!(requested);
        }
        if let Some(after) = after {
            body["after"] = json!(after);
        }

        let url = format!(
            "{}/crm/v3/objects/{object_type}/search",
            self.base_url.trim_end_matches('/')
        );
        let value = self.send(&url, &body).await?;

        let results: Vec<SearchResult> = match value.get("results") {
            Some(results) => serde_json::from_value(results.clone()).map_err(|e| {
                CdcError::Backend(anyhow::anyhow!(
                    "unreadable HubSpot search results for {object_type}: {e}"
                ))
            })?,
            None => Vec::new(),
        };
        let next_after = value
            .get("paging")
            .and_then(|p| p.get("next"))
            .and_then(|n| n.get("after"))
            .and_then(|a| a.as_str())
            .map(|a| a.to_string());

        Ok(Page {
            results,
            next_after,
        })
    }

    /// The newest timestamp, descending — used to place a `snapshot_mode =
    /// never` cold start.
    ///
    /// This exists so that "start from now" can be expressed in **HubSpot's**
    /// clock rather than ours. `Utc::now()` looks equivalent and is not: a
    /// connector host whose clock runs even slightly fast would set a watermark
    /// in HubSpot's future and silently skip every record modified in between.
    async fn newest_timestamp(&self, object_type: &str) -> Result<Option<(String, i64)>, CdcError> {
        let property = timestamp_property(object_type);
        let body = json!({
            "sorts": [{ "propertyName": property, "direction": "DESCENDING" }],
            "properties": [property],
            "limit": 1,
        });
        let url = format!(
            "{}/crm/v3/objects/{object_type}/search",
            self.base_url.trim_end_matches('/')
        );
        let value = self.send(&url, &body).await?;
        let results: Vec<SearchResult> = value
            .get("results")
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .unwrap_or_default();
        match results.first() {
            Some(result) => {
                let ts = Self::watermark_of(result, object_type)?;
                Ok(Some((result.id.clone(), ts)))
            }
            None => Ok(None),
        }
    }

    /// The timestamp a result is filed under.
    ///
    /// The property is read **first**, and the top-level `updatedAt` is only a
    /// fallback. That order is the correctness argument: the cursor's next
    /// query filters and sorts on the property, so a watermark taken from any
    /// other field could name a value the query cannot reproduce, and every
    /// record between the two would be skipped.
    fn watermark_of(result: &SearchResult, object_type: &str) -> Result<i64, CdcError> {
        let property = timestamp_property(object_type);
        if let Some(ts) = result.properties.get(property).and_then(parse_timestamp) {
            return Ok(ts);
        }
        if let Some(ts) = result
            .updated_at
            .as_deref()
            .and_then(|s| parse_timestamp(&Value::String(s.to_string())))
        {
            return Ok(ts);
        }
        // Defaulting to "now" here, or to the current watermark, would let a
        // single unreadable record either replay forever or vanish. Neither is
        // recoverable downstream, so the connector stops.
        Err(CdcError::Backend(anyhow::anyhow!(
            "HubSpot object {object_type}/{} carries no readable '{property}' and no \
             'updatedAt', so it has no position on the timeline this connector orders by. \
             Add '{property}' to the source's `properties` list.",
            result.id
        )))
    }

    /// POST, with the rate-limit and transient-failure policy.
    ///
    /// Every non-success path resolves one of two ways: retry, or return an
    /// error. Neither ends the stream, which is what keeps a throttled portal
    /// from looking like a portal with nothing left to say.
    async fn send(&self, url: &str, body: &Value) -> Result<Value, CdcError> {
        let mut attempt = 0u32;
        loop {
            let response = self
                .client
                .post(url)
                .bearer_auth(&self.token)
                .json(body)
                .send()
                .await;

            let wait = match response {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return response.json::<Value>().await.map_err(|e| {
                            CdcError::Backend(anyhow::anyhow!("unreadable HubSpot response: {e}"))
                        });
                    }

                    // A rejected token is a configuration error, not a
                    // transient one. Retrying it eight times just delays the
                    // moment an operator learns the token is wrong, and burns
                    // the app's request budget doing it.
                    if status == reqwest::StatusCode::UNAUTHORIZED
                        || status == reqwest::StatusCode::FORBIDDEN
                    {
                        let detail = response.text().await.unwrap_or_default();
                        return Err(CdcError::Backend(anyhow::anyhow!(
                            "HubSpot rejected the private-app token ({status}): {detail}. \
                             Check the token's value and that the app has the CRM scopes \
                             for the configured objects."
                        )));
                    }

                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
                    {
                        let retry_after = response
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .and_then(parse_retry_after);
                        retry_after.unwrap_or_else(|| backoff(attempt))
                    } else {
                        let detail = response.text().await.unwrap_or_default();
                        return Err(CdcError::Backend(anyhow::anyhow!(
                            "HubSpot returned {status} for {url}: {detail}"
                        )));
                    }
                }
                Err(e) => {
                    // A transport failure is indistinguishable from a restart
                    // of the far end, so it gets the same budget as a 429
                    // rather than killing a connector over one dropped socket.
                    if attempt + 1 >= MAX_RETRY_ATTEMPTS {
                        return Err(CdcError::Backend(anyhow::anyhow!(
                            "HubSpot request to {url} failed {MAX_RETRY_ATTEMPTS} times; \
                             last error: {e}"
                        )));
                    }
                    backoff(attempt)
                }
            };

            attempt += 1;
            if attempt >= MAX_RETRY_ATTEMPTS {
                // Loud, and an error rather than an end of stream. See the
                // module docs: `None` here would be read as "the source is
                // finished", the offset would be committed and the process
                // would exit zero while the portal kept changing.
                return Err(CdcError::Backend(anyhow::anyhow!(
                    "HubSpot has rate-limited or failed {MAX_RETRY_ATTEMPTS} consecutive \
                     requests to {url}. The connector is stopping rather than idling, \
                     because a stalled poller is indistinguishable from a healthy one. \
                     Raise the app's request limit, lengthen `poll_interval_ms`, or \
                     capture fewer object types."
                )));
            }
            eprintln!(
                "[merkql-connect hubspot] rate limited or unavailable ({attempt}/\
                 {MAX_RETRY_ATTEMPTS}); retrying {url} in {:?}",
                wait
            );
            tokio::time::sleep(wait).await;
        }
    }
}

/// `Retry-After` is seconds in every HubSpot response, but the HTTP spec also
/// allows an HTTP-date. Only the numeric form is honoured; an unparseable
/// header falls back to the backoff schedule rather than to zero, because a
/// zero wait against a rate limiter is a hot loop.
fn parse_retry_after(header: &str) -> Option<Duration> {
    header
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs.clamp(1, RETRY_CAP.as_secs())))
}

fn backoff(attempt: u32) -> Duration {
    let secs = RETRY_BASE.as_secs().saturating_mul(1u64 << attempt.min(6));
    Duration::from_secs(secs.min(RETRY_CAP.as_secs()))
}

/// HubSpot returns datetimes as epoch-millis strings on some endpoints and
/// ISO-8601 on others, and as a JSON number on none of them — but accepting a
/// number costs nothing and a rejected timestamp stops the connector.
fn parse_timestamp(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            if let Ok(ms) = s.parse::<i64>() {
                return Some(ms);
            }
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.timestamp_millis())
        }
        _ => None,
    }
}

// ── the source ───────────────────────────────────────────────────────────

struct Inner {
    http: Http,
    settings: Settings,
}

impl Inner {
    /// Build the envelope for one search result. This is the whole of the
    /// ingress mapping; everything downstream sees only what this produces.
    fn envelope(&self, object_type: &str, result: &SearchResult, ts_ms: i64) -> Envelope {
        let mut payload = result.properties.clone();
        payload.insert(
            "_source".to_string(),
            json!({
                "connector": CONNECTOR,
                "object_type": object_type,
                "object_id": result.id,
                "lastmodified_ms": ts_ms,
            }),
        );

        Envelope {
            // Namespaced by object type — see the module docs. Without this,
            // contact 512 and deal 512 become versions of one meshql record.
            id: format!("{object_type}:{}", result.id),
            payload,
            // HubSpot's modification time, not ours, so re-delivery is
            // idempotent under meshql's latest-version-wins read.
            created_at: chrono::DateTime::from_timestamp_millis(ts_ms).unwrap_or_default(),
            // Search never returns archived objects unless asked, so this is
            // normally false. Mapped anyway: if HubSpot ever does hand one
            // over, dropping the flag would publish a live-looking record for
            // something the CRM considers gone.
            deleted: result.archived,
            authorized_tokens: self.settings.authorized_tokens.clone(),
        }
    }

    fn record(
        &self,
        object_type: &str,
        result: &SearchResult,
        ts_ms: i64,
        op: Op,
        snapshot: Snapshot,
        position: Option<String>,
    ) -> ChangeRecord {
        ChangeRecord::new(
            op,
            self.envelope(object_type, result, ts_ms),
            SourceInfo {
                connector: CONNECTOR.to_string(),
                entity: self.settings.entity.clone(),
                ts_ms,
                position,
                snapshot,
            },
        )
    }
}

/// A HubSpot portal, polled incrementally.
///
/// Deliberately not `Debug`: it holds the private-app token, and a derived
/// `Debug` is how a secret ends up in a panic message or a log line.
pub struct HubspotSource {
    inner: Arc<Inner>,
}

impl HubspotSource {
    /// Build a source from the connector config, resolving the token from the
    /// environment named by `token_env`.
    pub fn from_config(source: &SourceConfig) -> Result<Self, CdcError> {
        let SourceConfig::Hubspot {
            objects,
            entity,
            properties,
            authorized_tokens,
            base_url,
            token_env,
            poll_interval_ms,
            page_size,
            index_lag_ms,
        } = source
        else {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "HubspotSource::from_config was handed a {source:?}"
            )));
        };

        let settings = Settings {
            base_url: base_url.clone(),
            objects: objects.clone(),
            entity: entity.clone(),
            authorized_tokens: authorized_tokens.clone(),
            properties: properties.clone(),
            page_size: *page_size,
            poll_interval: Duration::from_millis(*poll_interval_ms),
            index_lag: Duration::from_millis(*index_lag_ms),
            search_window: SEARCH_RESULT_WINDOW,
        };
        Self::with_token(settings, token_from_env(token_env)?)
    }

    /// The constructor [`Self::from_config`] delegates to once the secret is
    /// resolved, and the one the offline tests use.
    pub fn with_token(settings: Settings, token: String) -> Result<Self, CdcError> {
        if settings.objects.is_empty() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "a HubSpot source must name at least one CRM object type to capture \
                 (contacts, companies, deals, tickets, or a custom object)"
            )));
        }
        if let Some(duplicate) = first_duplicate(&settings.objects) {
            // Two passes over one type share a cursor entry and would each
            // filter the other's records out by id, halving throughput for no
            // reason and making the offset file's meaning ambiguous.
            return Err(CdcError::Backend(anyhow::anyhow!(
                "object type '{duplicate}' is listed twice in the HubSpot source's `objects`"
            )));
        }
        if settings.page_size == 0 || settings.page_size > MAX_PAGE_SIZE {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "page_size must be between 1 and {MAX_PAGE_SIZE}; HubSpot's search endpoint \
                 refuses anything larger"
            )));
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("building an HTTP client: {e}")))?;

        Ok(Self {
            inner: Arc::new(Inner {
                http: Http {
                    client,
                    base_url: settings.base_url.clone(),
                    token,
                },
                settings,
            }),
        })
    }
}

fn first_duplicate(objects: &[String]) -> Option<&String> {
    objects
        .iter()
        .enumerate()
        .find(|(i, o)| objects[..*i].contains(o))
        .map(|(_, o)| o)
}

/// Where the feed is in the snapshot-then-stream sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Draining the backlog that existed before the connector started. Records
    /// are `op: r`.
    Snapshot,
    /// Everything since. Records are `op: c`.
    Live,
}

/// What to do once the records from the last page have all been delivered.
///
/// Decided when a page is read, applied at the *start* of the next fetch —
/// which, because [`Feed::step`] only fetches on an empty buffer, is the first
/// moment the previous page is known to be fully delivered. Applying it
/// immediately would end the snapshot phase while its final records were still
/// sitting in the buffer, and none of them would ever be marked
/// [`Snapshot::Last`].
#[derive(Debug, Clone)]
enum NextStep {
    /// Follow HubSpot's paging token.
    Page(String),
    /// The result window is spent: re-issue the query from the advanced
    /// watermark rather than from a deeper offset.
    RestartPass,
    /// This object type is caught up.
    NextType,
}

struct Feed {
    inner: Arc<Inner>,
    cursor: Cursor,
    phase: Phase,
    /// Which object type the current pass is over.
    type_ix: usize,
    /// What the previous page decided; see [`NextStep`].
    next_step: Option<NextStep>,
    /// HubSpot's paging token within the current pass.
    after: Option<String>,
    /// Results consumed since this pass's query was last issued from scratch.
    /// Compared against `search_window` to know when to page by watermark.
    consumed_in_pass: u32,
    /// The watermark the current pass started from. If a pass exhausts the
    /// result window without moving this, paging cannot make progress.
    pass_start_ms: i64,
    buffer: VecDeque<ChangeRecord>,
    /// One record of lookahead, held back during the snapshot so its final
    /// record can be marked [`Snapshot::Last`].
    ///
    /// [`Snapshot::Last`] is the first snapshot position that is safe to resume
    /// a *stream* from, and a connector that never emits one re-snapshots the
    /// whole portal on every restart. Knowing which record is last requires
    /// knowing there is no next one, hence the lookahead. It costs one record
    /// of latency, which during a backlog drain is nothing, and it is dropped
    /// entirely once the phase is [`Phase::Live`].
    held: Option<ChangeRecord>,
    /// Did this round over all object types emit anything? If not, the feed
    /// sleeps before the next one instead of hammering the rate limit.
    produced_in_round: bool,
}

impl Feed {
    fn object_type(&self) -> &str {
        &self.inner.settings.objects[self.type_ix]
    }

    fn lag_ms(&self) -> i64 {
        self.inner.settings.index_lag.as_millis() as i64
    }

    /// Produce the next record, fetching and sleeping as needed. Never returns
    /// `None`: a poller has nothing to say only *yet*.
    async fn step(&mut self) -> Result<ChangeRecord, CdcError> {
        loop {
            if let Some(record) = self.buffer.pop_front() {
                match self.phase {
                    Phase::Live => return Ok(record),
                    Phase::Snapshot => {
                        // Hold this one back and release the previous, which we
                        // now know is not the snapshot's last.
                        if let Some(mut previous) = self.held.replace(record) {
                            previous.source.snapshot = Snapshot::True;
                            // Only the final snapshot record carries a
                            // position. An earlier one names a point *inside*
                            // the snapshot, and resuming a live stream there
                            // would begin it somewhere the stream never
                            // reached.
                            previous.source.position = None;
                            return Ok(previous);
                        }
                        continue;
                    }
                }
            }
            self.fetch().await?;
        }
    }

    /// Apply the transition the previous page decided on.
    ///
    /// Called only with an empty buffer, which is what makes the snapshot's
    /// final record identifiable: everything before it has been delivered, and
    /// `held` is holding it.
    async fn apply_next_step(&mut self) -> Result<(), CdcError> {
        let Some(step) = self.next_step.take() else {
            return Ok(());
        };
        match step {
            NextStep::Page(after) => {
                self.after = Some(after);
                return Ok(());
            }
            NextStep::RestartPass => {
                let object_type = self.object_type().to_string();
                let now = self.cursor.watermark(&object_type);
                if now <= self.pass_start_ms {
                    let window = self.inner.settings.search_window;
                    return Err(CdcError::Backend(anyhow::anyhow!(
                        "{object_type}: the search result window ({window}) filled without the \
                         watermark moving past {} — more than a window's worth of records share \
                         timestamps inside the {}ms index-lag window. Paging deeper is refused \
                         by HubSpot and advancing the watermark would skip the remainder, so \
                         the connector is stopping rather than losing them. Lower \
                         `index_lag_ms` (a historical backfill is already fully indexed, so 0 \
                         is safe for one), or narrow the capture.",
                        self.pass_start_ms,
                        self.lag_ms(),
                    )));
                }
                self.after = None;
                self.consumed_in_pass = 0;
                self.pass_start_ms = now;
                return Ok(());
            }
            NextStep::NextType => {}
        }

        self.after = None;
        self.consumed_in_pass = 0;
        self.type_ix += 1;

        if self.type_ix >= self.inner.settings.objects.len() {
            self.type_ix = 0;
            if self.phase == Phase::Snapshot {
                // Every configured type's backlog has been drained and
                // delivered: the snapshot is over. Release the held record as
                // its last, carrying the position the live stream resumes from.
                self.phase = Phase::Live;
                if let Some(mut last) = self.held.take() {
                    last.source.snapshot = Snapshot::Last;
                    self.buffer.push_back(last);
                }
            } else if !self.produced_in_round {
                // Nothing anywhere in the portal changed. Yielding to the clock
                // here is what makes this a poller; it is also the only place
                // the request budget is protected from an idle portal.
                tokio::time::sleep(self.inner.settings.poll_interval).await;
            }
            self.produced_in_round = false;
        }
        self.pass_start_ms = self.cursor.watermark(self.object_type());
        Ok(())
    }

    /// Issue the next request and buffer whatever it yields.
    async fn fetch(&mut self) -> Result<(), CdcError> {
        self.apply_next_step().await?;
        if !self.buffer.is_empty() {
            // The snapshot just ended and pushed its final record.
            return Ok(());
        }

        let object_type = self.object_type().to_string();
        let since = self.cursor.watermark(&object_type);
        let page = self
            .inner
            .http
            .search(
                &object_type,
                since,
                self.after.as_deref(),
                &self.inner.settings.properties,
                self.inner.settings.page_size,
            )
            .await?;

        let count = page.results.len() as u32;
        for result in &page.results {
            let ts = Http::watermark_of(result, &object_type)?;
            let lag = self.lag_ms();
            if !self
                .cursor
                .entry(&object_type)
                .accept(&result.id, ts, lag)?
            {
                continue;
            }
            let (op, snapshot) = match self.phase {
                Phase::Snapshot => (Op::Read, Snapshot::True),
                Phase::Live => (Op::Create, Snapshot::False),
            };
            // The position is the cursor *including* this record, so a crash
            // after the append and the commit resumes without re-delivering it
            // and without skipping the ones beside it in the same millisecond.
            let position = Some(self.cursor.encode());
            self.buffer.push_back(self.inner.record(
                &object_type,
                result,
                ts,
                op,
                snapshot,
                position,
            ));
            self.produced_in_round = true;
        }
        self.consumed_in_pass = self.consumed_in_pass.saturating_add(count);

        // An empty page ends the pass whatever the paging token says. HubSpot
        // should not hand back a token with no results, but if it does,
        // following it loops forever fetching nothing; ending the pass instead
        // costs one re-query from the same watermark, which the `>=` filter
        // makes lossless.
        self.next_step = match page.next_after {
            _ if page.results.is_empty() => Some(NextStep::NextType),
            None => Some(NextStep::NextType),
            Some(after)
                if self
                    .consumed_in_pass
                    .saturating_add(self.inner.settings.page_size)
                    <= self.inner.settings.search_window =>
            {
                Some(NextStep::Page(after))
            }
            // The result window is spent. Page by advancing the watermark
            // instead of by a deeper offset — the whole reason results are
            // sorted ascending.
            Some(_) => Some(NextStep::RestartPass),
        };
        Ok(())
    }
}

#[async_trait]
impl CommitSource for HubspotSource {
    fn connector(&self) -> &'static str {
        CONNECTOR
    }

    fn entity(&self) -> &str {
        &self.inner.settings.entity
    }

    async fn changes(&self, from: Resume, mode: SnapshotMode) -> Result<ChangeStream, CdcError> {
        let mut cursor = match &from {
            Resume::At(position) => Cursor::decode(position)?,
            Resume::Cold => Cursor::default(),
        };

        // A type present in the config but absent from a stored cursor starts
        // at watermark 0 and backfills. That is the only choice that cannot
        // gap: an object type added to a running connector has history the
        // topic has never seen. It arrives as `op: c` rather than `op: r`,
        // because the snapshot phase belongs to a cold start and this is not
        // one. Types in the cursor but no longer configured are left untouched
        // so that re-adding one resumes rather than re-backfills.

        let phase = if matches!(from, Resume::Cold) && mode.snapshots_on_cold_start() {
            Phase::Snapshot
        } else {
            Phase::Live
        };

        // `snapshot_mode = never` on a cold start means "begin at the live
        // position". That position is HubSpot's newest record, read from
        // HubSpot — never `Utc::now()`, which would be this host's clock
        // deciding what HubSpot has already emitted.
        if matches!(from, Resume::Cold) && !mode.snapshots_on_cold_start() {
            for object_type in &self.inner.settings.objects {
                if let Some((id, ts)) = self.inner.http.newest_timestamp(object_type).await? {
                    let lag = self.inner.settings.index_lag.as_millis() as i64;
                    cursor.entry(object_type).accept(&id, ts, lag)?;
                }
            }
        }

        let pass_start_ms = cursor.watermark(&self.inner.settings.objects[0]);
        let feed = Feed {
            inner: self.inner.clone(),
            cursor,
            phase,
            type_ix: 0,
            next_step: None,
            after: None,
            consumed_in_pass: 0,
            pass_start_ms,
            buffer: VecDeque::new(),
            held: None,
            produced_in_round: false,
        };

        Ok(Box::pin(stream::unfold(feed, |mut feed| async move {
            let item = feed.step().await;
            Some((item, feed))
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(id: &str, ts: &str) -> SearchResult {
        let mut properties = Stash::new();
        properties.insert("hs_lastmodifieddate".to_string(), json!(ts));
        properties.insert("dealname".to_string(), json!("a name"));
        SearchResult {
            id: id.to_string(),
            properties,
            updated_at: None,
            archived: false,
        }
    }

    fn inner(objects: &[&str]) -> Inner {
        Inner {
            http: Http {
                client: reqwest::Client::new(),
                base_url: "http://127.0.0.1:1".to_string(),
                token: "t".to_string(),
            },
            settings: Settings {
                objects: objects.iter().map(|o| o.to_string()).collect(),
                entity: "crm".to_string(),
                authorized_tokens: vec!["farm".to_string()],
                ..Settings::default()
            },
        }
    }

    // ── the ingress mapping ─────────────────────────────────────────────

    #[test]
    fn a_hubspot_object_becomes_an_envelope_carrying_its_own_provenance() {
        let inner = inner(&["deals"]);
        let envelope = inner.envelope("deals", &result("512", "1751892345123"), 1751892345123);

        assert_eq!(envelope.id, "deals:512");
        assert_eq!(envelope.payload["dealname"], json!("a name"));
        assert_eq!(envelope.authorized_tokens, vec!["farm".to_string()]);
        assert!(!envelope.deleted);

        // `created_at` is HubSpot's modification time. If it were `Utc::now()`,
        // a re-snapshot would mint a newer version than a live record already
        // on the topic and resurrect stale CRM state as the current one.
        assert_eq!(envelope.created_at.timestamp_millis(), 1751892345123);
    }

    /// The Debezium `source` block does not survive a repository append, so the
    /// object type and the modification time must be inside the payload or the
    /// domain cannot see them at all.
    #[test]
    fn the_payload_carries_the_source_block_the_envelope_would_otherwise_lose() {
        let inner = inner(&["contacts"]);
        let envelope = inner.envelope("contacts", &result("7", "1751892345123"), 1751892345123);
        let source = &envelope.payload["_source"];

        assert_eq!(source["connector"], json!("hubspot"));
        assert_eq!(source["object_type"], json!("contacts"));
        // The un-prefixed id, so a projection can link back to HubSpot without
        // reverse-engineering our composite id.
        assert_eq!(source["object_id"], json!("7"));
        // A NUMBER, not HubSpot's ISO-8601 string: nothing downstream can
        // compare those with `<`.
        assert_eq!(source["lastmodified_ms"], json!(1751892345123i64));
    }

    /// HubSpot ids are unique per object type only. Without the type prefix a
    /// deal would arrive looking like a newer version of an unrelated contact,
    /// and meshql's latest-version-wins read would silently replace one with
    /// the other.
    #[test]
    fn a_contact_and_a_deal_sharing_a_numeric_id_do_not_collide() {
        let inner = inner(&["contacts", "deals"]);
        let contact = inner.envelope("contacts", &result("512", "1"), 1);
        let deal = inner.envelope("deals", &result("512", "1"), 1);
        assert_ne!(contact.id, deal.id);
        assert_eq!(contact.id, "contacts:512");
        assert_eq!(deal.id, "deals:512");
    }

    #[test]
    fn an_archived_object_maps_to_a_deleted_envelope() {
        let inner = inner(&["deals"]);
        let mut archived = result("1", "1");
        archived.archived = true;
        assert!(inner.envelope("deals", &archived, 1).deleted);
    }

    // ── the watermark boundary ──────────────────────────────────────────

    /// The headline case. Three records share one millisecond; the connector
    /// stops after the second. Resuming must deliver the third and must not
    /// re-deliver the first two.
    ///
    /// A `>` cursor loses the third. A `>=` cursor without the id set
    /// re-delivers forever and — once enough records share the millisecond to
    /// fill a page — never advances at all.
    #[test]
    fn three_records_in_one_millisecond_survive_a_restart_at_the_boundary() {
        let t = 1751892345123i64;
        let mut cursor = Cursor::default();

        assert!(cursor.entry("deals").accept("a", t, 0).unwrap());
        assert!(cursor.entry("deals").accept("b", t, 0).unwrap());
        let position = cursor.encode();

        // The connector restarts here.
        let mut resumed = Cursor::decode(&position).unwrap();

        // The query filters `>= watermark`, so ALL THREE come back.
        assert_eq!(resumed.watermark("deals"), t);
        assert!(
            !resumed.entry("deals").accept("a", t, 0).unwrap(),
            "'a' was already emitted"
        );
        assert!(
            !resumed.entry("deals").accept("b", t, 0).unwrap(),
            "'b' was already emitted"
        );
        assert!(
            resumed.entry("deals").accept("c", t, 0).unwrap(),
            "'c' shared the boundary millisecond and MUST NOT be skipped"
        );
    }

    /// Once a later millisecond arrives the boundary set is pruned, so the
    /// cursor does not grow forever on a busy portal.
    #[test]
    fn the_boundary_set_is_pruned_once_the_watermark_moves_on() {
        let mut cursor = Cursor::default();
        cursor.entry("deals").accept("a", 100, 0).unwrap();
        cursor.entry("deals").accept("b", 100, 0).unwrap();
        assert_eq!(cursor.types["deals"].seen.len(), 2);

        cursor.entry("deals").accept("c", 200, 0).unwrap();
        assert_eq!(cursor.watermark("deals"), 200);
        assert_eq!(
            cursor.types["deals"].seen.keys().collect::<Vec<_>>(),
            vec!["c"],
            "ids below the watermark can never be re-fetched, so keeping them is pure cost"
        );
    }

    /// HubSpot's search index is eventually consistent and does not become
    /// searchable in timestamp order. Holding the watermark `index_lag` behind
    /// the newest record seen is what keeps a late-indexed record from being
    /// filtered out by the `>=` it now sits below.
    #[test]
    fn a_late_indexed_record_inside_the_lag_window_is_still_delivered() {
        let mut cursor = Cursor::default();
        let lag = 5_000;

        // A record stamped 10_000 becomes searchable first.
        assert!(cursor.entry("deals").accept("newer", 10_000, lag).unwrap());
        // The watermark trails it by the lag window rather than jumping to it.
        assert_eq!(cursor.watermark("deals"), 5_000);

        // Its neighbour, stamped earlier, is indexed afterwards. It is still
        // `>= 5000`, so the next query returns it, and it is not in `seen`.
        assert!(
            cursor.entry("deals").accept("older", 7_000, lag).unwrap(),
            "a record indexed out of order must not be lost to the watermark"
        );

        // With no lag the same record would already be below the watermark.
        let mut unlagged = Cursor::default();
        unlagged.entry("deals").accept("newer", 10_000, 0).unwrap();
        assert_eq!(unlagged.watermark("deals"), 10_000);
    }

    /// The watermark only moves forwards. A query whose tail is older than one
    /// already processed must not drag it back and re-deliver everything since.
    #[test]
    fn the_watermark_never_moves_backwards() {
        let mut cursor = Cursor::default();
        cursor.entry("deals").accept("a", 10_000, 0).unwrap();
        cursor.entry("deals").accept("b", 10_000, 3_000).unwrap();
        assert_eq!(cursor.watermark("deals"), 10_000);
    }

    /// Results are requested `>=` and sorted ascending. If one arrives below
    /// the watermark, one of those was not honoured — and silently advancing
    /// past whatever else was reordered is exactly the gap this crate forbids.
    #[test]
    fn a_result_below_the_watermark_is_an_error_not_a_silent_skip() {
        let mut cursor = Cursor::default();
        cursor.entry("deals").accept("a", 10_000, 0).unwrap();
        let err = cursor
            .entry("deals")
            .accept("b", 9_000, 0)
            .expect_err("an out-of-order result must not be swallowed");
        assert!(err.to_string().contains("sorted ascending"), "got: {err}");
    }

    /// Evicting the oldest ids costs re-delivery; raising the watermark to
    /// shrink the set would cost the records themselves.
    #[test]
    fn an_oversized_boundary_set_evicts_oldest_first_and_never_raises_the_watermark() {
        let mut cursor = TypeCursor::default();
        for i in 0..(MAX_SEEN_IDS + 10) {
            cursor.accept(&format!("id-{i}"), 1_000, 0).unwrap();
        }
        assert_eq!(cursor.seen.len(), MAX_SEEN_IDS);
        assert_eq!(
            cursor.ms, 1_000,
            "the watermark must not be raised to prune"
        );
    }

    // ── the cursor on the wire ──────────────────────────────────────────

    #[test]
    fn a_cursor_round_trips_through_the_offset_file() {
        let mut cursor = Cursor::default();
        cursor.entry("contacts").accept("1", 100, 0).unwrap();
        cursor.entry("deals").accept("2", 200, 0).unwrap();

        let decoded = Cursor::decode(&cursor.encode()).unwrap();
        assert_eq!(decoded, cursor);
        // Stable encoding: an offset file that churns on every commit for no
        // reason makes a resume unreproducible.
        assert_eq!(decoded.encode(), cursor.encode());
    }

    /// A cursor this build cannot read is an *unusable position*, not a backend
    /// error — that is what routes it through `snapshot_mode` instead of
    /// stopping the connector with no policy applied.
    #[test]
    fn an_unreadable_cursor_is_an_unusable_position() {
        for bad in ["", "not json", r#"{"v":99,"types":{}}"#, "[]"] {
            match Cursor::decode(bad) {
                Err(CdcError::UnusablePosition { .. }) => {}
                other => panic!("{bad:?} must be an UnusablePosition, got {other:?}"),
            }
        }
    }

    /// A stored cursor holding a type that is no longer captured must keep it,
    /// so that re-adding the type resumes instead of re-backfilling a portal.
    #[test]
    fn a_cursor_retains_types_that_are_no_longer_configured() {
        let mut cursor = Cursor::default();
        cursor.entry("tickets").accept("1", 500, 0).unwrap();
        let decoded = Cursor::decode(&cursor.encode()).unwrap();
        assert_eq!(decoded.watermark("tickets"), 500);
    }

    // ── HubSpot's shapes ────────────────────────────────────────────────

    /// Contacts use `lastmodifieddate`; everything else uses
    /// `hs_lastmodifieddate`. Filtering on the wrong one returns an empty
    /// result set rather than an error, so this mistake produces a connector
    /// that runs forever and delivers nothing.
    #[test]
    fn contacts_use_a_different_timestamp_property_from_every_other_object() {
        assert_eq!(timestamp_property("contacts"), "lastmodifieddate");
        for other in ["companies", "deals", "tickets", "p_custom_thing"] {
            assert_eq!(timestamp_property(other), "hs_lastmodifieddate");
        }
    }

    #[test]
    fn timestamps_parse_from_epoch_millis_and_from_iso_8601() {
        assert_eq!(
            parse_timestamp(&json!("1751892345123")),
            Some(1751892345123)
        );
        assert_eq!(
            parse_timestamp(&json!("2025-07-07T12:45:45.123Z")),
            Some(1751892345123)
        );
        assert_eq!(
            parse_timestamp(&json!(1751892345123i64)),
            Some(1751892345123)
        );
        for bad in [json!(""), json!("   "), json!("yesterday"), json!(null)] {
            assert_eq!(parse_timestamp(&bad), None, "{bad:?}");
        }
    }

    /// The watermark must come from the property the query sorts and filters
    /// on. `updatedAt` is a fallback only; a record with neither stops the
    /// connector rather than being filed under a guessed time.
    #[test]
    fn the_watermark_prefers_the_sorted_property_and_refuses_to_guess() {
        let mut with_property = result("1", "1751892345123");
        with_property.updated_at = Some("2000-01-01T00:00:00Z".to_string());
        assert_eq!(
            Http::watermark_of(&with_property, "deals").unwrap(),
            1751892345123,
            "the sorted property wins over updatedAt"
        );

        let fallback = SearchResult {
            id: "1".to_string(),
            properties: Stash::new(),
            updated_at: Some("2025-07-07T12:45:45.123Z".to_string()),
            archived: false,
        };
        assert_eq!(
            Http::watermark_of(&fallback, "deals").unwrap(),
            1751892345123
        );

        let neither = SearchResult {
            id: "1".to_string(),
            properties: Stash::new(),
            updated_at: None,
            archived: false,
        };
        assert!(Http::watermark_of(&neither, "deals").is_err());
    }

    // ── backoff ─────────────────────────────────────────────────────────

    /// `Retry-After` is what HubSpot actually knows about its own limiter, so
    /// it wins over the schedule. An unparseable header falls back to the
    /// schedule rather than to zero — a zero wait against a rate limiter is a
    /// hot loop that burns the remaining budget.
    #[test]
    fn retry_after_is_honoured_and_never_becomes_a_hot_loop() {
        assert_eq!(parse_retry_after("10"), Some(Duration::from_secs(10)));
        assert_eq!(parse_retry_after(" 3 "), Some(Duration::from_secs(3)));
        assert_eq!(
            parse_retry_after("0"),
            Some(Duration::from_secs(1)),
            "a zero wait must still yield to the limiter"
        );
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);

        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert!(backoff(30) <= RETRY_CAP, "backoff must be capped");
    }

    // ── construction ────────────────────────────────────────────────────

    /// An absent token must be a startup error naming the variable. The
    /// alternative is a `401` at the first poll, minutes after the connector
    /// has claimed the topic and printed a healthy startup line.
    #[test]
    fn an_absent_credential_names_the_variable_it_wanted() {
        let var = "HUBSPOT_TOKEN_THAT_IS_DELIBERATELY_NOT_SET_9f3a";
        let err = token_from_env(var).expect_err("an unset token must not be tolerated");
        let message = err.to_string();
        assert!(message.contains(var), "got: {message}");
        assert!(
            message.contains("private-app"),
            "the error must say what the value is: {message}"
        );
    }

    #[test]
    fn from_config_reports_a_missing_credential_rather_than_starting() {
        let source = SourceConfig::Hubspot {
            objects: vec!["contacts".to_string()],
            entity: "crm".to_string(),
            properties: vec![],
            authorized_tokens: vec![],
            base_url: "https://api.hubapi.com".to_string(),
            token_env: "HUBSPOT_TOKEN_ALSO_NOT_SET_4c71".to_string(),
            poll_interval_ms: 30_000,
            page_size: 100,
            index_lag_ms: 5_000,
        };
        let Err(err) = HubspotSource::from_config(&source) else {
            panic!("a connector with no credential must refuse to start");
        };
        assert!(
            err.to_string().contains("HUBSPOT_TOKEN_ALSO_NOT_SET_4c71"),
            "got: {err}"
        );
    }

    #[test]
    fn a_source_with_no_objects_or_a_duplicate_object_is_refused() {
        let empty = Settings {
            objects: vec![],
            ..Settings::default()
        };
        assert!(HubspotSource::with_token(empty, "t".into()).is_err());

        let duplicated = Settings {
            objects: vec!["deals".into(), "contacts".into(), "deals".into()],
            ..Settings::default()
        };
        let Err(err) = HubspotSource::with_token(duplicated, "t".into()) else {
            panic!("a duplicated object type must be refused");
        };
        assert!(err.to_string().contains("listed twice"), "got: {err}");

        let too_big = Settings {
            page_size: 500,
            ..Settings::default()
        };
        assert!(HubspotSource::with_token(too_big, "t".into()).is_err());
    }

    /// A minimal config must be a working config, and it must not be possible
    /// to put the token in it — there is no field for one.
    #[test]
    fn a_minimal_hubspot_connector_parses_with_working_defaults() {
        let config = crate::config::ConnectorConfig::from_toml(
            r#"
            topic = "crm"
            merkql_dir = "/m"
            state_dir = "/s"

            [source]
            type = "hubspot"
            objects = ["contacts", "deals"]
            entity = "crm"
        "#,
        )
        .unwrap();
        assert_eq!(config.connector_name(), "hubspot");
        assert_eq!(config.entity(), "crm");
        match config.source {
            SourceConfig::Hubspot {
                objects,
                base_url,
                token_env,
                page_size,
                index_lag_ms,
                poll_interval_ms,
                ..
            } => {
                assert_eq!(objects, vec!["contacts".to_string(), "deals".to_string()]);
                assert_eq!(base_url, "https://api.hubapi.com");
                assert_eq!(token_env, "HUBSPOT_PRIVATE_APP_TOKEN");
                assert_eq!(page_size, MAX_PAGE_SIZE);
                assert_eq!(poll_interval_ms, 30_000);
                assert_eq!(
                    index_lag_ms, 5_000,
                    "the eventual-consistency guard is on by default; a zero default \
                     would ship the gap it exists to close"
                );
            }
            other => panic!("expected a hubspot source, got {other:?}"),
        }
    }

    #[test]
    fn the_connector_and_entity_are_reported_for_the_offset_file() {
        let source = HubspotSource::with_token(
            Settings {
                entity: "crm".into(),
                ..Settings::default()
            },
            "t".into(),
        )
        .unwrap();
        assert_eq!(source.connector(), "hubspot");
        assert_eq!(source.entity(), "crm");
    }
}
