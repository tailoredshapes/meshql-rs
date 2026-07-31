//! Salesforce ingress: a `SystemModstamp` watermark poller over REST/SOQL,
//! with delete tracking, and an honest account of what it does not capture.
//!
//! # Why a poller, when Salesforce has a real change feed
//!
//! Salesforce offers three routes into change data, and they are not equally
//! available:
//!
//! - **Pub/Sub API** (gRPC, protobuf framing, Avro-encoded Change Data Capture
//!   events, server-issued replay IDs). This is the real feed: it reports
//!   creates, updates, deletes and undeletes as *transitions*, in commit order,
//!   with a resumable cursor. It is also the most expensive to reach — a gRPC
//!   transport, a generated protobuf client, an Avro decoder driven by schemas
//!   fetched at runtime, and a CDC entitlement on the org. There is no way to
//!   test any of it offline: `wiremock` fakes HTTP, not gRPC bidi streams.
//! - **CometD/Bayeux streaming** (the older `/cometd/` route). Same event
//!   model and the same replay-ID cursor, over ordinary HTTP/1.1 long polling
//!   with JSON — so unlike Pub/Sub API it *would* be reachable without gRPC
//!   and testable against a fake server. Two things rule it out here rather
//!   than one: there is no maintained Rust Bayeux client, so the handshake,
//!   subscribe and reconnect-advice state machine would be ours to write and
//!   own; and it carries the same CDC entitlement requirement as Pub/Sub API,
//!   because the entitlement is on the event stream, not the transport.
//!   Salesforce has **not** deprecated it — the official comparison page
//!   presents Streaming API and Pub/Sub API as coexisting options with no
//!   retirement date — so "it is on the way out" is not a reason to avoid it,
//!   and this module does not claim otherwise.
//! - **REST/SOQL polling on `SystemModstamp`.** No extra entitlement, ordinary
//!   HTTP, and every failure mode is reproducible against a fake server.
//!
//! This module takes the third and says so in its own record stream. The trait
//! it implements is push-shaped ([`crate::source`] explains why), and a poller
//! is a legitimate implementation of it — but it must not *pretend* to be a
//! feed. So: the stream yields on a timer, [`SalesforceSource::connector`]
//! reports `salesforce`, and the limits below are stated rather than glossed.
//!
//! ## The upgrade path, and what it would change
//!
//! Moving to Pub/Sub API replaces the window loop and nothing else. The cursor
//! is already an opaque `String`; a replay ID goes in the same slot. That is
//! why every cursor this module writes is **tagged** — `modstamp:<instant>` —
//! and why an untagged or `replay:`-tagged cursor is rejected as
//! [`CdcError::UnusablePosition`] rather than parsed on a guess. Without the
//! tag, an offset file written by this build and read by a Pub/Sub build would
//! be handed to `ReplayId` as bytes and the connector would resume from an
//! arbitrary point in the event bus. Both builds report the connector name
//! `salesforce`, so [`crate::OffsetStore`]'s connector/entity check cannot
//! catch that one; the tag is the only thing that can.
//!
//! # What this captures, and what it does not
//!
//! Captured:
//!
//! - **Creates and updates**, as the record's *current state* at the moment it
//!   was polled — not as a transition. Two updates inside one poll window
//!   collapse into one record carrying the later state. A log of states, not a
//!   log of edits.
//! - **Deletes**, via `/sobjects/{Type}/deleted/`, as tombstones (see below).
//!
//! Not captured:
//!
//! - **Intermediate states.** See above. If the domain needs every edit, it
//!   needs Pub/Sub API, and no amount of polling substitutes.
//! - **Undeletes.** A record restored from the recycle bin gets a fresh
//!   `SystemModstamp`, so it reappears as an ordinary update — which happens
//!   to be the right answer, but by luck rather than by design.
//! - **Field history.** Salesforce keeps it separately; it is a different
//!   SObject and would be a different connector.
//! - **A record whose commit lands more than `lag_seconds` after its own
//!   `SystemModstamp`.** This is the poller's one genuine gap and it is
//!   discussed under "the lag window" below.
//!
//! # The window loop, and why the target is captured first
//!
//! Each cycle:
//!
//! 1. read the **org's** clock (the `Date` header on a cheap
//!    `/services/data/` request) and subtract `lag_seconds` to get `target`;
//! 2. query the half-open window `[cursor, min(target, cursor + max_window))`;
//! 3. advance the cursor to that window's end.
//!
//! Step 1 precedes step 2 for the same reason the PostgreSQL source captures
//! `pg_current_wal_lsn()` before peeking: a record committing between the query
//! and the clock read would be excluded by the query and then skipped by the
//! cursor advance. Reversed, the mechanism that keeps the connector moving is
//! the mechanism that loses records.
//!
//! The clock is **Salesforce's, not this host's**. A connector host running a
//! few seconds fast would compute a `target` beyond what the org has committed
//! and advance its cursor past writes it never saw. Clock skew between two
//! machines is not an exotic failure; reading the org's own `Date` header
//! removes it from the design entirely.
//!
//! ## `>=` on the low bound and `<` on the high bound — exactly, not roughly
//!
//! SOQL datetime literals are second-granularity, while `SystemModstamp` values
//! carry milliseconds. `SystemModstamp >= 2026-07-30T11:22:33Z` therefore means
//! *at or after* `11:22:33.000`, and `< 2026-07-30T11:22:33Z` means *before*
//! it. Half-open windows tile the timeline with no overlap and no hole.
//!
//! Using `>` on the low bound instead would drop every record whose modstamp is
//! exactly `.000` in that second — a small, silent, permanent hole that only
//! shows up as a projection that is quietly missing rows. The half-open pairing
//! is the whole reason both bounds are stated explicitly rather than one of
//! them being left implicit.
//!
//! ## The lag window
//!
//! `lag_seconds` holds the query's upper bound back from the org's now.
//! Salesforce stamps `SystemModstamp` during a save, but the row becomes
//! visible to a subsequent query only when the transaction commits — and a
//! long-running trigger chain, a Bulk API load or a batch Apex job can put
//! seconds between the two. A window that reached all the way to *now* would
//! run past such a record before it was visible, and the cursor advance would
//! then skip it forever.
//!
//! So `lag_seconds` is the assumed worst-case stamp-to-visible delay. It is a
//! latency floor, not a tuning knob: lower it and the gap opens, raise it and
//! every record arrives later. It defaults to 30 seconds, which is generous for
//! an org doing interactive saves and thin for one running large Bulk loads.
//! **There is no value that makes the gap provably zero** — that is what
//! separates a watermark poller from a log, and the only real fix is Pub/Sub
//! API.
//!
//! ## Bounded windows, so a restart after downtime is not one enormous query
//!
//! `max_window_seconds` caps a single window. A connector down for a week
//! otherwise issues one SOQL query spanning a week, which will either time out
//! server-side or buffer an unbounded result set. Chunking also bounds *replay*:
//! only the last record of a window carries a resumable position, so a crash
//! re-delivers at most one window.
//!
//! # Positions: one per window, not one per record
//!
//! Every record in a window carries `source.position = None` except the last,
//! which carries `modstamp:<window_end>`. A record's own `SystemModstamp` is
//! **not** a resumable position: other records share its second and may not
//! have been emitted yet, and tombstones are ordered by `deletedDate` on a
//! different clock entirely. Committing a mid-window position and then crashing
//! would resume past records that were never appended.
//!
//! This is the same rule the PostgreSQL source applies when it gives every
//! record in a transaction the transaction's commit end LSN, and the same rule
//! the snapshot follows in every source here.
//!
//! # Deletes, and the tombstone's shape
//!
//! `GET /services/data/vXX.X/sobjects/{Type}/deleted/?start=&end=` enumerates
//! records deleted in a window, as `{id, deletedDate}` and nothing else. There
//! is no pre-image: Salesforce does not keep one for a caller with no prior
//! copy.
//!
//! Debezium puts the pre-image of a delete in `before` and leaves `after`
//! null. This module cannot, and would not want to: `after: null` produces a
//! record with no envelope, which [`ChangeRecord::key`] cannot key and a
//! meshql fold cannot read. Instead a delete becomes an envelope with
//! `deleted: true` — meshql's own deletion model, per [`crate::record`] — in
//! `after`, with `op: d`. A consumer reading Debezium's `op` and a consumer
//! reading meshql's `deleted` flag reach the same conclusion, and neither has
//! to know about the other. **The deviation is deliberate and is named here
//! because a consumer written against literal Debezium would look in `before`
//! and find nothing.**
//!
//! ## Delete tracking expires, and that is what makes a cursor unusable
//!
//! Salesforce keeps delete tracking for roughly 30 days. Ask
//! `/sobjects/{Type}/deleted/` for a window starting earlier and it answers
//! `INVALID_REPLICATION_DATE`. That server verdict — not a clock comparison of
//! our own — is what this module turns into [`CdcError::UnusablePosition`],
//! for the same reason the Mongo source trusts the server's rejection of a
//! resume token: the server is the only thing that knows.
//!
//! Resuming anyway would be a silent skip of a specific and nasty kind. The
//! *updates* between the stale cursor and now are still queryable, so the
//! connector would look healthy and produce records — while every delete in
//! that span was permanently invisible, leaving projections holding rows that
//! no longer exist in the org.
//!
//! With `capture_deletes = false` there is no retention limit at all: a SOQL
//! query on `SystemModstamp` reaches arbitrarily far back, so the cursor never
//! expires. That is the trade — a cursor that cannot go stale, in exchange for
//! never learning about a deletion.
//!
//! # Wrapping at ingress
//!
//! A Salesforce record is not a meshql envelope, so one is synthesised:
//!
//! - `id` — the **18-character** Salesforce Id. The 15-character form is
//!   case-*sensitive*, so two distinct records can differ only in the case of
//!   one character; anything that folds case (a spreadsheet, a case-insensitive
//!   collation, a URL matcher) merges them. merkql keys by the envelope id and
//!   a meshql fold groups by it, so an id collision would merge two aggregates
//!   permanently and undetectably. The 18-character form appends a three-
//!   character checksum encoding the capitalisation of the first fifteen, which
//!   is exactly what makes it safe under case folding. Salesforce's REST API
//!   returns 18 characters; [`to_18_char_id`] is the defensive path.
//! - `payload` — the record's fields, plus connector metadata (below).
//! - `authorized_tokens` — from configuration. Per-record authorisation
//!   derived from Salesforce sharing rules would be a second, drifting copy of
//!   an access model that lives in the org; the mesh's tokens are the mesh's
//!   business.
//!
//! ## What is materialised into the payload, and why it has to be
//!
//! The Debezium `source` block does not survive an append through a repository
//! sink — it is connector-level framing, not envelope content. Anything the
//! domain needs downstream must therefore be **in the payload**, so these keys
//! are written into it:
//!
//! | key | why |
//! | --- | --- |
//! | `_sobject` | which SObject this came from. One topic per connector today, but a fold that merges two topics has no other way to tell an `Account` from a `Contact`, and both use the same id shape. |
//! | `_systemModstamp` | the record's own replay position. Without it a consumer cannot dedupe two deliveries of the same record, order two versions of it, or tell how stale a projection is. |
//! | `_salesforceId` | the envelope id again. A projection folded from payloads alone never sees the envelope, and would otherwise hold Salesforce data with no Salesforce key. |
//! | `_org` | the org host the record came from. Salesforce Ids are unique within an org, not across orgs, so a sandbox and a production org feeding one mesh would otherwise collide silently. |
//! | `_deletedDate` | tombstones only: when the delete happened. |
//!
//! The leading underscore is not decoration. Salesforce field API names must
//! begin with a letter, so no `_`-prefixed key can ever collide with a real
//! field, and a consumer can tell connector metadata from org data by looking.
//!
//! # `durable_through` is a no-op here, deliberately
//!
//! Salesforce holds nothing back on our behalf. There is no slot to advance,
//! no cursor to acknowledge; re-issuing the same query returns the same rows.
//! So the trait's default is correct.
//!
//! The inverse hazard is worth stating, because it is the mirror image of the
//! PostgreSQL slot: precisely *because* nothing is held for us, the delete
//! tracking window runs on wall-clock time and not on our consumption. A
//! PostgreSQL connector left stopped fills a disk and is noticed; a Salesforce
//! connector left stopped for a month silently loses the ability to learn about
//! a month of deletions. [`SalesforceSource::changes`] warns when a stored
//! cursor is near the retention edge, at startup, which is when an operator can
//! still act.
//!
//! # Credentials come from the environment, never from the TOML
//!
//! The connector config names the instance URL, the API version, the SObject
//! and the OAuth *mode*. `SALESFORCE_CLIENT_ID`, `SALESFORCE_CLIENT_SECRET` and
//! (for `refresh_token`) `SALESFORCE_REFRESH_TOKEN` come from the environment.
//! A connector TOML is checked into version control and copied to hosts; an org
//! credential in one is a leak with a long tail and no revocation trail.

