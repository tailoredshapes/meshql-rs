//! SAP **ODP** (Operational Data Provisioning) ingress over the ODP OData
//! interface: the Operational Delta Queue is a real log, and this reads it.
//!
//! # Licensing: read this before pointing it at a customer system
//!
//! **SAP Note 3255746, "Unpermitted usage of ODP Data Replication APIs", is
//! understood to restrict third-party / non-SAP consumption of ODP.** That
//! note's body requires an S-user and **has not been read by anyone who wrote
//! this module**, so nothing here asserts what it says, how broad it is, which
//! releases it applies to, or whether it is a licensing statement, a support
//! statement or an enforcement notice. Third-party summaries agree that it
//! targets the **RFC** modules of the ODP Data Replication API and that the
//! OData interface this module uses is the sanctioned alternative — but that is
//! second-hand, and it is precisely the claim that would be convenient to
//! believe.
//!
//! One directly relevant sentence *is* verifiable, from SAP's own published
//! *Operational Data Provisioning FAQ* (v1.01, 2017), Q24:
//!
//! > "The ODP data replication API is restricted to SAP applications and not
//! > open to 3rd party ETL tools."
//!
//! That predates the note by five years and does not distinguish the RFC and
//! OData channels.
//!
//! So, stated plainly and stated once: *a restriction is believed to exist, its
//! exact scope is unverified, and anyone deploying this connector against a
//! customer's SAP system should confirm their own licensing position first.*
//! This is a commercial exposure, not a technical footnote — a connector that
//! works perfectly and is not licensed to exist is still a problem, and it is
//! the kind that surfaces during an audit rather than during a test run.
//!
//! # Why ODP and not [`crate::sap`]
//!
//! [`crate::sap`] consumes an OData **delta token** from an application service,
//! and its own module docs record the finding that makes it narrow: OData v2
//! delta is a per-application capability each data provider class has to
//! implement, OData v4 delta in the ABAP framework is on-premise / private cloud
//! only, and **no stock S/4HANA A2X OData API is documented as delta-enabled.**
//! On a stock system that source has nothing to point at.
//!
//! ODP is the mechanism that does work there. It is a genuine
//! **server-maintained delta queue** — per-subscription cursors, packages,
//! recovery of already-delivered data, monitoring in transaction `ODQMON` — and
//! it is the consumption path for CDS views annotated with Change Data Capture,
//! for classic DataSource extractors, and for BW objects. It is the closest
//! thing in the SAP stack to a log, which is what this crate wants.
//!
//! This module speaks the **ODP OData** interface over HTTP, not the RFC/BW one.
//! That is deliberate: OData is testable against a fake, and every guard below
//! is exercised offline in `tests/sap_odp.rs`. An RFC client would need
//! `librfc` and a live system, so the failure modes that matter would be
//! untested — and it is also the channel Note 3255746 is reported to target.
//!
//! # The seven questions, answered — and marked where they are not
//!
//! Facts below are labelled **CONFIRMED** where SAP's own documentation states
//! them and **UNCONFIRMED** where it does not. The connector-contract skill
//! requires unknowns recorded as unknowns rather than guesses, and ODP has more
//! of them than its reputation suggests.
//!
//! 1. **Is there a real change feed?** **Yes** (CONFIRMED). The ODQ is
//!    server-maintained, resumable, and accumulates changes while the subscriber
//!    is away. What is a *poll* is the asking — ODP OData has no notification
//!    edge — so `poll_interval_ms` decides how often the delta link is followed.
//!    Same honest-poller position as [`crate::sap`], and the same reason it is
//!    not a silent degradation: the protocol's own model is "hold this token,
//!    come back later".
//! 2. **What is the cursor and when does it expire?** A delta token issued as
//!    the tail of a tracked read, spelled as the custom query option
//!    `!deltatoken='D20151001131537_000052000'` — a timestamp and a sequence
//!    (CONFIRMED, that is SAP's own example). Retention is under "Retention"
//!    below: the 24-hour figure is confirmed and the rest is not.
//! 3. **Do deletes surface?** **Yes, in the feed, as data** (CONFIRMED). The
//!    change type rides in the `ODQ_CHANGEMODE` property of an ordinary row.
//!    There is no separate delete endpoint, so there is no second and shorter
//!    retention window to fall out of step with — the permanent hole
//!    [`crate::salesforce`] documents does not exist here.
//! 4. **Result-window cap on a backfill?** None documented. Paging is
//!    server-driven and this module follows `__next` / `@odata.nextLink` to
//!    exhaustion; it never sends `$top`, which caps a *result set* rather than a
//!    page and would silently truncate an initial load.
//! 5. **Rate limits?** **UNCONFIRMED** — SAP documents none for this interface.
//!    The real resource constraint is different in kind: a paged request starts a
//!    background job on the SAP side, so `page_size` is a server workload knob.
//!    See "Paging".
//! 6. **Token lifetime, can it expire mid-backfill?** For the OAuth modes, yes.
//!    [`crate::sap_auth`] refreshes a minute before expiry from inside the
//!    stream, so a 401 never reaches the connector loop — which matters here
//!    because the loop is fatal on error and the initial load is the long part.
//! 7. **Does the source return the whole record?** It returns the ODP's
//!    projection, which is a property of the ODP definition in SAP rather than of
//!    this request: this module sends no `$select`, so a field added to the
//!    underlying CDS view or extractor appears as soon as the ODP exposes it,
//!    with no connector change to remember. The mirror hazard is real though — a
//!    field *removed* from the projection silently stops arriving, and if it was
//!    a key property that is caught, hard, by [`OdpKey::from_row`].
//!
//! # The subscription is the credentials. That is the trap.
//!
//! This is the difference between ODP and every other source in this crate.
//!
//! An ODP delta is per-subscription, and SAP identifies the subscription by
//! **the OData service plus the user the client logs on as** — SAP's words:
//! *"deltas are always returned with a reference to the user used by the OData
//! client to log on to the OData service"* (CONFIRMED). There is **no subscriber
//! parameter in the protocol**. The connector cannot name the queue it wants,
//! cannot ask which one it is reading, and cannot tell that it has been given a
//! different one.
//!
//! Two consequences, both of which fail silently:
//!
//! - **Changing the credentials changes the queue.** A read arriving as a
//!   different user is a different subscriber: SAP does not object, it performs a
//!   fresh delta initialisation — which is a **full load** — and the previous
//!   queue is left on the server with nobody collecting it. Nothing about that
//!   looks like a failure. The connector keeps running and every record on the
//!   topic is a duplicate of history.
//! - **One subscription per (service, user).** Vendor reports are consistent
//!   that a new delta initialisation *cancels* the previous one for that pair, so
//!   two connectors sharing a service and a user fight over one queue. This is
//!   the ODP form of the crate's one-process-per-entity rule and it is stronger:
//!   distinct topics and distinct `state_dir`s do not separate them. Separate
//!   consumers need separate **users**, or separate generated services.
//!
//! The only defence a connector can offer is to make the identity a thing the
//! operator wrote down. `subscriber_identity` is exactly that: a label, never
//! sent on the wire, bound into the cursor, and compared on resume. A mismatch
//! is [`CdcError::UnusablePosition`] — the same treatment [`crate::sap`] gives a
//! delta link from a different host, for the same reason. It cannot detect a
//! credential swap the operator did not declare; it can make the declared one a
//! decision with a diff attached.
//!
//! ## The subscription function imports
//!
//! An ODP OData service publishes `SubscribedTo<EntitySet>` (returns
//! `SubscribedFlag`), `TerminateDeltasFor<EntitySet>` (returns `ResultFlag`) and
//! the delta-history set `DeltaLinksOf<EntitySet>` (CONFIRMED). None of them
//! takes a subscriber parameter, and **subscription is implicit**: the first read
//! carrying `Prefer: odata.track-changes` performs the delta initialisation
//! (CONFIRMED). So this connector calls none of them.
//!
//! `TerminateDeltasFor*` in particular is never called and never will be:
//! terminating a subscription discards the server-side queue, which is
//! destructive and irreversible from here. Retiring a connector is an operator
//! action in ODQMON or an explicit call — deliberately not something a config
//! edit can do by accident.
//!
//! `DeltaLinksOf*` is the documented way to ask whether a token is still live,
//! and it is the right diagnostic to reach for when this connector reports an
//! unusable position. It is not consulted automatically: see "Expiry" below for
//! why the connector errs towards stopping.
//!
//! # Change mode is the delete signal, and JSON is safe here
//!
//! [`crate::sap`] has to parse Atom because an OData v2 delta response can only
//! spell a deletion as the Atom `deleted-entry` element, so a v2 service read as
//! JSON never observes a deletion and never says so. **That argument does not
//! apply to ODP**, and the reason is worth stating rather than inheriting:
//!
//! > ODP does not use the protocol's tombstone mechanism at all. A deletion
//! > arrives as an ordinary row whose **`ODQ_CHANGEMODE`** property names it.
//!
//! The change mode is data, so it survives JSON, and this module reads JSON.
//! `ODQ_CHANGEMODE` is a CHAR(1) with a **three-value fixed domain** (CONFIRMED,
//! from the ABAP data element): `C` created, `U` changed, `D` deleted. It is
//! *not* the BW `RECORDMODE` field and does not carry `R`, `N` or `X` — a
//! plausible-looking assumption that would have silently mapped nothing.
//!
//! What this module must never do is see a value it has not been taught and
//! carry on. An unmapped mode is a change whose *meaning* is unknown, and
//! guessing "probably an update" is how a deletion becomes an upsert. Every
//! unknown value stops the stream and names itself — see [`ChangeMode::parse`].
//!
//! meshql has no delete operation. A deletion becomes a **new envelope version
//! with `deleted: true`** (`record.rs`), delivered as an ordinary `op: c`.
//!
//! ## `ODQ_ENTITYCNTR` is a sign, not a sequence
//!
//! The other control column is a signed record counter, not an ordering key
//! (CONFIRMED): `+1` for a row that exists, `-1` for one that does not. A
//! *changed* record may therefore arrive as **two rows** — the before-image at
//! `-1` and the after-image at `+1`, both with `ODQ_CHANGEMODE = 'U'`.
//!
//! That matters because both rows share the record's key and therefore share an
//! envelope id, so they are two versions of one entity and `created_at` decides
//! which one a read sees. A before-image is by construction the *previous*
//! state; if it won, the mesh would show every changed record's old values.
//!
//! # `created_at` decides which version wins, so it is arithmetic, not a label
//!
//! `envelope_order` sorts by `created_at` with `id` as tiebreak, and two versions
//! of one record have the *same* id — so a tie is resolved arbitrarily. Three
//! cases, each stamped deliberately:
//!
//! - **A current image** (`C`, `U` at `+1`, or a full-load row) takes
//!   `changed_at_property` when configured, and the observation clock otherwise.
//!   Labelled `entity` / `observed`.
//! - **A before-image** (`U` at `-1`) takes `changed_at_property` **minus one
//!   millisecond**, so it cannot tie with the after-image of the same change —
//!   which carries the identical timestamp, because it is the same change.
//!   Labelled `superseded`.
//! - **A deletion** (`D`) takes `max(observation time, the row's own timestamp +
//!   1 ms)`. ODP hands a deletion over carrying the row's *pre-deletion*
//!   timestamp, so stamping it with that value would make every deletion tie with
//!   or lose to the version it retires — an undetectably ineffective delete. The
//!   `max` covers a SAP system whose clock runs ahead of ours. Labelled
//!   `retired`.
//!
//! Where the observation clock is used it is **monotonic**
//! ([`Inner::observe`]) and never returns one millisecond twice, so delivery
//! order is preserved exactly. That is sound *because ODP is ordered*: the queue
//! is FIFO, so arrival order is commit order, and encoding it records a fact
//! rather than inventing one. The contract's prohibition on `Utc::now()` is
//! scoped to sources with **unordered** delivery, where a stale version arrives
//! last and wins forever; that failure is not reachable from a delta queue.
//!
//! The cost is the one the contract names: a backfill of five years of history
//! gets today's timestamps unless `changed_at_property` is set. Set it when the
//! ODP exposes a last-changed field and domain time matters.
//!
//! # Retention — the numbers, and which of them are real
//!
//! ODQ is often cited as the one place in the SAP stack with documented
//! retention. Checking that turned out to be worth doing:
//!
//! | Data | Retention | Status |
//! |---|---|---|
//! | Already-retrieved (or cancelled) data, for recovery | **24 hours**, adjustable | **CONFIRMED** (SAP ODP FAQ) |
//! | Un-retrieved data | **not deleted** | **CONFIRMED** in substance |
//! | "Low relevance" data | 1 week | **UNCONFIRMED**, and probably wrong — 10 days is the figure repeatedly reported |
//! | "Medium relevance" data | 31 days | **UNCONFIRMED** — no source found |
//!
//! The low/medium figures live on the selection screen of report `ODQ_CLEANUP`
//! (reachable from ODQMON → Goto → Reorganize Delta Queues), and the daily job
//! `ODQ_CLEANUP_CLIENT_<nnn>` is created on the first delta initialisation
//! (LIKELY, not from an SAP page). **Read the variant on the target system**
//! rather than trusting a number from here.
//!
//! **24 hours is the connector's MTTR budget** for the case that actually bites:
//! a connector that has been handed data and dies before committing its position
//! has a day to come back before that package cannot be re-delivered. Deploy with
//! `snapshot_mode = "when_needed"`.
//!
//! # Expiry: an unknown token is never a silent re-baseline
//!
//! What an ODP OData service answers for an expired or unknown delta token is
//! **UNCONFIRMED** — no SAP source states the status or the message. OData
//! specifies `410 Gone` for a delta link a service no longer honours, and SAP
//! Gateway is documented to be inconsistent about it and to answer `400`/`404`/
//! `412` with an error body naming the token instead. [`classify_failure`]
//! recognises both shapes and reports [`CdcError::UnusablePosition`], which lets
//! [`SnapshotMode`] decide.
//!
//! Everything it does *not* recognise is a fatal backend error, deliberately.
//! With the real expiry response unverified, the two available errors are: stop
//! when the token was actually fine (loud, an operator sees it, nothing is lost)
//! or re-baseline when it was not (silent under `when_needed`, republishes the
//! whole ODP, and the *next* expiry is just as invisible). Stopping is the one
//! that stays honest. The diagnostic an operator reaches for is
//! `DeltaLinksOf<EntitySet>`, which lists the tokens the queue still holds.
//!
//! # Paging starts work on the SAP side
//!
//! `Prefer: odata.maxpagesize=<N>` is not a response-size hint. SAP documents it
//! plainly: *"A background job is started for paging and the data is cached in
//! the operational delta queue"* (CONFIRMED). Pages are then walked with
//! `!skiptoken` (v2's `__next`). Three consequences this module is built around:
//!
//! - `page_size` is a server workload knob and belongs in the config with that
//!   said out loud, not in a constant.
//! - **The delta token arrives only on the last page.** A cycle that stops
//!   walking never gets a cursor at all, which is why [`Feed::cycle`] follows
//!   every `__next` before it emits anything.
//! - **A restart mid-page-set does not resume** — see [`SapOdpSource::changes`].
//!
//! One live hazard, from SAP KBA 2825795: with **JSON and server-side paging**,
//! Gateway has been observed failing to return the delta token where XML works.
//! That is exactly this module's combination. It is not silent here — a cycle
//! that ends without a delta link is [`CdcError::NoFeed`] and fatal — but an
//! operator hitting it should know the KBA exists before concluding the ODP is
//! misconfigured.
//!
//! # A cycle is atomic: only its last record carries a position
//!
//! Same shape as [`crate::sap`]. A cycle follows every page and ends with one new
//! delta link covering everything it returned; there is no position naming
//! "halfway through a cycle", so every record but the last carries
//! `position: None`. A crash mid-cycle stages nothing and replays the cycle —
//! duplicates, never a gap.
//!
//! # The envelope id
//!
//! ```text
//! sap_odp:<client>:<odp>(<Name>='<Value>',…)
//! ```
//!
//! See [`OdpKey`] for why each component is there, why the encoding is
//! injective, and why **the SAP client is in it** even though it never appears in
//! an ODP OData payload.

use crate::config::{SapAuthConfig, SapODataVersion, SourceConfig};
use crate::record::{ChangeRecord, Op, Snapshot, SourceInfo};
use crate::sap_auth::{AuthedClient, SapAuth};
use crate::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use futures::stream;
use meshql_core::{Envelope, Stash};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use url::Url;

const CONNECTOR: &str = "sap_odp";

/// The reserved payload key holding everything the Debezium `source` block
/// cannot carry past a repository sink. A collision with a real ODP field is
/// refused rather than silently overwritten.
pub const ENVELOPE_META_KEY: &str = "_sap_odp";

/// ODP's change-mode column: the delete signal. Consumed and removed from the
/// business payload — it is queue bookkeeping, and a fold written against the
/// ODP's own field list should not be handed columns the ODP does not have.
pub const CHANGE_MODE_PROPERTY: &str = "ODQ_CHANGEMODE";

/// ODP's signed record counter: `+1` for a row that exists, `-1` for one that
/// does not. Consumed and removed alongside the change mode, and reported in the
/// metadata block.
pub const ENTITY_COUNTER_PROPERTY: &str = "ODQ_ENTITYCNTR";

/// OData v2's per-entry transport bookkeeping. Not business data; dropped.
const V2_METADATA_PROPERTY: &str = "__metadata";

/// The cursor's encoding version. Bumping it must come with a tolerant reader
/// for the previous shape, or every deployed connector's stored position becomes
/// a parse failure on upgrade — which surfaces as `UnusablePosition`, which
/// means `when_needed` re-baselines the whole ODP and `initial`/`never` refuse to
/// start. A deploy that looks like a data-loss incident.
const CURSOR_VERSION: u8 = 1;

// ─────────────────────────────────────────────────────────────────────────────
// Change mode
// ─────────────────────────────────────────────────────────────────────────────

/// What ODP says happened to a row.
///
/// The value set is the fixed domain of the ABAP data element `ODQ_CHANGEMODE`,
/// which has exactly three values. It is **not** BW's `RECORDMODE`, and the
/// before/after distinction it looks like it should carry is actually in
/// [`ENTITY_COUNTER_PROPERTY`]'s sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeMode {
    /// No `ODQ_CHANGEMODE` on the row. A full-load package carries no change
    /// mode, because every row in it is current state.
    Unspecified,
    /// `C` — "The data record was created as new record in the source."
    Created,
    /// `U` — "The data record was changed in the source." Note that a CDS-view
    /// CDC delta is reported to emit inserts *and* updates as `U`, so `C` may
    /// never appear at all for an `ABAP_CDS` context. Both are upserts here, so
    /// nothing depends on the distinction.
    Changed,
    /// `D` — "The data record was deleted from the source."
    Deleted,
}

impl ChangeMode {
    /// Whether this mode retires the record.
    pub fn is_deletion(&self) -> bool {
        matches!(self, ChangeMode::Deleted)
    }