use crate::config::SalesforceAuth;
use crate::record::{ChangeRecord, Op, Snapshot, SourceInfo};
use crate::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use futures::stream::{self, StreamExt};
use meshql_core::{Envelope, Stash};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

const CONNECTOR: &str = "salesforce";

/// Cursor tag for a `SystemModstamp` watermark. See the module docs: an
/// untagged cursor is refused rather than guessed at.
const MODSTAMP_TAG: &str = "modstamp:";

/// Cursor tag a future Pub/Sub API implementation would use. Recognised here
/// only so that such a cursor is reported as unusable-by-this-build instead of
/// being parsed as a timestamp and failing with a confusing message.
const REPLAY_TAG: &str = "replay:";

/// Salesforce's delete-tracking retention, in days. Used only to decide when to
/// *warn*; whether a cursor is actually usable is Salesforce's verdict, not
/// this constant's.
const DELETE_TRACKING_DAYS: i64 = 30;

/// Warn when a stored cursor is within this fraction of the retention edge.
const RETENTION_WARN_FRACTION: f64 = 0.75;

/// The org's clock may legitimately differ from a record's timestamps by a
/// little; a cursor further ahead than this is not skew, it is the wrong org.
const CLOCK_SKEW_TOLERANCE_SECS: i64 = 300;

// ── credentials ──────────────────────────────────────────────────────────

/// OAuth credentials, read from the environment.
///
/// Cloned into the HTTP client rather than re-read per request: an operator
/// rotating a secret restarts the connector, and re-reading the environment
/// mid-run would let a half-rotated pair produce an authentication failure that
/// no configuration change explains.
#[derive(Clone)]
pub struct Credentials {
    client_id: String,
    client_secret: String,
    refresh_token: Option<String>,
}

/// Redacted by hand rather than derived. `CdcError::Backend` wraps an
/// `anyhow::Error`, connector errors are printed, and a derived `Debug` would
/// put an org's client secret into whatever collects this process's stderr —
/// the exact leak that keeping credentials out of the TOML was meant to
/// prevent, arriving by a different route.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("client_id", &"<redacted>")
            .field("client_secret", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl Credentials {
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        refresh_token: Option<String>,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            refresh_token,
        }
    }

    /// Read the credentials the configured `auth` mode needs.
    pub fn from_env(auth: SalesforceAuth) -> Result<Self, CdcError> {
        Self::resolve(auth, |key| std::env::var(key).ok())
    }

    /// The environment lookup, factored out so the failure can be tested
    /// without mutating the process environment — which is global state that
    /// two tests running in parallel would fight over.
    fn resolve(
        auth: SalesforceAuth,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, CdcError> {
        // An empty or whitespace-only variable is treated as absent. A shell
        // that exports `SALESFORCE_CLIENT_SECRET=` from an unset template
        // variable would otherwise sail past the check here and fail later as
        // an opaque `invalid_client` from Salesforce.
        let need = |key: &str, why: &str| {
            lookup(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    CdcError::Backend(anyhow::anyhow!(
                        "{key} is not set in the environment. merkql-connect reads Salesforce \
                         credentials from the environment and never from the connector TOML, \
                         because that file is version-controlled and copied to hosts. {why}"
                    ))
                })
        };

        let client_id = need(
            "SALESFORCE_CLIENT_ID",
            "It is the connected app's consumer key.",
        )?;
        let client_secret = need(
            "SALESFORCE_CLIENT_SECRET",
            "It is the connected app's consumer secret.",
        )?;
        let refresh_token = match auth {
            SalesforceAuth::ClientCredentials => None,
            SalesforceAuth::RefreshToken => Some(need(
                "SALESFORCE_REFRESH_TOKEN",
                "The `refresh_token` auth mode cannot obtain a session without it; \
                 use auth = \"client_credentials\" if the connected app is configured \
                 for the client-credentials flow instead.",
            )?),
        };

        Ok(Self {
            client_id,
            client_secret,
            refresh_token,
        })
    }
}

// ── the cursor ───────────────────────────────────────────────────────────

/// Parse a stored position into the instant a window should resume from.
///
/// Returns the reason it is unusable rather than a bare `None`, because every
/// caller turns that reason into [`CdcError::UnusablePosition`] and the
/// operator reading the log needs to know *which* kind of wrong cursor it was.
fn parse_cursor(position: &str) -> Result<DateTime<Utc>, String> {
    if let Some(rest) = position.strip_prefix(REPLAY_TAG) {
        return Err(format!(
            "{position:?} is a Pub/Sub API replay ID ({rest:?}), which this build cannot \
             honour: it polls SystemModstamp over REST and has no way to translate a replay \
             ID into a timestamp. Salesforce discards replay IDs after about 72 hours, so \
             there is also nothing to fall back to"
        ));
    }
    let Some(rest) = position.strip_prefix(MODSTAMP_TAG) else {
        return Err(format!(
            "{position:?} carries no recognised cursor tag. This connector writes \
             '{MODSTAMP_TAG}<RFC 3339 instant>'; an untagged value cannot be told apart from \
             a cursor written by a different Salesforce ingestion mechanism, and guessing \
             would resume from an arbitrary point"
        ));
    };
    let parsed = DateTime::parse_from_rfc3339(rest)
        .map_err(|e| format!("{position:?} is not an RFC 3339 instant: {e}"))?
        .with_timezone(&Utc);
    // Floor to the second. SOQL literals are second-granularity, so a cursor
    // carrying milliseconds would be rounded by Salesforce anyway; flooring
    // here makes the rounding *downwards* and therefore a re-delivery rather
    // than a skip.
    Ok(floor_to_second(parsed))
}

fn format_cursor(at: DateTime<Utc>) -> String {
    format!("{MODSTAMP_TAG}{}", format_instant(at))
}

fn floor_to_second(at: DateTime<Utc>) -> DateTime<Utc> {
    Utc.timestamp_opt(at.timestamp(), 0).single().unwrap_or(at)
}

/// The form both SOQL literals and the `getDeleted` query parameters take.
fn format_instant(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Salesforce returns `2026-07-30T11:22:33.000+0000` — RFC 3339 except for the
/// missing colon in the offset, which `parse_from_rfc3339` rejects. Both forms
/// are accepted so that a hand-written fixture and a real response parse
/// identically.
fn parse_salesforce_instant(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f%z")
        .or_else(|_| DateTime::parse_from_rfc3339(text))
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

// ── Salesforce Ids ───────────────────────────────────────────────────────

/// Widen a 15-character Salesforce Id to its 18-character case-safe form.
///
/// The suffix is three characters over the alphabet `A`–`Z`,`0`–`5`; each
/// encodes one five-character chunk as a bitmask whose *i*-th bit is set when
/// the chunk's *i*-th character is an uppercase letter. That is Salesforce's
/// published case-safe-Id algorithm.
///
/// This is a **defensive** path: the REST API returns 18-character Ids
/// everywhere, including from `getDeleted`. It exists because the alternative
/// on encountering a 15-character Id is to drop the record or to use the
/// 15-character form as the merkql key — and the second one silently merges
/// two records that differ only in letter case, which is the exact failure the
/// 18-character form was invented to prevent.
pub fn to_18_char_id(id: &str) -> Option<String> {
    const SUFFIX: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ012345";

    if !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    match id.len() {
        18 => Some(id.to_string()),
        15 => {
            let mut out = String::with_capacity(18);
            out.push_str(id);
            for chunk in id.as_bytes().chunks(5) {
                let mut index = 0usize;
                for (i, byte) in chunk.iter().enumerate() {
                    if byte.is_ascii_uppercase() {
                        index |= 1 << i;
                    }
                }
                out.push(SUFFIX[index] as char);
            }
            Some(out)
        }
        _ => None,
    }
}

/// SOQL is assembled by string concatenation — there is no bind-parameter form
/// — so every identifier that reaches a query is checked first. A field named
/// `Name FROM Account WHERE Id != null OR Id =` would otherwise rewrite the
/// query.
///
/// Relationship traversal (`Owner.Name`) is allowed because it is genuinely
/// useful and is still only `[A-Za-z0-9_.]`.
fn validate_field(name: &str) -> Result<(), CdcError> {
    let plausible = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    if plausible {
        Ok(())
    } else {
        Err(CdcError::Backend(anyhow::anyhow!(
            "'{name}' is not a valid Salesforce field name (letters, digits, underscore, \
             and '.' for relationship fields; must start with a letter)"
        )))
    }
}

fn validate_sobject(name: &str) -> Result<(), CdcError> {
    let plausible = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    if plausible {
        Ok(())
    } else {
        Err(CdcError::Backend(anyhow::anyhow!(
            "'{name}' is not a valid SObject name (letters, digits and underscore; \
             custom objects end in '__c')"
        )))
    }
}

fn validate_api_version(version: &str) -> Result<(), CdcError> {
    let plausible = version.starts_with('v')
        && version.len() >= 4
        && version[1..].chars().all(|c| c.is_ascii_digit() || c == '.');
    if plausible {
        Ok(())
    } else {
        Err(CdcError::Backend(anyhow::anyhow!(
            "'{version}' is not a Salesforce API version; it looks like 'v62.0'"
        )))
    }
}

// ── the HTTP client ──────────────────────────────────────────────────────

#[derive(Clone)]
struct Session {
    access_token: String,
    /// The host Salesforce told us to call, which is **not** necessarily the
    /// one in the config. The token endpoint lives on the login host (or the
    /// org's My Domain); the data API lives wherever `instance_url` in the
    /// token response points. Orgs get migrated between instances and My
    /// Domain changes are routine, so calling the configured host for data
    /// works right up until the day it returns a redirect nobody expected.
    instance_url: String,
}

struct ApiResponse {
    status: u16,
    body: Value,
    /// The org's clock, from the HTTP `Date` header.
    date: Option<DateTime<Utc>>,
}

impl ApiResponse {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Salesforce error bodies are a JSON array of
    /// `{"message": …, "errorCode": …}`. A few endpoints return a bare object.
    fn error_code(&self) -> Option<&str> {
        let first = match &self.body {
            Value::Array(items) => items.first()?,
            other => other,
        };
        first.get("errorCode")?.as_str()
    }

    fn error_message(&self) -> String {
        let first = match &self.body {
            Value::Array(items) => items.first(),
            other => Some(other),
        };
        let message = first
            .and_then(|v| v.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("no message");
        match self.error_code() {
            Some(code) => format!("HTTP {} {code}: {message}", self.status),
            None => format!("HTTP {}: {message}", self.status),
        }
    }
}

#[derive(Clone)]
struct Api {
    http: reqwest::Client,
    /// The configured login/instance host — where the token endpoint lives.
    login_url: String,
    api_version: String,
    auth: SalesforceAuth,
    credentials: Credentials,
    session: Arc<Mutex<Option<Session>>>,
}

impl Api {
    async fn authenticate(&self) -> Result<Session, CdcError> {
        let url = format!("{}/services/oauth2/token", self.login_url);
        let mut form: Vec<(&str, &str)> = vec![
            ("client_id", &self.credentials.client_id),
            ("client_secret", &self.credentials.client_secret),
        ];
        match self.auth {
            SalesforceAuth::ClientCredentials => form.push(("grant_type", "client_credentials")),
            SalesforceAuth::RefreshToken => {
                form.push(("grant_type", "refresh_token"));
                // `Credentials::resolve` guarantees this for the mode; the
                // fallback keeps the failure a Salesforce error rather than a
                // panic if the two ever drift apart.
                form.push((
                    "refresh_token",
                    self.credentials.refresh_token.as_deref().unwrap_or(""),
                ));
            }
        }

        let response = self.http.post(&url).form(&form).send().await.map_err(|e| {
            CdcError::Backend(anyhow::anyhow!("requesting a Salesforce token: {e}"))
        })?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

        if !(200..300).contains(&status) {
            // Salesforce's token endpoint uses OAuth's `error`/
            // `error_description`, not the REST API's `errorCode`/`message`.
            let error = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown_error");
            let description = body
                .get("error_description")
                .and_then(|v| v.as_str())
                .unwrap_or("no description");
            return Err(CdcError::Backend(anyhow::anyhow!(
                "Salesforce refused the {:?} grant at {url}: HTTP {status} {error} \
                 ({description}). Check SALESFORCE_CLIENT_ID / SALESFORCE_CLIENT_SECRET and \
                 that the connected app permits this flow.",
                self.auth
            )));
        }

        let access_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CdcError::Backend(anyhow::anyhow!(
                    "Salesforce token response carried no access_token"
                ))
            })?
            .to_string();
        let instance_url = body
            .get("instance_url")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.login_url)
            .trim_end_matches('/')
            .to_string();

        let session = Session {
            access_token,
            instance_url,
        };
        *self.session.lock().await = Some(session.clone());
        Ok(session)
    }

    async fn session(&self) -> Result<Session, CdcError> {
        if let Some(session) = self.session.lock().await.clone() {
            return Ok(session);
        }
        self.authenticate().await
    }

    fn absolute(&self, session: &Session, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{path}", session.instance_url)
        }
    }

    async fn send(
        &self,
        session: &Session,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<ApiResponse, CdcError> {
        let url = self.absolute(session, path);
        let response = self
            .http
            .get(&url)
            .bearer_auth(&session.access_token)
            .query(params)
            .send()
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("GET {url}: {e}")))?;

        let status = response.status().as_u16();
        let date = response
            .headers()
            .get(reqwest::header::DATE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| DateTime::parse_from_rfc2822(v).ok())
            .map(|d| d.with_timezone(&Utc));
        let text = response.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

        Ok(ApiResponse { status, body, date })
    }

    /// One authenticated GET, re-authenticating once on an expired session.
    ///
    /// Salesforce access tokens expire on the org's session-timeout policy —
    /// two hours by default — and can be revoked from Setup at any moment.
    /// Without this retry the connector runs perfectly for two hours and then
    /// reports a permanent authentication failure, which from the outside is
    /// indistinguishable from a Salesforce outage and gets escalated as one.
    ///
    /// Exactly once: a genuinely bad credential would otherwise loop, and a
    /// loop that re-authenticates on every request is how a connector gets an
    /// org's login rate limit tripped.
    async fn get(&self, path: &str, params: &[(&str, String)]) -> Result<ApiResponse, CdcError> {
        let session = self.session().await?;
        let response = self.send(&session, path, params).await?;
        if response.status == 401 {
            let session = self.authenticate().await?;
            return self.send(&session, path, params).await;
        }
        Ok(response)
    }

    fn data_path(&self, suffix: &str) -> String {
        format!("/services/data/{}{suffix}", self.api_version)
    }
}