    /// The wire value, as it goes into the metadata block.
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeMode::Unspecified => "unspecified",
            ChangeMode::Created => "created",
            ChangeMode::Changed => "changed",
            ChangeMode::Deleted => "deleted",
        }
    }

    /// Read `ODQ_CHANGEMODE`.
    ///
    /// # Why an unknown value is fatal
    ///
    /// This single character is the only thing distinguishing a create from a
    /// deletion. A value this connector has not been taught is a change whose
    /// *meaning* is unknown, and every available fallback is a silent lie:
    /// treating it as an upsert keeps deleted records alive forever, treating it
    /// as a deletion destroys live ones, and skipping it drops a change with
    /// nothing reporting a problem. So it stops the stream and names the value,
    /// which is the only outcome an operator can act on.
    ///
    /// An **absent or empty** mode is [`ChangeMode::Unspecified`] rather than an
    /// error: a full-load package legitimately carries no change mode, and every
    /// row in one is current state. That is a documented shape, not a guess at an
    /// unknown one.
    pub fn parse(raw: Option<&Json>) -> Result<Self, CdcError> {
        let text = match raw {
            None | Some(Json::Null) => return Ok(ChangeMode::Unspecified),
            Some(Json::String(s)) => s.trim().to_string(),
            Some(other) => {
                return Err(CdcError::Backend(anyhow::anyhow!(
                    "{CHANGE_MODE_PROPERTY} arrived as {other}, which is not a record mode. It is \
                     the only signal distinguishing a deletion from an update, so a value that \
                     cannot be read is not something to carry on past."
                )))
            }
        };
        if text.is_empty() {
            return Ok(ChangeMode::Unspecified);
        }
        match text.to_ascii_uppercase().as_str() {
            "C" => Ok(ChangeMode::Created),
            "U" => Ok(ChangeMode::Changed),
            "D" => Ok(ChangeMode::Deleted),
            other => Err(CdcError::Backend(anyhow::anyhow!(
                "{CHANGE_MODE_PROPERTY} is {other:?}. The ABAP data element {CHANGE_MODE_PROPERTY} \
                 has a three-value fixed domain — C (created), U (changed), D (deleted) — so this \
                 is either a different field wearing the same name or a value SAP added. There is \
                 no safe default: treating it as an update would keep deleted records alive \
                 forever, and skipping it would drop a change silently. Confirm the value's \
                 meaning against the domain in SE11 and add the mapping in \
                 `sap_odp::ChangeMode::parse`."
            ))),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The cursor
// ─────────────────────────────────────────────────────────────────────────────

/// The opaque position, decoded.
///
/// # Why this is a struct and not the delta token
///
/// An ODP delta token names a place in **one subscription's** queue, and ODP
/// identifies a subscription by the service and the logon user rather than by
/// anything the protocol carries. Presented against a different subscription the
/// token does not mean "somewhere else in the same feed"; SAP's answer is to
/// start a new queue rather than to complain. A cursor carrying only the token
/// would therefore let a credential change silently re-baseline the whole ODP.
///
/// Carrying the declared identity alongside the token turns that into a startup
/// decision: [`SapOdpSource::decode_cursor`] compares each component and reports
/// an unusable position naming the one that moved.
///
/// Serialised as JSON into `Resume::At`, versioned, and interpreted by nothing
/// but this module — `mongo.rs` is the precedent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Encoding version. See [`CURSOR_VERSION`].
    pub v: u8,
    /// The operator's declaration of which (service, user) pair this token was
    /// issued to. Never sent to SAP; see the module docs.
    pub subscriber_identity: String,
    pub odp: String,
    pub client: String,
    /// The server-issued delta link, verbatim.
    pub delta_link: String,
}

impl Cursor {
    /// Encode. Never fails: every field is a `String` or a `u8`.
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("a cursor of strings serialises")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The composite-key encoding
// ─────────────────────────────────────────────────────────────────────────────

/// One ODP record's identity, canonicalised.
///
/// # The shape
///
/// ```text
/// sap_odp:<client>:<odp>(<Name>='<Value>',…)
/// ```
///
/// - **`sap_odp`** — the system, so an ODP-sourced record cannot collide with
///   anything else sharing a topic.
/// - **`<client>`** — the SAP client (MANDT). This is the SAP-specific trap and
///   the reason this component exists: **MANDT is part of a record's database
///   identity and is invisible in an ODP OData payload.** The ODP is resolved
///   inside the logon client, so the connection is an implicit namespace, and a
///   connector that left the client out would produce ids unique *within*
///   whichever system it happened to be pointed at — colliding the moment a
///   second client's data reached the same topic. A QA client replicated beside
///   production, a client copy, a merged landscape. Nothing would report it: the
///   two clients' records would become versions of each other and the older one
///   would vanish from every read. The value comes from configuration because
///   the payload cannot supply it, which makes it an operator's declaration,
///   checked once, rather than a silent property of a URL.
/// - **`<odp>`** — the ODP name, so two ODPs sharing a key name cannot collide.
/// - **the key predicate** — the semantic key, sorted by property name in byte
///   order, values canonicalised to text and single-quoted with OData's own
///   doubled-quote escape.
///
/// # Why it is injective
///
/// Two separators, both provably outside the alphabets of the components that
/// precede them:
///
/// - `:` cannot appear in a SAP client (three alphanumerics) or in an ABAP object
///   name (letters, digits, `_`, and `/` in a namespace). [`OdpKey::new`]
///   *checks* that rather than trusting it, because "cannot occur" is exactly the
///   assumption that turns into a collision.
/// - Inside the predicate, property names are OData identifiers — never `=`,
///   `,`, `(`, `)` or `'` — and values are always quoted with `''` escaping, so a
///   scanner finds each closing quote unambiguously. Every structural character
///   appears unquoted only as a separator, so decoding is total and encoding is
///   injective. `("A","BC")` and `("AB","C")` cannot meet.
///
/// # Sorting by name, not by arrival order
///
/// The order properties appear in a payload, or in `key_properties`, is not
/// something to build a permanent identity on. Sorting makes the id independent
/// of both.
///
/// # Text canonicalisation
///
/// Integer `1` and string `"1"` encode identically. Within one ODP a key field
/// has one type, so those cannot be distinct records — and in exchange the id is
/// invariant across OData v2's string-typed numerics and v4's JSON numbers, which
/// is drift that does happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OdpKey {
    client: String,
    odp: String,
    /// Sorted by name. `BTreeMap` is the invariant, not an implementation detail.
    parts: BTreeMap<String, String>,
}

impl OdpKey {
    /// Build a key, refusing components that could forge a separator.
    pub fn new(client: &str, odp: &str, parts: BTreeMap<String, String>) -> Result<Self, CdcError> {
        for (what, value) in [("client", client), ("odp_name", odp)] {
            if value.is_empty() {
                return Err(CdcError::Backend(anyhow::anyhow!(
                    "source.{what} is empty, and it is a component of every envelope id"
                )));
            }
            if let Some(bad) = value.chars().find(|c| ":()',=".contains(*c)) {
                return Err(CdcError::Backend(anyhow::anyhow!(
                    "source.{what} is {value:?}, which contains {bad:?}. The envelope id joins the \
                     client and the ODP name with ':' and then a key predicate, so a component \
                     carrying a structural character could forge another record's id — two \
                     unrelated business records would become versions of each other with nothing \
                     reporting it."
                )));
            }
        }
        if parts.is_empty() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "an ODP record produced no key parts, so it has no envelope id"
            )));
        }
        Ok(Self {
            client: client.to_string(),
            odp: odp.to_string(),
            parts,
        })
    }

    /// The envelope id. Stable for the life of the record.
    pub fn envelope_id(&self) -> String {
        let body = self
            .parts
            .iter()
            .map(|(name, value)| format!("{name}='{}'", value.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        format!("{CONNECTOR}:{}:{}({body})", self.client, self.odp)
    }

    /// The key parts, for the metadata block.
    pub fn parts(&self) -> &BTreeMap<String, String> {
        &self.parts
    }

    /// Read the semantic key out of an ODP row.
    ///
    /// Unlike [`crate::sap`] there is no entity-id-URL fallback, and none is
    /// wanted: an ODP deletion is a **data row carrying the key columns**, not a
    /// tombstone naming a URL. So a missing key property is a missing key
    /// property in every case, and it is fatal — emitting the record under a
    /// partial key would merge it with every other row sharing whichever
    /// components did arrive, silently and permanently.
    pub fn from_row(
        client: &str,
        odp: &str,
        key_properties: &[String],
        row: &serde_json::Map<String, Json>,
    ) -> Result<Self, CdcError> {
        let mut parts = BTreeMap::new();
        let mut missing = Vec::new();
        for name in key_properties {
            match row.get(name) {
                Some(value) => {
                    parts.insert(name.clone(), key_text(odp, name, value)?);
                }
                None => missing.push(name.clone()),
            }
        }
        if !missing.is_empty() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "an ODP {odp} row is missing key {missing:?}, so its envelope id cannot be formed. \
                 Emitting it under a partial key would silently merge it with every other row \
                 sharing the key properties that did arrive. Check `source.key_properties` against \
                 the ODP's field list — and against the *delete* packages too, since a projection \
                 that drops a field still delivers deletions."
            )));
        }
        Self::new(client, odp, parts)
    }
}

/// Canonicalise a key value to text.
///
/// `null` is refused: a null key means the payload is not what `key_properties`
/// claims it is, and coercing it to `""` would merge every such row into one
/// aggregate.
fn key_text(odp: &str, name: &str, value: &Json) -> Result<String, CdcError> {
    match value {
        Json::String(s) => Ok(s.clone()),
        Json::Number(n) => Ok(n.to_string()),
        Json::Bool(b) => Ok(b.to_string()),
        Json::Null => Err(CdcError::Backend(anyhow::anyhow!(
            "{odp}.{name} is configured as a key property but arrived null; a key that can be null \
             is not a key, so either `key_properties` is wrong or the payload is"
        ))),
        Json::Array(_) | Json::Object(_) => Err(CdcError::Backend(anyhow::anyhow!(
            "{odp}.{name} is configured as a key property but arrived as a structured value; only \
             scalars can identify a record"
        ))),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Page parsing
// ─────────────────────────────────────────────────────────────────────────────

/// One row as the queue reported it, before it becomes an envelope.
#[derive(Debug, Clone, PartialEq)]
struct RowEvent {
    mode: ChangeMode,
    /// `ODQ_ENTITYCNTR`. `None` when the ODP does not supply it.
    counter: Option<i64>,
    row: serde_json::Map<String, Json>,
}

impl RowEvent {
    /// Whether this row is the *previous* state of a changed record.
    ///
    /// A negative counter on a non-deletion is ODP's before-image, paired with an
    /// after-image carrying the same key and the same business timestamp. It has
    /// to sort before its partner or the mesh shows the old values.
    fn is_before_image(&self) -> bool {
        !self.mode.is_deletion() && self.counter.is_some_and(|c| c < 0)
    }
}

#[derive(Debug, Default, PartialEq)]
struct Page {
    rows: Vec<RowEvent>,
    next_link: Option<String>,
    delta_link: Option<String>,
}

/// Read one ODP OData response.
///
/// The two dialects differ only in where the array and the links live, and both
/// are read exactly rather than probed: a body that is neither shape is a hard
/// error, because the alternative — an empty page and a cheerful carry-on — is a
/// gateway error page being replicated as "no changes", forever.
fn parse_page(version: SapODataVersion, body: &Json) -> Result<Page, CdcError> {
    let (rows, next_link, delta_link) = match version {
        SapODataVersion::V2 => {
            let d = body.get("d").ok_or_else(|| {
                CdcError::Backend(anyhow::anyhow!(
                    "the ODP service's OData v2 response has no `d` element. A gateway error page, \
                     an HTML login redirect or a v4 response body looks like this. Reading it as \
                     an empty page would report 'no changes' for a request that failed."
                ))
            })?;
            let rows = d.get("results").and_then(Json::as_array).ok_or_else(|| {
                CdcError::Backend(anyhow::anyhow!(
                    "the ODP service's OData v2 response has no `d.results` array"
                ))
            })?;
            (
                rows,
                d.get("__next").and_then(Json::as_str),
                d.get("__delta").and_then(Json::as_str),
            )
        }
        SapODataVersion::V4 => {
            let rows = body.get("value").and_then(Json::as_array).ok_or_else(|| {
                CdcError::Backend(anyhow::anyhow!(
                    "the ODP service's OData v4 response has no `value` array. A gateway error \
                     page or a v2 response body looks like this, and reading it as an empty page \
                     would report 'no changes' for a request that failed."
                ))
            })?;
            (
                rows,
                body.get("@odata.nextLink").and_then(Json::as_str),
                body.get("@odata.deltaLink").and_then(Json::as_str),
            )
        }
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Json::Object(row) = row else {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "an ODP delta package contains {row}, which is not an entity"
            )));
        };
        let mode = ChangeMode::parse(row.get(CHANGE_MODE_PROPERTY))?;
        let counter = row.get(ENTITY_COUNTER_PROPERTY).and_then(as_i64);
        let mut row = row.clone();
        // Queue bookkeeping, not business data. Kept out of the payload and
        // reported in the metadata block instead.
        row.remove(CHANGE_MODE_PROPERTY);
        row.remove(ENTITY_COUNTER_PROPERTY);
        row.remove(V2_METADATA_PROPERTY);
        out.push(RowEvent { mode, counter, row });
    }

    Ok(Page {
        rows: out,
        next_link: next_link.map(str::to_string),
        delta_link: delta_link.map(str::to_string),
    })
}