// ── capture ──────────────────────────────────────────────────────────────

/// Everything needed to turn a window of Salesforce time into change records.
/// Cloned into the live feed so the stream owns its own copy.
#[derive(Clone)]
struct Capture {
    api: Api,
    sobject: String,
    /// Pre-joined SOQL select list, already validated.
    select: String,
    entity: String,
    authorized_tokens: Vec<String>,
    capture_deletes: bool,
}

impl Capture {
    /// The org's clock.
    ///
    /// `/services/data/` is the API-version listing: cheap, stable across every
    /// release, and served by the same edge as the data API, so its `Date`
    /// header is the clock the query will be evaluated against.
    async fn server_now(&self) -> Result<DateTime<Utc>, CdcError> {
        let response = self.api.get("/services/data/", &[]).await?;
        if !response.ok() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "reading the Salesforce server clock: {}",
                response.error_message()
            )));
        }
        response.date.ok_or_else(|| {
            // Falling back to the local clock here is exactly the skew the
            // module docs rule out of the design, so it is a failure instead.
            CdcError::Backend(anyhow::anyhow!(
                "Salesforce returned no Date header, so the org's clock is unknown. \
                 Using this host's clock instead would let skew between the two skip \
                 records, so the connector stops rather than guessing."
            ))
        })
    }

    /// Run a SOQL query to exhaustion, following `nextRecordsUrl`.
    async fn query(&self, soql: &str) -> Result<Vec<Value>, CdcError> {
        let mut records = Vec::new();
        let mut response = self
            .api
            .get(&self.api.data_path("/query"), &[("q", soql.to_string())])
            .await?;

        loop {
            if !response.ok() {
                return Err(CdcError::Backend(anyhow::anyhow!(
                    "SOQL query failed ({}): {soql}",
                    response.error_message()
                )));
            }
            if let Some(page) = response.body.get("records").and_then(|v| v.as_array()) {
                records.extend(page.iter().cloned());
            }
            // `done: false` with no `nextRecordsUrl` would silently truncate
            // the window, so treat the absence of the URL as the end only when
            // Salesforce also said it was done.
            let done = response
                .body
                .get("done")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let next = response
                .body
                .get("nextRecordsUrl")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            match (done, next) {
                (_, Some(url)) => response = self.api.get(&url, &[]).await?,
                (true, None) => return Ok(records),
                (false, None) => {
                    return Err(CdcError::Backend(anyhow::anyhow!(
                        "Salesforce reported the query incomplete but gave no nextRecordsUrl; \
                         continuing would silently drop the rest of the window"
                    )))
                }
            }
        }
    }

    /// Records deleted in `[from, to]`.
    ///
    /// The window is passed closed at both ends. The endpoint's boundary
    /// semantics are not precisely specified, and at-least-once makes the two
    /// errors wildly asymmetric: a tombstone emitted twice on a boundary is
    /// absorbed by an idempotent fold, while one dropped on a boundary leaves a
    /// row in a projection that no longer exists in the org, forever.
    async fn deleted(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<(String, DateTime<Utc>)>, CdcError> {
        let path = self
            .api
            .data_path(&format!("/sobjects/{}/deleted/", self.sobject));
        let response = self
            .api
            .get(
                &path,
                &[("start", format_instant(from)), ("end", format_instant(to))],
            )
            .await?;

        if !response.ok() {
            // The server's own verdict on the cursor. See the module docs: a
            // clock comparison of ours is a guess, this is not.
            if response.error_code() == Some("INVALID_REPLICATION_DATE") {
                return Err(CdcError::UnusablePosition {
                    connector: CONNECTOR,
                    position: format_cursor(from),
                    reason: format!(
                        "Salesforce no longer tracks deletions from {}; delete tracking is \
                         retained for about {DELETE_TRACKING_DAYS} days. Updates since then \
                         are still queryable, so resuming would produce a connector that \
                         looks healthy while every deletion in the gap stays invisible and \
                         projections keep rows the org has dropped ({})",
                        format_instant(from),
                        response.error_message()
                    ),
                });
            }
            return Err(CdcError::Backend(anyhow::anyhow!(
                "listing deleted {} records: {}",
                self.sobject,
                response.error_message()
            )));
        }

        let mut out = Vec::new();
        if let Some(items) = response
            .body
            .get("deletedRecords")
            .and_then(|v| v.as_array())
        {
            for item in items {
                let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(id) = to_18_char_id(id) else {
                    continue;
                };
                let at = item
                    .get("deletedDate")
                    .and_then(|v| v.as_str())
                    .and_then(parse_salesforce_instant)
                    .unwrap_or(to);
                out.push((id, at));
            }
        }
        // Ordered so that the records of one window are emitted in a stable
        // sequence regardless of what Salesforce returned; the topic's order
        // is the only order a downstream fold can see.
        out.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    /// One SOQL row into a change record.
    ///
    /// `None` when the row cannot produce a well-formed envelope — no Id, no
    /// modstamp. Dropping such a row is deliberate: emitting an envelope with
    /// a synthesised id would put a record on the topic that corresponds to
    /// nothing in the org and can never be superseded.
    fn record_from_row(&self, row: &Value, snapshot: Snapshot) -> Option<ChangeRecord> {
        let object = row.as_object()?;
        let id = to_18_char_id(object.get("Id")?.as_str()?)?;
        let modstamp_text = object.get("SystemModstamp")?.as_str()?.to_string();
        let modstamp = parse_salesforce_instant(&modstamp_text)?;
        let created = object
            .get("CreatedDate")
            .and_then(|v| v.as_str())
            .and_then(parse_salesforce_instant);

        let mut payload: Stash = Stash::new();
        for (key, value) in object {
            // `attributes` is Salesforce's per-row framing: the SObject type
            // and a *version-qualified* record URL. Letting it through would
            // make otherwise-identical payloads differ across an API version
            // bump, which is a diff in every downstream projection caused by a
            // config change that touched no data.
            if key == "attributes" {
                continue;
            }
            payload.insert(key.clone(), strip_attributes(value));
        }
        self.materialise(&mut payload, &id, &modstamp_text);

        let op = match (snapshot.is_snapshot(), created) {
            // Snapshot rows are `r` by the crate's contract, whatever their
            // history.
            (true, _) => Op::Read,
            // Not a heuristic: if the record has not been touched since it was
            // created, this *is* its original state, and `c` is true. If it
            // has, `u` is true. What neither can say is whether this is the
            // connector's first sighting — a record created and immediately
            // rewritten by a trigger arrives as `u` the first time it is seen.
            // Consumers should fold `c` and `u` identically; meshql's
            // append-only model does so anyway.
            (false, Some(created)) if created == modstamp => Op::Create,
            (false, _) => Op::Update,
        };

        Some(ChangeRecord::new(
            op,
            Envelope {
                id,
                payload,
                // The *version's* timestamp, not the record's CreatedDate.
                // meshql orders a result set by the resolved version's
                // created_at; using Salesforce's CreatedDate would stamp every
                // version of a record with the same instant and make their
                // order undefined.
                created_at: modstamp,
                deleted: false,
                authorized_tokens: self.authorized_tokens.clone(),
            },
            SourceInfo {
                connector: CONNECTOR.to_string(),
                entity: self.entity.clone(),
                ts_ms: modstamp.timestamp_millis(),
                position: None,
                snapshot,
            },
        ))
    }

    /// A delete, as an envelope carrying `deleted: true`. See the module docs
    /// for why the tombstone goes in `after` rather than Debezium's `before`.
    fn tombstone(&self, id: &str, at: DateTime<Utc>) -> ChangeRecord {
        let mut payload = Stash::new();
        self.materialise(&mut payload, id, &format_instant(at));
        payload.insert(
            "_deletedDate".to_string(),
            Value::String(format_instant(at)),
        );

        ChangeRecord::new(
            Op::Delete,
            Envelope {
                id: id.to_string(),
                payload,
                created_at: at,
                deleted: true,
                authorized_tokens: self.authorized_tokens.clone(),
            },
            SourceInfo {
                connector: CONNECTOR.to_string(),
                entity: self.entity.clone(),
                ts_ms: at.timestamp_millis(),
                position: None,
                snapshot: Snapshot::False,
            },
        )
    }

    /// Write the connector metadata the `source` block cannot carry downstream.
    /// See the table in the module docs for why each key is here.
    fn materialise(&self, payload: &mut Stash, id: &str, modstamp: &str) {
        payload.insert("_sobject".to_string(), Value::String(self.sobject.clone()));
        payload.insert(
            "_systemModstamp".to_string(),
            Value::String(modstamp.to_string()),
        );
        payload.insert("_salesforceId".to_string(), Value::String(id.to_string()));
        payload.insert(
            "_org".to_string(),
            Value::String(self.api.login_url.clone()),
        );
    }

    /// Every record modified in `[from, to)`, plus every record deleted in
    /// `[from, to]` when delete capture is on. Positions are left `None`; the
    /// caller stamps the last record, because only a whole window is resumable.
    async fn live_window(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ChangeRecord>, CdcError> {
        let soql = format!(
            "SELECT {} FROM {} WHERE SystemModstamp >= {} AND SystemModstamp < {} \
             ORDER BY SystemModstamp ASC, Id ASC",
            self.select,
            self.sobject,
            format_instant(from),
            format_instant(to)
        );
        let mut records: Vec<ChangeRecord> = self
            .query(&soql)
            .await?
            .iter()
            .filter_map(|row| self.record_from_row(row, Snapshot::False))
            .collect();

        if self.capture_deletes {
            for (id, at) in self.deleted(from, to).await? {
                records.push(self.tombstone(&id, at));
            }
        }
        Ok(records)
    }

    /// Every record as it stands before `to`, as `op: r`.
    ///
    /// Deletes are deliberately not enumerated: a record deleted before the
    /// snapshot is simply absent from the org, so a fold built from the
    /// snapshot never learns of it and has nothing to retract. Emitting
    /// tombstones for records the consumer has never seen would be noise that
    /// grows with the age of the org.
    async fn snapshot_rows(&self, to: DateTime<Utc>) -> Result<Vec<ChangeRecord>, CdcError> {
        let soql = format!(
            "SELECT {} FROM {} WHERE SystemModstamp < {} ORDER BY SystemModstamp ASC, Id ASC",
            self.select,
            self.sobject,
            format_instant(to)
        );
        Ok(self
            .query(&soql)
            .await?
            .iter()
            .filter_map(|row| self.record_from_row(row, Snapshot::True))
            .collect())
    }
}

/// Drop Salesforce's `attributes` framing from nested relationship objects too
/// — a `SELECT Owner.Name` returns a nested object carrying its own copy.
fn strip_attributes(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| k.as_str() != "attributes")
                .map(|(k, v)| (k.clone(), strip_attributes(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_attributes).collect()),
        other => other.clone(),
    }
}

/// Give the last record of a batch the batch's resumable position.
///
/// Every other record keeps `position: None`, so [`crate::run_connector`] never
/// commits a position that names a point inside a batch it has only partly
/// appended.
fn stamp_last(records: &mut [ChangeRecord], position: String, flag: Snapshot) {
    if let Some(last) = records.last_mut() {
        last.source.position = Some(position);
        last.source.snapshot = flag;
    }
}

// ── the source ───────────────────────────────────────────────────────────

/// Everything the connector config names for a Salesforce source, resolved.
///
/// A struct rather than a long argument list so that adding a knob does not
/// silently reorder two `String` parameters at a call site.
pub struct SalesforceOptions {
    /// The login or My Domain host, e.g. `https://acme.my.salesforce.com`.
    pub instance_url: String,
    pub api_version: String,
    pub sobject: String,
    pub fields: Vec<String>,
    pub entity: String,
    pub authorized_tokens: Vec<String>,
    pub auth: SalesforceAuth,
    pub poll_interval: Duration,
    pub lag: Duration,
    pub max_window: Duration,
    pub capture_deletes: bool,
}

pub struct SalesforceSource {
    capture: Capture,
    entity: String,
    poll_interval: Duration,
    lag: chrono::Duration,
    max_window: chrono::Duration,
}

impl SalesforceSource {
    /// Open a source, taking credentials from the environment.
    pub async fn open(options: SalesforceOptions) -> Result<Self, CdcError> {
        let credentials = Credentials::from_env(options.auth)?;
        Self::with_credentials(options, credentials).await
    }

    /// Open a source with credentials supplied directly. Used by the tests,
    /// which must not touch the process environment, and by any embedder that
    /// already has a secret in hand.
    pub async fn with_credentials(
        options: SalesforceOptions,
        credentials: Credentials,
    ) -> Result<Self, CdcError> {
        validate_sobject(&options.sobject)?;
        validate_api_version(&options.api_version)?;

        if options.fields.is_empty() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "no fields configured for SObject '{}'. SOQL has no 'SELECT *', and \
                 describing the object to select everything would change the payload shape \
                 whenever an admin adds a custom field — a breaking change to every \
                 downstream fold with nothing in version control to show for it. List the \
                 fields the mesh needs.",
                options.sobject
            )));
        }
        if options.authorized_tokens.is_empty() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "no authorized_tokens configured. An envelope with no tokens is PUBLIC to \
                 every reader of the mesh (see meshql_core::envelope_visible_to), and \
                 defaulting CRM data to public is not a default anyone should get by \
                 omission. Set the tokens the mesh uses, or [\"*\"] to mean public \
                 deliberately."
            )));
        }
        for field in &options.fields {
            validate_field(field)?;
        }

        // Id and SystemModstamp drive the cursor and the envelope id;
        // CreatedDate decides `c` versus `u`. Added rather than required, so a
        // config lists the domain fields and nothing else, and deduplicated
        // case-insensitively because SOQL is case-insensitive on field names
        // and `SELECT Id, id` is a syntax error.
        let mut select: Vec<String> = vec![
            "Id".to_string(),
            "SystemModstamp".to_string(),
            "CreatedDate".to_string(),
        ];
        for field in &options.fields {
            if !select.iter().any(|f| f.eq_ignore_ascii_case(field)) {
                select.push(field.clone());
            }
        }

        let http = reqwest::Client::builder()
            // Without a timeout a wedged edge parks the connector forever with
            // no error and no records — a stall indistinguishable from an idle
            // org.
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("building an HTTP client: {e}")))?;

        let api = Api {
            http,
            login_url: options.instance_url.trim_end_matches('/').to_string(),
            api_version: options.api_version.clone(),
            auth: options.auth,
            credentials,
            session: Arc::new(Mutex::new(None)),
        };

        // Authenticate at open, not at the first poll, for the same reason the
        // PostgreSQL source creates its slot at open: a deployment with a bad
        // secret or a connected app that forbids the flow should fail at
        // startup, in front of the operator, rather than hours later.
        api.authenticate().await?;

        let capture = Capture {
            api,
            sobject: options.sobject,
            select: select.join(", "),
            entity: options.entity.clone(),
            authorized_tokens: options.authorized_tokens,
            capture_deletes: options.capture_deletes,
        };

        Ok(Self {
            capture,
            entity: options.entity,
            poll_interval: options.poll_interval,
            lag: chrono::Duration::from_std(options.lag)
                .map_err(|e| CdcError::Backend(anyhow::anyhow!("lag_seconds out of range: {e}")))?,
            max_window: chrono::Duration::from_std(options.max_window).map_err(|e| {
                CdcError::Backend(anyhow::anyhow!("max_window_seconds out of range: {e}"))
            })?,
        })
    }

    /// Check a stored cursor against the org before using it.
    ///
    /// Two rejections, each naming a distinct disaster:
    ///
    /// - a cursor **ahead of the org's clock** means this offset file does not
    ///   belong to this org. The routine way to produce one is a sandbox
    ///   refresh: the sandbox is rebuilt from production, every record gets a
    ///   new Id, and the connector's stored watermark now describes an org that
    ///   no longer exists. Resuming would emit nothing until the sandbox's
    ///   clock caught up, looking healthy the entire time.
    /// - a cursor **older than delete tracking** — Salesforce's verdict, not
    ///   ours; see [`Capture::deleted`].
    async fn validate_cursor(&self, position: &str) -> Result<DateTime<Utc>, CdcError> {
        let at = parse_cursor(position).map_err(|reason| CdcError::UnusablePosition {
            connector: CONNECTOR,
            position: position.to_string(),
            reason,
        })?;

        let now = self.capture.server_now().await?;
        if at > now + chrono::Duration::seconds(CLOCK_SKEW_TOLERANCE_SECS) {
            return Err(CdcError::UnusablePosition {
                connector: CONNECTOR,
                position: position.to_string(),
                reason: format!(
                    "the cursor is ahead of the org's own clock ({} > {}). This offset file \
                     belongs to a different org, or the org has been refreshed from another \
                     one — after a sandbox refresh every record has a new Id and the stored \
                     watermark describes data that no longer exists",
                    format_instant(at),
                    format_instant(now)
                ),
            });
        }

        if self.capture.capture_deletes {
            // A one-second probe: the cheapest question that gets Salesforce to
            // rule on whether the cursor is inside the retention window, asked
            // at startup so the answer reaches an operator rather than a log
            // nobody reads.
            self.capture
                .deleted(at, at + chrono::Duration::seconds(1))
                .await?;

            let age = now - at;
            let limit = chrono::Duration::days(DELETE_TRACKING_DAYS);
            if age.num_seconds() as f64 > limit.num_seconds() as f64 * RETENTION_WARN_FRACTION {
                eprintln!(
                    "[merkql-connect salesforce] WARNING: the stored cursor is {} days old and \
                     Salesforce retains delete tracking for about {DELETE_TRACKING_DAYS} days. \
                     Nothing here holds that data for us the way a PostgreSQL slot would — the \
                     window runs on wall-clock time. If this connector stays behind, deletions \
                     in the gap become permanently invisible and the cursor stops being usable \
                     at all.",
                    age.num_days()
                );
            }
        }

        Ok(at)
    }
}