/// OData v2 renders a numeric as a string and v4 as a number. The entity counter
/// — whose *sign* decides whether a row is current state — has to be read
/// through both, or every v2 before-image would be silently indistinguishable
/// from an after-image.
fn as_i64(value: &Json) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

// ─────────────────────────────────────────────────────────────────────────────
// The source
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the feed needs, owned, so the returned stream is `'static`.
struct Inner {
    client: AuthedClient,
    service_root: Url,
    entity_set: String,
    odp_name: String,
    sap_client: String,
    send_sap_client: bool,
    subscriber_identity: String,
    entity: String,
    version: SapODataVersion,
    key_properties: Vec<String>,
    changed_at_property: Option<String>,
    authorized_tokens: Vec<String>,
    page_size: u32,
    poll_interval: Duration,
    /// The monotonic observation clock, epoch millis. See [`Inner::observe`].
    observed: Mutex<i64>,
}

/// Everything [`SapOdpSource::open`] needs.
///
/// A struct rather than a positional argument list: half of these are `String`s
/// that would compile in each other's places, and confusing `odp_name` with
/// `subscriber_identity` would address a different delta queue without a
/// compiler error.
#[derive(Debug, Clone)]
pub struct SapOdpOptions {
    pub service_root: String,
    pub odp_name: String,
    /// `None` means `FactsOf{odp_name}`.
    pub entity_set: Option<String>,
    pub client: String,
    pub send_sap_client: bool,
    /// The operator's declaration of the (service, user) pair. Never sent.
    pub subscriber_identity: String,
    pub odata_version: SapODataVersion,
    pub entity: String,
    pub key_properties: Vec<String>,
    pub changed_at_property: Option<String>,
    pub authorized_tokens: Vec<String>,
    pub page_size: u32,
    pub auth: SapAuthConfig,
    pub poll_interval: Duration,
}

/// One SAP Operational Delta Queue, replicated onto a topic.
pub struct SapOdpSource {
    inner: Arc<Inner>,
}

impl SapOdpSource {
    /// Open a source. Resolves credentials and builds the HTTP client here, so a
    /// misconfigured deployment fails before it claims a topic rather than an
    /// hour later on the first poll.
    pub async fn open(options: SapOdpOptions) -> Result<Self, CdcError> {
        let SapOdpOptions {
            service_root,
            odp_name,
            entity_set,
            client,
            send_sap_client,
            subscriber_identity,
            odata_version,
            entity,
            key_properties,
            changed_at_property,
            authorized_tokens,
            page_size,
            auth,
            poll_interval,
        } = options;

        if key_properties.is_empty() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "source.key_properties is empty; without it there is no envelope id. Take the \
                 ODP's semantic key from its field list — not its `ODQ_*` control columns, which \
                 describe a queue position rather than a business record."
            )));
        }
        if key_properties.iter().any(|k| k.trim().is_empty()) {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "source.key_properties contains an empty name"
            )));
        }
        if key_properties
            .iter()
            .any(|k| k == CHANGE_MODE_PROPERTY || k == ENTITY_COUNTER_PROPERTY)
        {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "source.key_properties names an ODP control column \
                 ({CHANGE_MODE_PROPERTY}/{ENTITY_COUNTER_PROPERTY}). Those describe a position in \
                 the delta queue, not a business record, so an id built from them would make every \
                 change to one record a different entity — and this connector strips them from the \
                 payload before the id is derived, so the id could not be formed at all."
            )));
        }
        if subscriber_identity.trim().is_empty() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "source.subscriber_identity is empty. ODP identifies a subscription by the OData \
                 service and the logon user and sends nothing on the wire, so this label is the \
                 only record of which queue a stored cursor belongs to. Write the pair down, e.g. \
                 \"ZODP_SO_SRV/MERKQL_CDC\"."
            )));
        }
        if page_size == 0 {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "source.page_size is 0. `Prefer: odata.maxpagesize=0` is not a request for \
                 unlimited pages; leave the default if the page size does not matter."
            )));
        }

        let parsed_root = Url::parse(&service_root).map_err(|e| {
            CdcError::Backend(anyhow::anyhow!(
                "source.service_root {service_root:?} is not a URL: {e}"
            ))
        })?;
        if parsed_root.cannot_be_a_base() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "source.service_root {parsed_root} has no path to append an entity set to"
            )));
        }

        let entity_set = entity_set.unwrap_or_else(|| format!("FactsOf{odp_name}"));

        // Build a key eagerly, so a client or ODP name that could forge a
        // separator is a startup error rather than a collision discovered later
        // by counting rows.
        OdpKey::new(
            &client,
            &odp_name,
            key_properties
                .iter()
                .map(|k| (k.clone(), String::new()))
                .collect(),
        )?;

        let authed = AuthedClient::new(SapAuth::resolve(&auth)?)?;

        Ok(Self {
            inner: Arc::new(Inner {
                client: authed,
                service_root: parsed_root,
                entity_set,
                odp_name,
                sap_client: client,
                send_sap_client,
                subscriber_identity,
                entity,
                version: odata_version,
                key_properties,
                changed_at_property,
                authorized_tokens,
                page_size,
                poll_interval,
                observed: Mutex::new(0),
            }),
        })
    }

    /// Open from a parsed config. Credentials are resolved from the environment
    /// inside `open`; nothing on this path can carry a secret out of the TOML.
    pub async fn from_config(source: &SourceConfig) -> Result<Self, CdcError> {
        let SourceConfig::SapOdp {
            service_root,
            odp_name,
            entity_set,
            client,
            send_sap_client,
            subscriber_identity,
            odata_version,
            entity,
            key_properties,
            changed_at_property,
            authorized_tokens,
            page_size,
            auth,
            poll_interval_ms,
        } = source
        else {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "SapOdpSource::from_config was handed a {source:?}"
            )));
        };

        Self::open(SapOdpOptions {
            service_root: service_root.clone(),
            odp_name: odp_name.clone(),
            entity_set: entity_set.clone(),
            client: client.clone(),
            send_sap_client: *send_sap_client,
            subscriber_identity: subscriber_identity.clone(),
            odata_version: *odata_version,
            entity: entity.clone(),
            key_properties: key_properties.clone(),
            changed_at_property: changed_at_property.clone(),
            auth: authorized_tokens.clone().into(),
            page_size: *page_size,
            auth: auth.clone(),
            poll_interval: Duration::from_millis(*poll_interval_ms),
        })
        .await
    }

    /// Decode and validate a stored position.
    ///
    /// Every rejection here is [`CdcError::UnusablePosition`] rather than a
    /// backend error, and that is the point: each one means "the cursor you kept
    /// no longer names a place in the feed you are about to read", which is
    /// exactly the condition [`SnapshotMode`] exists to arbitrate. Reporting them
    /// as backend errors would leave `when_needed` unable to recover from any of
    /// them; starting anyway would make all of them silent.
    fn decode_cursor(&self, raw: &str) -> Result<(Cursor, Url), CdcError> {
        let unusable = |reason: String| CdcError::UnusablePosition {
            connector: CONNECTOR,
            position: raw.to_string(),
            reason,
        };

        let cursor: Cursor = serde_json::from_str(raw).map_err(|e| {
            unusable(format!(
                "the stored position is not a {CONNECTOR} cursor ({e}). Either the offset file was \
                 written by a different connector, or by a build of this one using an older cursor \
                 encoding that no longer round-trips"
            ))
        })?;

        if cursor.v != CURSOR_VERSION {
            return Err(unusable(format!(
                "the stored cursor is encoding version {}, and this build writes version \
                 {CURSOR_VERSION}",
                cursor.v
            )));
        }

        // Each of these is an identity component of the delta queue. A mismatch
        // means the token names a position in a queue this connector is no
        // longer reading — and SAP's response to a read from a different
        // subscription is to perform a fresh delta initialisation rather than to
        // complain, so continuing would be a full silent re-baseline plus an
        // abandoned queue accumulating on the server.
        for (what, stored, configured) in [
            (
                "subscriber_identity",
                &cursor.subscriber_identity,
                &self.inner.subscriber_identity,
            ),
            ("odp_name", &cursor.odp, &self.inner.odp_name),
            ("client", &cursor.client, &self.inner.sap_client),
        ] {
            if stored != configured {
                return Err(unusable(format!(
                    "the stored cursor was issued for {what} {stored:?} and the config now says \
                     {configured:?}. An ODP delta belongs to one subscription, so this token names \
                     a position in a queue that is no longer the one being read; continuing would \
                     start a fresh full load without anyone deciding to, and leave the old queue \
                     on the SAP system with nobody collecting it"
                )));
            }
        }

        let url = Url::parse(&cursor.delta_link).map_err(|e| {
            unusable(format!(
                "the stored cursor's delta link is not an absolute URL ({e})"
            ))
        })?;

        // The repoint guard. A copy-back from production into QA leaves the old
        // system's delta link in the offset file, and following it would
        // replicate the old system's changes onto the new system's topic for as
        // long as the old host stayed reachable.
        if url.host_str() != self.inner.service_root.host_str()
            || url.port_or_known_default() != self.inner.service_root.port_or_known_default()
        {
            return Err(unusable(format!(
                "the stored delta link points at {}, but source.service_root is {}. The connector \
                 was repointed at a different SAP system, and one system's delta token cannot name \
                 a position in another's",
                url.host_str().unwrap_or("<no host>"),
                self.inner.service_root.host_str().unwrap_or("<no host>"),
            )));
        }

        Ok((cursor, url))
    }
}