/// The live poller's state, owned by the stream.
struct Feed {
    capture: Capture,
    cursor: DateTime<Utc>,
    buffer: std::collections::VecDeque<ChangeRecord>,
    poll_interval: Duration,
    lag: chrono::Duration,
    max_window: chrono::Duration,
}

#[async_trait]
impl CommitSource for SalesforceSource {
    fn connector(&self) -> &'static str {
        CONNECTOR
    }

    fn entity(&self) -> &str {
        &self.entity
    }

    // `durable_through` deliberately takes the trait default. Salesforce holds
    // nothing back for us: there is no slot to advance and no acknowledgement
    // to send, and re-issuing a query returns the same rows. See the module
    // docs for the hazard that *creates* rather than removes.

    async fn changes(&self, from: Resume, mode: SnapshotMode) -> Result<ChangeStream, CdcError> {
        let mut snapshot_records: Vec<Result<ChangeRecord, CdcError>> = Vec::new();

        let cursor = match &from {
            Resume::At(position) => self.validate_cursor(position).await?,
            Resume::Cold => {
                // Capture the streaming position FIRST, then snapshot — the
                // ordering every source in this crate follows. Anything
                // written after `target` is picked up by the first window;
                // anything before it is in the snapshot; nothing falls between.
                let target = floor_to_second(self.capture.server_now().await? - self.lag);

                if mode.snapshots_on_cold_start() {
                    let mut rows = self.capture.snapshot_rows(target).await?;
                    // Only the final snapshot record is resumable, and its
                    // position is where the stream will start — not where the
                    // snapshot happened to end. An earlier one would resume the
                    // live feed at a point the snapshot had not passed.
                    stamp_last(&mut rows, format_cursor(target), Snapshot::Last);
                    snapshot_records.extend(rows.into_iter().map(Ok));
                }
                target
            }
        };

        let feed = Feed {
            capture: self.capture.clone(),
            cursor,
            buffer: std::collections::VecDeque::new(),
            poll_interval: self.poll_interval,
            lag: self.lag,
            max_window: self.max_window,
        };

        let live = stream::unfold(feed, |mut feed| async move {
            loop {
                if let Some(record) = feed.buffer.pop_front() {
                    return Some((Ok(record), feed));
                }

                // ORDER IS THE CORRECTNESS ARGUMENT: read the org's clock
                // first, then query up to it. Reversed, a record committing
                // between the query and the clock read is excluded by the
                // query and then skipped by the cursor advance below.
                let now = match feed.capture.server_now().await {
                    Ok(now) => now,
                    Err(e) => return Some((Err(e), feed)),
                };
                let target = floor_to_second(now - feed.lag);

                if target <= feed.cursor {
                    // Inside the lag window; nothing is safe to read yet.
                    tokio::time::sleep(feed.poll_interval).await;
                    continue;
                }

                let window_end = std::cmp::min(target, feed.cursor + feed.max_window);
                let mut records = match feed.capture.live_window(feed.cursor, window_end).await {
                    Ok(records) => records,
                    Err(e) => return Some((Err(e), feed)),
                };

                feed.cursor = window_end;

                if records.is_empty() {
                    // An empty window advances the cursor in memory but commits
                    // nothing: `ChangeRecord` requires an envelope, so there is
                    // no way to tell the connector "nothing happened, but the
                    // position moved". On an idle org the durable cursor
                    // therefore ages in place until delete tracking expires
                    // under it — at which point the next start reports
                    // UnusablePosition and `snapshot_mode` decides. Loud and
                    // safe, but the recovery is a full re-snapshot; see the
                    // module docs.
                    if window_end >= target {
                        tokio::time::sleep(feed.poll_interval).await;
                    }
                    continue;
                }

                stamp_last(&mut records, format_cursor(window_end), Snapshot::False);
                feed.buffer.extend(records);
            }
        });

        Ok(Box::pin(stream::iter(snapshot_records).chain(live)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── cursors ─────────────────────────────────────────────────────────

    #[test]
    fn a_modstamp_cursor_round_trips() {
        let at = Utc.with_ymd_and_hms(2026, 7, 30, 11, 22, 33).unwrap();
        let text = format_cursor(at);
        assert_eq!(text, "modstamp:2026-07-30T11:22:33Z");
        assert_eq!(parse_cursor(&text).unwrap(), at);
    }

    /// Flooring must round *down*. A cursor rounded up would exclude records in
    /// the second it names, and SOQL's second-granularity literals mean some
    /// rounding is unavoidable.
    #[test]
    fn a_sub_second_cursor_floors_rather_than_rounding_up() {
        let parsed = parse_cursor("modstamp:2026-07-30T11:22:33.987Z").unwrap();
        assert_eq!(format_instant(parsed), "2026-07-30T11:22:33Z");
    }

    /// The tag is the only thing that can stop a Pub/Sub build from reading a
    /// poller's cursor, or this build from reading a replay ID: both report the
    /// connector name `salesforce`, so the offset store's own connector check
    /// passes.
    #[test]
    fn a_replay_id_is_refused_rather_than_misread() {
        let err = parse_cursor("replay:AAAAAgAAAAAAAAA").expect_err("must not parse");
        assert!(err.contains("replay ID"), "got: {err}");
        assert!(err.contains("72 hours"), "got: {err}");
    }

    #[test]
    fn an_untagged_cursor_is_refused() {
        for bad in [
            "2026-07-30T11:22:33Z",
            "",
            "42",
            "modstamp:not-a-date",
            "MODSTAMP:2026-07-30T11:22:33Z",
        ] {
            assert!(parse_cursor(bad).is_err(), "{bad:?} must not parse");
        }
    }

    /// Salesforce writes `+0000`, which is not RFC 3339. Both spellings must
    /// land on the same instant or a fixture and a live response would disagree.
    #[test]
    fn salesforce_timestamps_parse_in_both_spellings() {
        let a = parse_salesforce_instant("2026-07-30T11:22:33.000+0000").unwrap();
        let b = parse_salesforce_instant("2026-07-30T11:22:33Z").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.timestamp_millis(), 1785410553000);
    }

    // ── Salesforce Ids ──────────────────────────────────────────────────

    /// The property that matters: the 18-character form must keep apart two Ids
    /// that the 15-character form only distinguishes by letter case. A collision
    /// here would merge two aggregates in merkql permanently.
    #[test]
    fn the_18_char_form_separates_ids_that_differ_only_in_case() {
        let upper = to_18_char_id("001D000000IqhSL").unwrap();
        let lower = to_18_char_id("001d000000iqhsl").unwrap();
        assert_ne!(upper, lower);
        assert_eq!(&upper[..15], "001D000000IqhSL");
        assert_eq!(upper.len(), 18);
        assert_eq!(lower.len(), 18);
    }

    /// Pins the checksum alphabet and the bit order: bit *i* of a chunk's mask
    /// is its *i*-th character, indexing `A–Z0–5`. All-lowercase means mask 0
    /// (`A`); all-uppercase means mask 31 (`5`).
    #[test]
    fn the_checksum_suffix_is_a_little_endian_uppercase_mask() {
        assert_eq!(
            to_18_char_id("abcdeabcdeabcde").unwrap(),
            "abcdeabcdeabcdeAAA"
        );
        assert_eq!(
            to_18_char_id("ABCDEabcdeABCDE").unwrap(),
            "ABCDEabcdeABCDE5A5"
        );
        // Only the 4th character of the first chunk is uppercase: 1 << 3 == 8,
        // and the 8th letter is 'I'.
        assert!(to_18_char_id("001D000000000000").is_none());
        assert_eq!(&to_18_char_id("001D00000000000").unwrap()[15..16], "I");
    }

    #[test]
    fn an_18_char_id_passes_through_and_junk_is_rejected() {
        assert_eq!(
            to_18_char_id("001D000000IqhSLIAY").unwrap(),
            "001D000000IqhSLIAY"
        );
        for bad in ["", "short", "001D000000IqhS!", "001D000000IqhSLIAYZ"] {
            assert!(to_18_char_id(bad).is_none(), "{bad:?} must be rejected");
        }
    }

    // ── identifiers ─────────────────────────────────────────────────────

    /// SOQL has no bind parameters, so an unchecked field name rewrites the
    /// query.
    #[test]
    fn soql_identifiers_must_be_plain() {
        for good in ["Name", "Custom_Field__c", "Owner.Name"] {
            assert!(validate_field(good).is_ok(), "{good:?}");
        }
        for bad in [
            "",
            "1Name",
            "Name FROM Account WHERE Id != null OR Id =",
            "Name'",
            "Name Owner",
        ] {
            assert!(validate_field(bad).is_err(), "{bad:?} must be rejected");
        }

        assert!(validate_sobject("Lay_Report__c").is_ok());
        for bad in ["", "Account; DROP", "2Account", "Owner.Name"] {
            assert!(validate_sobject(bad).is_err(), "{bad:?}");
        }
        assert!(validate_api_version("v62.0").is_ok());
        for bad in ["", "62.0", "vXX", "latest"] {
            assert!(validate_api_version(bad).is_err(), "{bad:?}");
        }
    }

    // ── credentials ─────────────────────────────────────────────────────

    /// A missing credential must be a clear startup error naming the variable —
    /// not an `invalid_client` from Salesforce hours later, and never a silent
    /// fallback to an anonymous session.
    #[test]
    fn missing_credentials_name_the_variable_that_is_missing() {
        let err = Credentials::resolve(SalesforceAuth::ClientCredentials, |_| None)
            .expect_err("an empty environment must not produce credentials");
        let text = err.to_string();
        assert!(text.contains("SALESFORCE_CLIENT_ID"), "got: {text}");
        assert!(
            text.contains("never from the connector TOML"),
            "got: {text}"
        );

        let err = Credentials::resolve(SalesforceAuth::ClientCredentials, |k| {
            (k == "SALESFORCE_CLIENT_ID").then(|| "id".to_string())
        })
        .expect_err("a half-set environment must not produce credentials");
        assert!(
            err.to_string().contains("SALESFORCE_CLIENT_SECRET"),
            "got: {err}"
        );

        // The refresh-token mode needs a third variable, and the error says so
        // rather than letting Salesforce reject an empty assertion.
        let err = Credentials::resolve(SalesforceAuth::RefreshToken, |k| {
            matches!(k, "SALESFORCE_CLIENT_ID" | "SALESFORCE_CLIENT_SECRET")
                .then(|| "x".to_string())
        })
        .expect_err("refresh_token mode needs a refresh token");
        assert!(
            err.to_string().contains("SALESFORCE_REFRESH_TOKEN"),
            "got: {err}"
        );
    }

    /// An exported-but-empty variable is the shape a templated deployment
    /// produces when a substitution is missing, and it must not count as set.
    #[test]
    fn an_empty_credential_counts_as_absent() {
        let err = Credentials::resolve(SalesforceAuth::ClientCredentials, |k| {
            Some(if k == "SALESFORCE_CLIENT_ID" {
                "id".to_string()
            } else {
                "   ".to_string()
            })
        })
        .expect_err("whitespace is not a secret");
        assert!(
            err.to_string().contains("SALESFORCE_CLIENT_SECRET"),
            "got: {err}"
        );
    }

    // ── payload framing ─────────────────────────────────────────────────

    /// Salesforce's `attributes` block carries a version-qualified record URL,
    /// so leaving it in would change every payload on an API version bump — a
    /// diff in every downstream projection caused by a config change that
    /// touched no data. Nested relationship objects carry their own copy.
    #[test]
    fn attributes_are_stripped_at_every_depth() {
        let row = json!({
            "attributes": {"type": "Account", "url": "/services/data/v62.0/sobjects/Account/001"},
            "Name": "Acme",
            "Owner": {
                "attributes": {"type": "User", "url": "/x"},
                "Name": "Wile E."
            },
            "Contacts": {"records": [{"attributes": {"type": "Contact"}, "Name": "Road R."}]}
        });
        let stripped = strip_attributes(&row);
        assert!(stripped.get("attributes").is_none());
        assert!(stripped["Owner"].get("attributes").is_none());
        assert!(stripped["Contacts"]["records"][0]
            .get("attributes")
            .is_none());
        assert_eq!(stripped["Owner"]["Name"], json!("Wile E."));
    }

    fn capture() -> Capture {
        Capture {
            api: Api {
                http: reqwest::Client::new(),
                login_url: "https://acme.my.salesforce.com".to_string(),
                api_version: "v62.0".to_string(),
                auth: SalesforceAuth::ClientCredentials,
                credentials: Credentials::new("id", "secret", None),
                session: Arc::new(Mutex::new(None)),
            },
            sobject: "Account".to_string(),
            select: "Id, SystemModstamp, CreatedDate, Name".to_string(),
            entity: "lay_report".to_string(),
            authorized_tokens: vec!["farm".to_string()],
            capture_deletes: true,
        }
    }

    /// The envelope mapping, end to end: the 18-char Id becomes the merkql key,
    /// the fields become the payload, the tokens come from configuration, and
    /// the four metadata keys that the `source` block cannot carry downstream
    /// are materialised into the payload itself.
    #[test]
    fn a_salesforce_row_becomes_an_envelope() {
        let row = json!({
            "attributes": {"type": "Account", "url": "/services/data/v62.0/sobjects/Account/x"},
            "Id": "001D000000IqhSLIAY",
            "SystemModstamp": "2026-07-30T11:22:33.000+0000",
            "CreatedDate": "2026-07-01T09:00:00.000+0000",
            "Name": "Acme Poultry",
            "AnnualRevenue": 42000
        });
        let record = capture()
            .record_from_row(&row, Snapshot::False)
            .expect("a well-formed row must map");

        assert_eq!(record.key().as_deref(), Some("001D000000IqhSLIAY"));
        let envelope = record.after.as_ref().unwrap();
        assert_eq!(envelope.authorized_tokens, vec!["farm".to_string()]);
        assert!(!envelope.deleted);
        // The *version's* timestamp, not the record's CreatedDate — meshql
        // orders result sets by the resolved version's created_at.
        assert_eq!(envelope.created_at.timestamp_millis(), 1785410553000);
        assert_eq!(record.source.ts_ms, 1785410553000);

        assert_eq!(envelope.payload["Name"], json!("Acme Poultry"));
        assert_eq!(envelope.payload["AnnualRevenue"], json!(42000));
        assert!(envelope.payload.get("attributes").is_none());

        // Materialised because the Debezium `source` block does not survive a
        // repository append.
        assert_eq!(envelope.payload["_sobject"], json!("Account"));
        assert_eq!(
            envelope.payload["_systemModstamp"],
            json!("2026-07-30T11:22:33.000+0000")
        );
        assert_eq!(
            envelope.payload["_salesforceId"],
            json!("001D000000IqhSLIAY")
        );
        assert_eq!(
            envelope.payload["_org"],
            json!("https://acme.my.salesforce.com")
        );

        // Modified since creation, so `u` is the truthful code.
        assert_eq!(record.op, Op::Update);
        // A window's position is stamped by the caller, never by the mapping.
        assert_eq!(record.source.position, None);
    }

    /// `c` is claimed only when the record has not been touched since it was
    /// created, which is a fact rather than a guess.
    #[test]
    fn an_untouched_record_is_a_create_and_a_snapshot_row_is_a_read() {
        let row = json!({
            "Id": "001D000000IqhSLIAY",
            "SystemModstamp": "2026-07-01T09:00:00.000+0000",
            "CreatedDate": "2026-07-01T09:00:00.000+0000"
        });
        let capture = capture();
        assert_eq!(
            capture.record_from_row(&row, Snapshot::False).unwrap().op,
            Op::Create
        );
        assert_eq!(
            capture.record_from_row(&row, Snapshot::True).unwrap().op,
            Op::Read
        );
    }

    /// A row that cannot produce a well-formed envelope is dropped rather than
    /// given a synthesised id: a record on the topic corresponding to nothing
    /// in the org can never be superseded.
    #[test]
    fn an_unmappable_row_is_dropped_rather_than_invented() {
        let capture = capture();
        for bad in [
            json!({"SystemModstamp": "2026-07-30T11:22:33.000+0000"}),
            json!({"Id": "001D000000IqhSLIAY"}),
            json!({"Id": "nope", "SystemModstamp": "2026-07-30T11:22:33.000+0000"}),
            json!({"Id": "001D000000IqhSLIAY", "SystemModstamp": "yesterday"}),
        ] {
            assert!(
                capture.record_from_row(&bad, Snapshot::False).is_none(),
                "{bad} must not map"
            );
        }
    }

    /// A delete carries BOTH signals: Debezium's `op: d` and meshql's
    /// `deleted: true`. The image is in `after`, not Debezium's `before`,
    /// because Salesforce gives no pre-image and an `after: null` record has no
    /// merkql key at all.
    #[test]
    fn a_delete_becomes_a_tombstone_readable_by_either_contract() {
        let at = Utc.with_ymd_and_hms(2026, 7, 30, 11, 22, 33).unwrap();
        let record = capture().tombstone("001D000000IqhSLIAY", at);

        assert_eq!(record.op, Op::Delete);
        assert!(record.before.is_none());
        let envelope = record.after.as_ref().expect("the tombstone is in `after`");
        assert!(envelope.deleted);
        assert_eq!(record.key().as_deref(), Some("001D000000IqhSLIAY"));
        assert_eq!(envelope.authorized_tokens, vec!["farm".to_string()]);
        assert_eq!(
            envelope.payload["_deletedDate"],
            json!("2026-07-30T11:22:33Z")
        );
        assert_eq!(envelope.payload["_sobject"], json!("Account"));
    }

    /// Only the last record of a batch is resumable. If an earlier one carried
    /// a position, a crash mid-batch would commit a point past records that
    /// were never appended.
    #[test]
    fn only_the_last_record_of_a_batch_carries_a_position() {
        let at = Utc.with_ymd_and_hms(2026, 7, 30, 11, 22, 33).unwrap();
        let capture = capture();
        let mut records = vec![
            capture.tombstone("001D000000IqhSLIAY", at),
            capture.tombstone("001D000000IqhSMIAY", at),
            capture.tombstone("001D000000IqhSNIAY", at),
        ];
        stamp_last(&mut records, format_cursor(at), Snapshot::Last);

        assert_eq!(records[0].source.position, None);
        assert_eq!(records[1].source.position, None);
        assert_eq!(
            records[2].source.position.as_deref(),
            Some("modstamp:2026-07-30T11:22:33Z")
        );
        assert_eq!(records[2].source.snapshot, Snapshot::Last);
        assert!(
            !records[2].source.snapshot.in_progress(),
            "the last snapshot record must be resumable"
        );
    }
}