impl Inner {
    /// The cold-start request: the ODP's facts, with change tracking on.
    ///
    /// **No `$top` and no `$select`.** `$top` bounds the whole result rather than
    /// a page, so it would truncate an initial load and then stream forward from
    /// a snapshot that never finished; `$select` would pin the projection to a
    /// field list this connector would have to keep in step with the ODP by hand,
    /// silently dropping anything added later. Paging is server-driven and its
    /// size is a `Prefer` header, which is what SAP documents for ODP.
    ///
    /// **Nothing here names a subscriber**, because the protocol has nowhere to
    /// put one: the subscription is the service plus the logon user. This request
    /// with `Prefer: odata.track-changes` *is* the delta initialisation, and SAP
    /// documents that it cannot be skipped — a first tracked read is always a
    /// full load through the queue.
    fn initial_url(&self) -> Result<Url, CdcError> {
        let mut url = self.service_root.clone();
        url.path_segments_mut()
            .map_err(|()| {
                CdcError::Backend(anyhow::anyhow!(
                    "service_root {} cannot have an entity set appended",
                    self.service_root
                ))
            })?
            .pop_if_empty()
            .push(&self.entity_set);
        if self.send_sap_client {
            url.query_pairs_mut()
                .append_pair("sap-client", &self.sap_client);
        }
        Ok(url)
    }

    /// Fetch one page.
    ///
    /// `position` is the cursor the cycle is following, or `None` on the
    /// cold-start read. It decides whether a rejection is an unusable *position*
    /// or merely a backend failure: on a cold read there is no position for the
    /// service to be objecting to.
    async fn fetch_page(&self, url: &Url, position: Option<&str>) -> Result<Page, CdcError> {
        // `odata.track-changes` asks for a delta link; `odata.maxpagesize` is the
        // page size SAP documents for ODP — and which SAP also documents as
        // starting a background job that caches the page set in the delta queue,
        // which is why it is configuration rather than a constant.
        let prefer = format!("odata.track-changes,odata.maxpagesize={}", self.page_size);
        let request = self
            .client
            .client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .header("Prefer", prefer);
        let request = self.client.authorize(request).await?;

        let response = request
            .send()
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("requesting {}: {e}", redact(url))))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(classify_failure(status.as_u16(), &body, position, url));
        }

        let body = response.text().await.map_err(|e| {
            CdcError::Backend(anyhow::anyhow!(
                "reading the ODP response from {}: {e}",
                redact(url)
            ))
        })?;
        let body: Json = serde_json::from_str(&body).map_err(|e| {
            CdcError::Backend(anyhow::anyhow!(
                "parsing the ODP response from {}: {e}",
                redact(url)
            ))
        })?;
        parse_page(self.version, &body)
    }

    /// A strictly increasing observation clock, epoch millis.
    ///
    /// Two records observed in the same millisecond would otherwise tie on
    /// `created_at` with the same `id` as tiebreak, which makes version
    /// resolution arbitrary. Forcing a strict increase records what is true of a
    /// FIFO delta queue — arrival order *is* source order — instead of throwing
    /// it away to the clock's resolution.
    fn observe(&self) -> DateTime<Utc> {
        let mut last = self.observed.lock().expect("observation clock poisoned");
        let now = Utc::now().timestamp_millis();
        let next = now.max(*last + 1);
        *last = next;
        DateTime::<Utc>::from_timestamp_millis(next).unwrap_or_else(Utc::now)
    }

    /// `created_at`, and an honest label for where it came from.
    ///
    /// See the module docs: `created_at` decides which version of an id a read
    /// resolves to, so a row describing the *past* must not be stamped with a
    /// value that lets it win. A deletion is pushed strictly after the version it
    /// retires; a before-image strictly before the after-image it is paired with.
    fn changed_at(&self, event: &RowEvent) -> (DateTime<Utc>, &'static str) {
        let declared = self
            .changed_at_property
            .as_deref()
            .and_then(|name| event.row.get(name))
            .and_then(parse_timestamp);

        if event.mode.is_deletion() {
            // ODP hands a deletion over carrying the row's pre-deletion
            // timestamp, which is the timestamp its live version already
            // published under. Stamping the tombstone with it would tie — and
            // the tiebreak is the id, which is identical — so the delete would be
            // undetectably ineffective. `max` also covers a SAP clock ahead of
            // ours.
            let observed = self.observe();
            let floor = declared
                .map(|d| d + chrono::Duration::milliseconds(1))
                .unwrap_or(observed);
            return (observed.max(floor), "retired");
        }

        if event.is_before_image() {
            // The before-image of a changed record carries the *same* business
            // timestamp as its after-image, because it is the same change. One
            // millisecond back is what stops the old values winning the tie.
            return match declared {
                Some(ts) => (ts - chrono::Duration::milliseconds(1), "superseded"),
                None => (self.observe(), "superseded"),
            };
        }

        match declared {
            Some(ts) => (ts, "entity"),
            None => (self.observe(), "observed"),
        }
    }

    /// Turn one row into a change record.
    fn record(
        &self,
        event: &RowEvent,
        op: Op,
        snapshot: Snapshot,
        position: Option<String>,
        read_from: Option<&str>,
        next_delta_link: &str,
    ) -> Result<ChangeRecord, CdcError> {
        let key = OdpKey::from_row(
            &self.sap_client,
            &self.odp_name,
            &self.key_properties,
            &event.row,
        )?;

        let (changed_at, changed_at_source) = self.changed_at(event);

        let mut payload: Stash = event.row.clone();
        // A silent overwrite here would put connector bookkeeping where a
        // business property used to be, and nothing downstream could tell.
        if payload.contains_key(ENVELOPE_META_KEY) {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "the ODP {} has a field named {ENVELOPE_META_KEY:?}, which collides with the \
                 reserved key merkql-connect uses to carry the queue's provenance into the \
                 payload. Rename the field in the ODP's projection, or replicate it with a \
                 different connector.",
                self.odp_name
            )));
        }
        payload.insert(
            ENVELOPE_META_KEY.to_string(),
            json!({
                "odp": self.odp_name,
                "entity_set": self.entity_set,
                "client": self.sap_client,
                "odata_version": dialect(self.version),
                "subscriber_identity": self.subscriber_identity,
                "key": key.parts(),
                "op": if event.mode.is_deletion() { "delete" } else { "upsert" },
                "change_mode": event.mode.as_str(),
                "entity_counter": event.counter,
                // A before-image is a historical row, not current state. A fold
                // that assumes every non-deleted version describes "now" has to
                // be able to see this.
                "before_image": event.is_before_image(),
                "read_from_delta_link": read_from,
                "next_delta_link": next_delta_link,
                "changed_at": changed_at.to_rfc3339(),
                "changed_at_source": changed_at_source,
            }),
        );

        let mut envelope =
            Envelope::new(key.envelope_id(), payload, self.authorized_tokens.clone());
        // meshql has no delete operation: a deletion is a new envelope version
        // carrying the flag, which reaches the topic as an ordinary create.
        envelope.deleted = event.mode.is_deletion();
        // `created_at` is meshql's version-ordering key, so it is a correctness
        // decision rather than a label. See `changed_at`.
        envelope.created_at = changed_at;

        Ok(ChangeRecord::new(
            op,
            envelope,
            SourceInfo {
                connector: CONNECTOR.to_string(),
                entity: self.entity.clone(),
                ts_ms: changed_at.timestamp_millis(),
                position,
                snapshot,
            },
        ))
    }

    /// The cursor to publish for a delta link.
    fn cursor(&self, delta_link: &str) -> Cursor {
        Cursor {
            v: CURSOR_VERSION,
            subscriber_identity: self.subscriber_identity.clone(),
            odp: self.odp_name.clone(),
            client: self.sap_client.clone(),
            delta_link: delta_link.to_string(),
        }
    }
}

/// Read a timestamp out of a property. Accepts epoch millis, OData v2's
/// `/Date(…)/`, and ISO-8601 with or without a zone.
fn parse_timestamp(value: &Json) -> Option<DateTime<Utc>> {
    if let Some(ms) = value.as_i64() {
        return DateTime::<Utc>::from_timestamp_millis(ms);
    }
    let text = value.as_str()?;
    if let Some(inner) = text
        .strip_prefix("/Date(")
        .and_then(|s| s.strip_suffix(")/"))
    {
        // `/Date(1469577600000)/` or `/Date(1469577600000+0000)/`.
        let millis = inner
            .split(['+', '-'])
            .next()
            .unwrap_or(inner)
            .trim()
            .parse::<i64>()
            .ok()?;
        return DateTime::<Utc>::from_timestamp_millis(millis);
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Some(dt.with_timezone(&Utc));
    }
    // SAP routinely omits the zone on an `Edm.DateTime`. OData v2 defines that
    // type as UTC, so reading it as UTC follows the spec rather than guessing —
    // but it is the one place a wrong assumption would shift every timestamp by a
    // fixed offset, so it is last and it is deliberate.
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Some(naive.and_utc());
        }
    }
    None
}

/// The dialect's name, for the metadata block.
///
/// `SapODataVersion::as_str` exists in [`crate::sap`], which a `sap-odp`-only
/// build does not compile. Four lines here beats making one connector's feature
/// imply the other's.
fn dialect(version: SapODataVersion) -> &'static str {
    match version {
        SapODataVersion::V2 => "v2",
        SapODataVersion::V4 => "v4",
    }
}

/// A URL with its query stripped. Delta links carry the token, and a token is
/// close enough to a credential that it does not belong in a log line.
fn redact(url: &Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    format!("{url}?…")
}

/// Whether an error body is the service objecting to our delta token.
fn mentions_delta_token(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "delta token",
        "deltatoken",
        "delta_token",
        "delta link",
        "no longer available",
        "subscription",
        "odq",
        "operational delta queue",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Decide whether the service rejected *our stored position* or merely failed.
///
/// Getting this wrong is expensive in both directions: calling a transient 503 an
/// unusable position re-replicates the whole ODP under `when_needed`, while
/// calling an expired token transient retries a doomed request forever and
/// delivers nothing while looking healthy.
///
/// What ODP actually answers for an expired token is **UNCONFIRMED**. `410 Gone`
/// is what OData specifies for a delta link a service no longer honours, and SAP
/// Gateway is documented to be inconsistent and to answer `400`, `404` or `412`
/// with an error body naming the token or the queue — both are recognised here.
/// Everything else stays a fatal backend error on purpose: with the real response
/// unverified, stopping loudly on a token that was fine costs an operator a
/// restart, while re-baselining on one that was not is silent and republishes the
/// whole ODP.
///
/// The body is only consulted **when a position was actually sent**. A rejection
/// on the cold-start read cannot be about a position we did not present, and
/// misreading it as one turns an authorisation problem into a re-baseline loop.
///
/// A **429** and a **401** are deliberately not unusable positions. A throttle
/// converted into a full re-read produces more throttling, and an expired access
/// token is the auth layer's problem — [`crate::sap_auth`] refreshes before
/// expiry so it does not reach here.
fn classify_failure(status: u16, body: &str, position: Option<&str>, url: &Url) -> CdcError {
    let token_rejected =
        status == 410 || (matches!(status, 400 | 404 | 412) && mentions_delta_token(body));

    match (token_rejected, position) {
        (true, Some(position)) => CdcError::UnusablePosition {
            connector: CONNECTOR,
            position: position.to_string(),
            reason: format!(
                "SAP answered {status} for the stored delta link. The delta queue no longer holds \
                 a position for this token: it has aged past ODQ's retention (24 hours for \
                 already-retrieved data by default), the subscription was terminated or \
                 re-initialised by another consumer logging on as the same user, or the system was \
                 copied since the token was issued. `DeltaLinksOf<EntitySet>` lists the tokens the \
                 queue still holds"
            ),
        },
        (true, None) => CdcError::Backend(anyhow::anyhow!(
            "SAP answered {status} for the initial tracked read of {}. This is not an unusable \
             position — no delta token was sent — so it is a service, subscription or \
             authorisation problem.",
            redact(url)
        )),
        _ => CdcError::Backend(anyhow::anyhow!(
            "SAP answered {status} for {}: {}",
            redact(url),
            body.chars().take(400).collect::<String>()
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The feed
// ─────────────────────────────────────────────────────────────────────────────

struct Feed {
    inner: Arc<Inner>,
    /// The cycle just fetched, not yet yielded.
    pending: VecDeque<Result<ChangeRecord, CdcError>>,
    /// Where the next cycle starts.
    next_url: Url,
    /// The encoded cursor `next_url` came from, or `None` for the cold read.
    position: Option<String>,
    /// The next cycle is the initial tracked read.
    initial: bool,
    /// Emit the initial read's rows, or only harvest its delta link.
    emit_initial_rows: bool,
    /// Wait a poll interval before the next cycle.
    idle: bool,
    /// A fatal error has been reported; the stream is over.
    done: bool,
}

impl Feed {
    async fn step(&mut self) -> Option<Result<ChangeRecord, CdcError>> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            if self.done {
                return None;
            }
            if self.idle {
                tokio::time::sleep(self.inner.poll_interval).await;
            }
            if let Err(e) = self.cycle().await {
                self.done = true;
                return Some(Err(e));
            }
            self.idle = true;
        }
    }

    /// One delta cycle: follow `next_url` through every page, then turn the whole
    /// thing into records.
    ///
    /// Every page is walked before anything is emitted, and that is required
    /// rather than tidy: with server-side paging the delta token is only on the
    /// **last** page, so a cycle that stopped early would have no cursor to
    /// resume from at all.
    async fn cycle(&mut self) -> Result<(), CdcError> {
        let mut url = self.next_url.clone();
        let mut rows: Vec<RowEvent> = Vec::new();
        let delta_link;

        loop {
            let page = self
                .inner
                .fetch_page(&url, self.position.as_deref())
                .await?;
            rows.extend(page.rows);

            if let Some(next) = page.next_link {
                url = self.resolve(&next)?;
                continue;
            }
            delta_link = page.delta_link;
            break;
        }

        // No delta link means the read was not served as a delta queue. Without
        // one the connector has nowhere to go next, and the only thing it *could*
        // do is re-read the ODP forever, emitting every row on every cycle — a
        // connector that looks healthy and floods the topic with duplicates of
        // history. Fatal, and named.
        let Some(delta_link) = delta_link else {
            return Err(CdcError::NoFeed {
                connector: CONNECTOR,
                reason: format!(
                    "the ODP OData service returned no delta link for {}. Either the entity set is \
                     not an ODP's facts, or the service does not honour `Prefer: \
                     odata.track-changes`, or SAP KBA 2825795 applies — Gateway has been observed \
                     omitting the delta token when JSON and server-side paging are combined, which \
                     is this connector's combination. merkql-connect will not silently degrade \
                     into re-reading the whole ODP on every poll, so this stops here. Check the \
                     subscription in transaction ODQMON.",
                    self.inner.entity_set
                ),
            });
        };

        let read_from = self.position.clone();
        let was_initial = self.initial;
        let emit = !was_initial || self.emit_initial_rows;
        let cursor = self.inner.cursor(&delta_link).encode();

        if emit {
            let last = rows.len().saturating_sub(1);
            for (i, event) in rows.iter().enumerate() {
                // A snapshot cycle emits `r`; every later cycle is live traffic
                // and emits `c`, including deletions — meshql spells a delete as
                // a new version with `deleted: true`, not as an `op: d`.
                let op = if was_initial { Op::Read } else { Op::Create };
                let snapshot = match (was_initial, i == last) {
                    (false, _) => Snapshot::False,
                    (true, false) => Snapshot::True,
                    (true, true) => Snapshot::Last,
                };
                // Only the final record of a cycle names a resumable place. A
                // delta cycle has no interior positions, so an earlier record
                // carrying one would let a restart resume past changes it never
                // appended — a permanent loss, not a duplicate.
                let position = (i == last).then(|| cursor.clone());

                let record = self.inner.record(
                    event,
                    op,
                    snapshot,
                    position,
                    read_from.as_deref(),
                    &delta_link,
                );
                let fatal = record.is_err();
                self.pending.push_back(record);
                if fatal {
                    break;
                }
            }
        }

        // The new link becomes the cursor whether or not anything was emitted.
        //
        // When a cycle is empty there is no record to hang it on, so it is only
        // held in memory and the offset store keeps the older cursor. That is
        // safe — ODP does not invalidate a token by issuing its successor — but
        // it is where this source is weaker than a row-cursor one: an ODP idle
        // for longer than the recovery window restarts onto a token past its
        // retention, which surfaces as `UnusablePosition` and needs
        // `snapshot_mode = "when_needed"` to recover. `CommitSource` has no way
        // to commit a position without a record.
        self.next_url = self.resolve(&delta_link)?;
        self.position = Some(cursor);
        self.initial = false;
        Ok(())
    }

    /// Resolve a `__next` / `__delta` link. SAP documents the delta link as
    /// *relative*, so joining against the service root is the normal path here
    /// rather than a fallback.
    fn resolve(&self, link: &str) -> Result<Url, CdcError> {
        Url::parse(link)
            .or_else(|_| self.inner.service_root.join(link))
            .map_err(|e| {
                CdcError::Backend(anyhow::anyhow!(
                    "the ODP {} returned the unusable link {link:?}: {e}",
                    self.inner.odp_name
                ))
            })
    }
}

#[async_trait]
impl CommitSource for SapOdpSource {
    fn connector(&self) -> &'static str {
        CONNECTOR
    }

    fn entity(&self) -> &str {
        &self.inner.entity
    }

    async fn changes(&self, from: Resume, mode: SnapshotMode) -> Result<ChangeStream, CdcError> {
        // # Why a partial initial load is not resumed
        //
        // [`Resume::Snapshotting`] hands back a position staged mid-snapshot so a
        // source that *can* continue does not redo hours of work. This one
        // cannot, and the reason is specific rather than lazy.
        //
        // The only thing naming a place inside an initial load is the server's
        // `!skiptoken` next-link. SAP documents `Prefer: odata.maxpagesize` as
        // starting a background job that computes the result and **caches the
        // page set in the delta queue**, so that link is a handle into a
        // server-side cache whose lifetime is ODQ's cleanup schedule — not the
        // connector's, and not something the connector can observe. How long a
        // half-consumed page set survives is **UNCONFIRMED**, and a handle whose
        // cache has been reaped does not announce itself: the plausible failure
        // is a short page or an empty one, which reads exactly like a completed
        // load. That is a gap, and a gap is the one outcome this crate refuses.
        //
        // Nor is there a fallback in the other direction: with paging on, the
        // delta token arrives only on the **last** page, so an interrupted
        // initial load has no token covering "the first three packages" either.
        //
        // So the initial load restarts, and it restarts on the SAP side too — the
        // module docs say so, and an operator sizes the backfill accordingly.
        let from = from.without_snapshot_resume();

        let (next_url, position, initial) = match &from {
            // Collapsed to `Cold` by `without_snapshot_resume` immediately above.
            // Named rather than wildcarded so that removing that call is a
            // compile error here instead of a silent behaviour change.
            Resume::Snapshotting(_) => {
                unreachable!("Resume::Snapshotting was collapsed to Cold before this match")
            }
            Resume::Cold => (self.inner.initial_url()?, None, true),
            Resume::At(raw) => {
                let (_, url) = self.decode_cursor(raw)?;
                (url, Some(raw.clone()), false)
            }
        };

        // `SnapshotMode::Never` still makes the cold-start request, and here that
        // is not merely a protocol quirk: SAP documents that a delta
        // initialisation *is* a full load and cannot be skipped. The rows are
        // fetched and dropped, which costs one full extraction the operator did
        // not want but never manufactures a token — and a manufactured token is a
        // silent skip of everything between it and reality.
        let emit_initial_rows = mode.snapshots_on_cold_start();

        let mut feed = Feed {
            inner: self.inner.clone(),
            pending: VecDeque::new(),
            next_url,
            position,
            initial,
            emit_initial_rows,
            idle: false,
            done: false,
        };

        // Under `never` the initial read is fetched **here**, not on the first
        // poll, and the difference is not cosmetic.
        //
        // `never` means "start at the live tail", and the tail is whatever the
        // tracked read returns. Leaving that read until the first `next()` makes
        // the tail whatever it is *then*, so every change committed between
        // `changes()` returning and the caller polling is discarded with the
        // rest of the full load — a small silent gap whose size is however long
        // the connector loop took to start. Capturing eagerly makes the promise
        // true at the moment it is made, and has the side benefit that a service
        // or subscription failure is reported from `changes()` rather than as
        // the stream's first item.
        //
        // Only on the discard path: when the rows are being *emitted* the same
        // read is the snapshot, and buffering a full ODP extraction in memory to
        // gain nothing is not a trade worth making. Nothing is lost there
        // either — a later read sees more, never less.
        if feed.initial && !feed.emit_initial_rows {
            feed.cycle().await?;
            feed.idle = false;
        }

        Ok(Box::pin(stream::unfold(feed, |mut feed| async move {
            feed.step().await.map(|item| (item, feed))
        })))
    }

    /// Nothing to release **at this seam**, and that is worth being precise about
    /// because ODP is the one source here where the server really does hold data
    /// back.
    ///
    /// ODQ retains a package until its retention window passes, and the only
    /// acknowledgement in the protocol is implicit in presenting the next delta
    /// token. There is no flush call to make — unlike a PostgreSQL replication
    /// slot, where advancing is both necessary and destructive. Nothing
    /// accumulates on the SAP side because *this* connector is conservative; what
    /// accumulates is a queue whose subscriber stopped reading entirely, and that
    /// is an operator's problem in ODQMON.
    async fn durable_through(&self, _position: &str) -> Result<(), CdcError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, Json)]) -> serde_json::Map<String, Json> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn id_of(keys: &[&str], pairs: &[(&str, Json)]) -> String {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
        OdpKey::from_row("100", "SEPM_SO", &keys, &row(pairs))
            .expect("the key forms")
            .envelope_id()
    }

    /// The collision the composite encoding exists to prevent. `("A","BC")` and
    /// `("AB","C")` are different business records and must never share an id —
    /// with a naive `format!("{a}-{b}")` they do, and the mesh merges them
    /// permanently with nothing reporting it.
    #[test]
    fn distinct_composite_keys_never_share_an_envelope_id() {
        let a = id_of(
            &["SalesOrder", "Item"],
            &[("SalesOrder", json!("A")), ("Item", json!("BC"))],
        );
        let b = id_of(
            &["SalesOrder", "Item"],
            &[("SalesOrder", json!("AB")), ("Item", json!("C"))],
        );
        assert_ne!(a, b);
    }

    /// A value containing the encoding's own separators must not be able to forge
    /// another record's id. The quote escaping is what makes the encoding
    /// injective, so this is the test that earns it.
    #[test]
    fn a_separator_inside_a_value_cannot_forge_another_records_id() {
        let forged = id_of(
            &["SalesOrder", "Item"],
            &[
                ("SalesOrder", json!("1',Item='9")),
                ("Item", json!("ignored")),
            ],
        );
        let real = id_of(
            &["SalesOrder", "Item"],
            &[("SalesOrder", json!("1")), ("Item", json!("9"))],
        );
        assert_ne!(forged, real);
        assert!(forged.contains("''"), "the quote is escaped: {forged}");
    }

    /// The id must not depend on the order properties happened to arrive in.
    #[test]
    fn the_id_is_independent_of_property_order() {
        let a = id_of(
            &["Item", "SalesOrder"],
            &[("Item", json!("2")), ("SalesOrder", json!("1"))],
        );
        let b = id_of(
            &["SalesOrder", "Item"],
            &[("SalesOrder", json!("1")), ("Item", json!("2"))],
        );
        assert_eq!(a, b);
    }

    /// OData v2 renders `Edm.Int32` as a string and v4 as a number. The same
    /// record read over the two dialects must get one id, or a service upgrade
    /// forks every aggregate in the mesh.
    #[test]
    fn the_id_is_invariant_across_odata_v2_and_v4_typing() {
        assert_eq!(
            id_of(&["SalesOrder"], &[("SalesOrder", json!("42"))]),
            id_of(&["SalesOrder"], &[("SalesOrder", json!(42))]),
        );
    }

    /// **The MANDT trap.** The SAP client is part of a record's database identity
    /// and never appears in an ODP payload, so two clients' records carry
    /// identical business keys. Without the client in the id they become versions
    /// of each other and the older one silently disappears from every read.
    #[test]
    fn two_clients_records_with_one_business_key_get_distinct_ids() {
        let keys = vec!["SalesOrder".to_string()];
        let row = row(&[("SalesOrder", json!("1"))]);
        let prod = OdpKey::from_row("100", "SEPM_SO", &keys, &row)
            .unwrap()
            .envelope_id();
        let qa = OdpKey::from_row("200", "SEPM_SO", &keys, &row)
            .unwrap()
            .envelope_id();
        assert_ne!(prod, qa);
        assert_eq!(prod, "sap_odp:100:SEPM_SO(SalesOrder='1')");
    }

    /// Two ODPs sharing a key name must not collide either.
    #[test]
    fn the_odp_name_is_part_of_the_id() {
        let keys = vec!["Id".to_string()];
        let row = row(&[("Id", json!("1"))]);
        assert_ne!(
            OdpKey::from_row("100", "SEPM_SO", &keys, &row)
                .unwrap()
                .envelope_id(),
            OdpKey::from_row("100", "SEPM_BP", &keys, &row)
                .unwrap()
                .envelope_id(),
        );
    }

    /// A client or ODP name carrying a structural character could forge an id, so
    /// it is refused at construction rather than encoded and hoped about.
    #[test]
    fn a_client_that_could_forge_a_separator_is_refused() {
        let parts: BTreeMap<String, String> =
            [("Id".to_string(), "1".to_string())].into_iter().collect();
        assert!(OdpKey::new("100:SEPM_SO(Id='9'", "X", parts.clone()).is_err());
        assert!(OdpKey::new("100", "SEPM'SO", parts.clone()).is_err());
        // A namespaced ABAP name is legal and must still be accepted.
        assert!(OdpKey::new("100", "/BIC/AZSALES", parts).is_ok());
    }

    /// A missing key property is fatal. Emitting under a partial key merges every
    /// row sharing whichever components did arrive.
    #[test]
    fn a_missing_key_property_is_refused_rather_than_guessed() {
        let keys = vec!["SalesOrder".to_string(), "Item".to_string()];
        let err = OdpKey::from_row("100", "SEPM_SO", &keys, &row(&[("SalesOrder", json!("1"))]))
            .expect_err("a partial key must not become an id");
        assert!(format!("{err}").contains("Item"), "{err}");
    }

    #[test]
    fn a_null_key_property_is_refused() {
        let keys = vec!["SalesOrder".to_string()];
        assert!(
            OdpKey::from_row("100", "SEPM_SO", &keys, &row(&[("SalesOrder", Json::Null)])).is_err()
        );
    }

    /// `ODQ_CHANGEMODE` has a three-value fixed domain, and only `D` retires a
    /// record. `U` is *changed*, not "before image" — that distinction lives in
    /// the counter's sign, and reading it here instead would tombstone every
    /// updated row.
    #[test]
    fn change_modes_map_to_the_right_deletion_verdict() {
        for (raw, mode, deletes) in [
            ("C", ChangeMode::Created, false),
            ("U", ChangeMode::Changed, false),
            ("D", ChangeMode::Deleted, true),
            ("d", ChangeMode::Deleted, true),
        ] {
            let parsed = ChangeMode::parse(Some(&json!(raw))).expect("a known mode parses");
            assert_eq!(parsed, mode, "{raw}");
            assert_eq!(parsed.is_deletion(), deletes, "{raw}");
        }
    }

    /// A full-load package carries no change mode, and every row in one is
    /// current state. Absent and empty mean the same thing, and neither is an
    /// error.
    #[test]
    fn an_absent_change_mode_is_a_full_load_row() {
        for raw in [None, Some(Json::Null), Some(json!("")), Some(json!("  "))] {
            assert_eq!(
                ChangeMode::parse(raw.as_ref()).unwrap(),
                ChangeMode::Unspecified
            );
        }
        assert!(!ChangeMode::Unspecified.is_deletion());
    }

    /// An unrecognised change mode is a change whose meaning is unknown. Every
    /// fallback is a silent lie, so it stops the stream and names the value.
    /// `R` and `N` are the trap: they are BW `RECORDMODE` values, and a connector
    /// that quietly accepted them would be mapping a field it is not reading.
    #[test]
    fn an_unknown_change_mode_is_fatal_and_names_itself() {
        for raw in ["Z", "R", "N", "X", "A"] {
            let err = ChangeMode::parse(Some(&json!(raw)))
                .expect_err("an unmapped record mode must not be guessed at");
            let message = format!("{err}");
            assert!(message.contains(&format!("{raw:?}")), "{message}");
            assert!(message.contains(CHANGE_MODE_PROPERTY), "{message}");
        }
    }

    /// The counter is a sign, not a sequence: a negative one on a non-deletion is
    /// the before-image of a changed record. A deletion is negative too and must
    /// not be mistaken for one.
    #[test]
    fn a_negative_counter_marks_a_before_image_but_never_a_deletion() {
        let before = RowEvent {
            mode: ChangeMode::Changed,
            counter: Some(-1),
            row: row(&[]),
        };
        let after = RowEvent {
            mode: ChangeMode::Changed,
            counter: Some(1),
            row: row(&[]),
        };
        let gone = RowEvent {
            mode: ChangeMode::Deleted,
            counter: Some(-1),
            row: row(&[]),
        };
        assert!(before.is_before_image());
        assert!(!after.is_before_image());
        assert!(!gone.is_before_image(), "a deletion is not a before-image");
    }

    /// The cursor round-trips, and it carries the identity the delta token only
    /// means something inside of.
    #[test]
    fn a_cursor_round_trips_through_its_encoding() {
        let cursor = Cursor {
            v: CURSOR_VERSION,
            subscriber_identity: "ZODP_SO_SRV/MERKQL_CDC".into(),
            odp: "SEPM_SO".into(),
            client: "100".into(),
            delta_link: "https://s4.example.com/x?!deltatoken='D20151001131537_000052000'".into(),
        };
        let back: Cursor = serde_json::from_str(&cursor.encode()).unwrap();
        assert_eq!(back, cursor);
    }

    /// v2 and v4 responses differ only in where the array and the links live, and
    /// the control columns are stripped out of the business payload in both.
    #[test]
    fn both_dialects_parse_to_one_page_shape() {
        let v2 = json!({"d": {
            "results": [{
                "__metadata": {"uri": "…"},
                "SalesOrder": "1",
                "ODQ_CHANGEMODE": "C",
                "ODQ_ENTITYCNTR": "1",
            }],
            "__next": "https://s4/next",
            "__delta": "https://s4/delta",
        }});
        let v4 = json!({
            "value": [{
                "SalesOrder": "1",
                "ODQ_CHANGEMODE": "C",
                "ODQ_ENTITYCNTR": 1,
            }],
            "@odata.nextLink": "https://s4/next",
            "@odata.deltaLink": "https://s4/delta",
        });
        let a = parse_page(SapODataVersion::V2, &v2).unwrap();
        let b = parse_page(SapODataVersion::V4, &v4).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.rows[0].mode, ChangeMode::Created);
        assert_eq!(a.rows[0].counter, Some(1));
        assert_eq!(a.rows[0].row.keys().collect::<Vec<_>>(), vec!["SalesOrder"]);
        assert_eq!(a.next_link.as_deref(), Some("https://s4/next"));
        assert_eq!(a.delta_link.as_deref(), Some("https://s4/delta"));
    }

    /// v2 renders the counter as a string. Reading only `as_i64` would make every
    /// v2 before-image indistinguishable from an after-image — the two rows of
    /// one change would tie, and which values a read saw would be arbitrary.
    #[test]
    fn a_string_typed_counter_still_carries_its_sign() {
        let page = parse_page(
            SapODataVersion::V2,
            &json!({"d": {"results": [{
                "SalesOrder": "1",
                "ODQ_CHANGEMODE": "U",
                "ODQ_ENTITYCNTR": "-1",
            }], "__delta": "https://s4/delta"}}),
        )
        .unwrap();
        assert_eq!(page.rows[0].counter, Some(-1));
        assert!(page.rows[0].is_before_image());
    }

    /// A body that is neither dialect's shape is refused. Reading a gateway error
    /// page as an empty page would report "no changes" for a request that failed,
    /// forever.
    #[test]
    fn a_response_that_is_not_a_delta_package_is_refused() {
        assert!(parse_page(SapODataVersion::V4, &json!({"error": {"message": "no"}})).is_err());
        assert!(parse_page(SapODataVersion::V2, &json!({"value": []})).is_err());
        assert!(parse_page(SapODataVersion::V4, &json!({"d": {"results": []}})).is_err());
    }

    /// A 410 on a stored position is the token being rejected; the same status on
    /// the cold read cannot be, because no token was sent. Calling the second one
    /// an unusable position would turn an authorisation failure into a re-baseline
    /// loop under `when_needed`.
    #[test]
    fn only_a_rejection_of_a_token_we_sent_is_an_unusable_position() {
        let url = Url::parse("https://s4.example.com/x").unwrap();
        assert!(matches!(
            classify_failure(410, "", Some("cursor"), &url),
            CdcError::UnusablePosition { .. }
        ));
        assert!(matches!(
            classify_failure(410, "", None, &url),
            CdcError::Backend(_)
        ));
    }

    /// A throttle is not a position failure. Under `when_needed` that
    /// misclassification re-reads the whole ODP on a transient 429, which produces
    /// more throttling.
    #[test]
    fn a_transient_failure_is_not_an_unusable_position() {
        let url = Url::parse("https://s4.example.com/x").unwrap();
        for status in [429, 500, 503, 401] {
            assert!(
                matches!(
                    classify_failure(status, "busy", Some("cursor"), &url),
                    CdcError::Backend(_)
                ),
                "{status}"
            );
        }
    }

    /// SAP Gateway is documented to be inconsistent about `410`, so a `400` whose
    /// body names the delta queue is its other spelling of "your token is gone".
    /// Missing it means retrying a doomed request forever while looking healthy.
    #[test]
    fn a_400_naming_the_delta_token_is_an_unusable_position() {
        let url = Url::parse("https://s4.example.com/x").unwrap();
        assert!(matches!(
            classify_failure(
                400,
                "Delta token 'D20151001131537_000052000' is no longer available",
                Some("cursor"),
                &url
            ),
            CdcError::UnusablePosition { .. }
        ));
    }

    /// A failure message must never carry the token: it is the one thing in the
    /// URL close enough to a credential to keep out of a log.
    #[test]
    fn failure_messages_do_not_leak_the_delta_token() {
        let url =
            Url::parse("https://s4.example.com/x?!deltatoken='D20151001131537_000052000'").unwrap();
        let message = format!("{}", classify_failure(500, "boom", None, &url));
        assert!(!message.contains("D20151001131537"), "{message}");
    }

    /// The observation clock is strictly increasing, so two records seen in one
    /// millisecond keep their delivery order in `created_at` — which is what
    /// decides which version of an id wins a read.
    #[test]
    fn the_observation_clock_never_repeats_a_millisecond() {
        let inner = test_inner(None);
        let times: Vec<i64> = (0..50)
            .map(|_| inner.observe().timestamp_millis())
            .collect();
        for pair in times.windows(2) {
            assert!(pair[1] > pair[0], "{times:?}");
        }
    }

    /// **The tombstone trap.** ODP hands a deletion over carrying the row's
    /// pre-deletion timestamp, so a tombstone stamped with it would tie with —
    /// and, the tiebreak being the identical id, arbitrarily lose to — the very
    /// version it retires. It must sort strictly after.
    #[test]
    fn a_deletion_sorts_after_the_version_it_retires() {
        let inner = test_inner(Some("LastChangeDateTime"));
        let fields = &[
            ("SalesOrder", json!("1")),
            ("LastChangeDateTime", json!("2026-07-31T09:00:00Z")),
        ];

        let live = RowEvent {
            mode: ChangeMode::Changed,
            counter: Some(1),
            row: row(fields),
        };
        let gone = RowEvent {
            mode: ChangeMode::Deleted,
            counter: Some(-1),
            row: row(fields),
        };

        let (live_at, live_source) = inner.changed_at(&live);
        let (gone_at, gone_source) = inner.changed_at(&gone);

        assert_eq!(live_source, "entity");
        assert_eq!(gone_source, "retired");
        assert!(
            gone_at > live_at,
            "the tombstone must win the version comparison: {gone_at} vs {live_at}"
        );
    }

    /// A deletion whose row carries a timestamp *ahead* of our clock still has to
    /// sort after it — an SAP system running ahead is not a reason for a delete to
    /// lose.
    #[test]
    fn a_deletion_outranks_a_future_dated_row() {
        let inner = test_inner(Some("LastChangeDateTime"));
        let future = "2999-01-01T00:00:00Z";
        let gone = RowEvent {
            mode: ChangeMode::Deleted,
            counter: Some(-1),
            row: row(&[
                ("SalesOrder", json!("1")),
                ("LastChangeDateTime", json!(future)),
            ]),
        };
        let (at, _) = inner.changed_at(&gone);
        assert!(at > DateTime::parse_from_rfc3339(future).unwrap());
    }

    /// The before-image and after-image of one change carry the *same* business
    /// timestamp, because they are the same change. Stamped equally they would
    /// tie on an identical id, and a read would arbitrarily return the old
    /// values.
    #[test]
    fn a_before_image_sorts_before_the_after_image_it_is_paired_with() {
        let inner = test_inner(Some("LastChangeDateTime"));
        let fields = &[
            ("SalesOrder", json!("1")),
            ("LastChangeDateTime", json!("2026-07-31T09:00:00Z")),
        ];
        let (before_at, before_source) = inner.changed_at(&RowEvent {
            mode: ChangeMode::Changed,
            counter: Some(-1),
            row: row(fields),
        });
        let (after_at, after_source) = inner.changed_at(&RowEvent {
            mode: ChangeMode::Changed,
            counter: Some(1),
            row: row(fields),
        });
        assert_eq!(before_source, "superseded");
        assert_eq!(after_source, "entity");
        assert!(before_at < after_at, "{before_at} vs {after_at}");
    }

    /// SAP spells a timestamp four ways depending on dialect and field type, and
    /// all four have to land on the same instant — a `created_at` that drifts by a
    /// timezone is a version-ordering bug, not a display one.
    #[test]
    fn timestamps_parse_from_every_spelling_sap_uses() {
        let want = DateTime::parse_from_rfc3339("2026-07-31T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for value in [
            json!("2026-07-31T09:00:00Z"),
            json!("2026-07-31T09:00:00"),
            json!("/Date(1785488400000)/"),
            json!(1785488400000i64),
        ] {
            assert_eq!(parse_timestamp(&value), Some(want), "{value}");
        }
        assert_eq!(parse_timestamp(&json!("not a date")), None);
    }

    fn test_inner(changed_at_property: Option<&str>) -> Inner {
        Inner {
            client: AuthedClient::new(SapAuth::None).unwrap(),
            service_root: Url::parse("https://s4.example.com/sap/opu/odata/SAP/ZODP_SRV").unwrap(),
            entity_set: "FactsOfSEPM_SO".to_string(),
            odp_name: "SEPM_SO".to_string(),
            sap_client: "100".to_string(),
            send_sap_client: true,
            subscriber_identity: "ZODP_SO_SRV/MERKQL_CDC".to_string(),
            entity: "sales_order".to_string(),
            version: SapODataVersion::V4,
            key_properties: vec!["SalesOrder".to_string()],
            changed_at_property: changed_at_property.map(str::to_string),
            authorized_tokens: vec!["sap".to_string()],
            page_size: 100,
            poll_interval: Duration::from_millis(50),
            observed: Mutex::new(0),
        }
    }
}
