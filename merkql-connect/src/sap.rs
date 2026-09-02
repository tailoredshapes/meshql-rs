//! SAP S/4HANA CDC over OData delta: the delta token *is* the cursor, and the
//! entity key predicate *is* the envelope id.
//!
//! # Scope: this connector needs a *delta-enabled* service, which stock S/4HANA
//! APIs are not
//!
//! Read this before pointing the connector at an API. The delta protocol is not
//! a generic OData feature that any entity set has:
//!
//! - **OData v2 delta is a framework capability each application's data
//!   provider class must implement.** SAP Gateway supplies the plumbing (the
//!   delta token, the `/IWBEP/D_QRL_*` tables); whether a given entity set
//!   answers a tracked read at all is up to the DPC someone wrote.
//! - **OData v4 delta in the ABAP OData v4 framework is on-premise / private
//!   cloud only.** It is excluded from S/4HANA Cloud public edition.
//! - **No stock S/4HANA A2X OData API is documented as delta-enabled.**
//!
//! So this source is for a service someone deliberately built delta support
//! into. Point it at a standard `API_BUSINESS_PARTNER`-style A2X API and the
//! best case is the immediate hard failure [`CdcError::NoFeed`] (no delta link
//! came back), which is why that guard exists and why it is fatal rather than a
//! warning.
//!
//! ## What to use instead when the service has no delta support
//!
//! - **On-premise / private cloud:** define a CDS view with Change Data Capture
//!   and consume it through ODP (the Operational Delta Queue). That is SAP's
//!   own supported change feed and it does capture deletions.
//! - **S/4HANA Cloud public edition:** the CDI (Cloud Data Integration) API over
//!   CDS views, for the same reason.
//! - **Business events** via SAP Event Mesh / the Event Bus. These are real
//!   push, but they are **key-only**: an event names the object that changed and
//!   nothing else, so every event needs a follow-up API read to get a payload,
//!   and that read sees the *current* state rather than the state at the event.
//!   A connector built on them is a different source, not this one.
//!
//! ## How long a cursor lives
//!
//! Nothing in SAP documents an expiry for an OData v2 delta token. Its lifetime
//! is instead governed by whenever somebody schedules the cleanup report
//! `/IWBEP/R_CLEAN_UP_QRL` over `/IWBEP/D_QRL_HDR` and `/IWBEP/D_QRL_ITM` — an
//! operational decision at the SAP end that this connector cannot see, predict
//! or influence. The only concretely documented retention numbers anywhere in
//! the stack are the Operational Delta Queue's, and they are for a different
//! mechanism: recovery of already-retrieved data **24 hours**, low relevance
//! **1 week**, medium relevance **31 days**.
//!
//! Treat "the cursor is gone, re-baseline" as a **normal operating mode**, not
//! an exception. That is what `snapshot_mode = "when_needed"` is for, and it is
//! the mode a SAP source should be deployed with unless the operator has a
//! specific reason otherwise. What must never happen is the re-baseline
//! happening *silently*, which is why an expired token surfaces as
//! [`CdcError::UnusablePosition`] and lets [`SnapshotMode`] decide.
//!
//! # OData v2 deletions exist only in Atom, so the v2 path parses Atom
//!
//! This is the sharpest protocol difference between the two dialects, and
//! getting it wrong is invisible.
//!
//! A v4 delta response spells a deletion in JSON, as `{"@removed": …,
//! "@id": …}`. **A v2 delta response cannot.** SAP Gateway's delta query support
//! carries deletions as the Atom `deleted-entry` element of RFC 6721 (the
//! backend hands the framework a `<DELETED_ENTITIES>` list of entity ids), and
//! SAP states plainly that this is Atom/XML only and **not supported in JSON**.
//!
//! An earlier cut of this module requested `application/json` on both dialects
//! and looked for `@sap.deleted_entity` / `deleted_entity` in the v2 payload.
//! Those keys are v4's spelling wearing a v2 costume: on a real v2 service they
//! never appear, so **no deletion would ever be observed, and nothing would
//! report a problem.** Rows would keep flowing, the connector would look
//! perfectly healthy, and every deleted business partner would live forever in
//! the mesh. That is precisely the silent gap this crate exists to prevent, so
//! it is fixed rather than documented:
//!
//! - **v2 reads ask for `application/atom+xml` and are parsed as an Atom feed**,
//!   including `<at:deleted-entry ref="…">` tombstones. See [`parse_v2_atom`].
//! - **No `$format=json` is sent.** SAP's own documentation lists JSON format
//!   alongside `$skiptoken`, `$top`, `$skip` and `$expand` as *mutually
//!   exclusive with a v2 delta query*, so asking for it is asking the service to
//!   either refuse the delta or answer without one.
//! - A v2 response body that turns out to be JSON is a **hard error naming this
//!   limitation** rather than a best-effort parse, because a best-effort parse
//!   is exactly how the gap reopens.
//! - The refuse-to-start alternative — hard-failing `open()` for any v2 source —
//!   was rejected: it costs a dependency to actually capture deletes, and
//!   "capture them" beats "decline to run" whenever the capture is achievable.
//!
//! Paging on the *initial* snapshot read is a different request and stays: the
//! server drives it with `rel="next"`, and following it is how a snapshot
//! finishes. It is only the delta read that must carry none of those options,
//! and none of them are sent on one.
//!
//! # Where this logic came from, and why it is duplicated rather than shared
//!
//! The OData v2/v4 delta walk here, and the six auth modes now in
//! [`crate::sap_auth`], are **deliberately duplicated from
//! `tailoredshapes/sap-cdc-mcp`** (`crates/sap-cdc/src/source/odata.rs` and
//! `crates/sap-cdc/src/auth.rs`), which already solved them for a different
//! sink. Extracting them into a crate both repos depend on is the
//! architecturally cleaner move and it was considered and rejected for this
//! first cut:
//!
//! - `sap-cdc-mcp` is a **separate git repository**, so a shared crate becomes
//!   a GitHub-git-dep pinned to a tag — the convention the workspace already
//!   uses for `merkql` and `merk-cloud`. That buys a two-repo release dance
//!   for every fix.
//! - The shapes are not actually the same. `sap-cdc`'s `Source` trait is
//!   poll-and-persist-your-own-state; [`CommitSource`] is opaque-cursor,
//!   snapshot-then-stream, and above all **[`CdcError::UnusablePosition`] or
//!   nothing**. Roughly half of what follows exists only to express that, and
//!   would have to live here even if the transport were shared.
//! - Three divergences below are *corrections*, not ports (see `$top`, key
//!   canonicalisation, and the Atom v2 delta path above), and shipping them as a
//!   shared-crate change would mean changing `sap-cdc-mcp`'s behaviour as a side
//!   effect of writing a merkql connector.
//!
//! When a third consumer of SAP OData appears, extract then. Until then this
//! note is the pointer: **a fix to the delta walk or to auth probably belongs
//! in both places.**
//!
//! That reasoning is about *another repository*, and it does not extend to
//! another module of this crate. [`crate::sap_odp`] speaks to the same gateways
//! with the same credentials, so the auth modes were lifted out into
//! [`crate::sap_auth`] the moment there were two callers — a second copy of
//! credential handling in one `src/` directory is a copy that gets fixed once.
//!
//! # This source is a poller, on purpose, and that is not a silent degradation
//!
//! [`crate::source`]'s whole argument is that a change feed is push-shaped and
//! that a connector which quietly becomes a poller is a connector whose latency
//! and load stopped matching what was deployed — hence
//! [`CdcError::NoFeed`].
//!
//! SAP OData **has no notification edge at all.** There is no inotify, no
//! `LISTEN`, no `watch()` cursor; the delta protocol's own model is "hold this
//! token, come back later." So this source polls the delta link on an interval,
//! and that is the protocol working as designed rather than a fallback from
//! something better. What `NoFeed` still guards here is the case that *is* a
//! silent degradation: a service that returns no delta link, which would turn
//! the connector into a full-table re-reader emitting every row every cycle
//! while looking healthy. See [`SapSource::changes`].
//!
//! The push-shaped alternative for SAP is a Debezium/SLT webhook, which is a
//! different source and not this module.
//!
//! # A delta cycle is atomic, so only its last record carries a position
//!
//! An OData delta response is not a per-row cursor. A cycle fetches one or more
//! pages (`__next` / `@odata.nextLink`) and ends with a **single** new delta
//! link covering everything the cycle returned. There is no position that names
//! "halfway through a cycle", so inventing one would be a lie that skips.
//!
//! Therefore every record of a cycle carries `position: None` except the last,
//! which carries the new delta link. A crash mid-cycle stages nothing and
//! replays the whole cycle on restart — duplicates, never a gap, exactly the
//! trade the crate contract asks for.
//!
//! # Snapshot-then-stream falls out of the protocol
//!
//! On a cold start the connector issues the entity-set read with `Prefer:
//! odata.track-changes`. SAP returns the rows **and** a delta link consistent
//! with that read. So the snapshot and the streaming-position capture are the
//! same request, and the overlap the other sources pay for (open the stream
//! first, tolerate duplicates) does not arise: the service guarantees the token
//! covers everything after the rows it just handed over.
//!
//! # An expired delta token is never a silent full re-read
//!
//! SAP invalidates a delta token after a retention window (and after certain
//! system copies/upgrades). It says so with `410 Gone`, or — SAP Gateway is
//! not consistent about this — a `400`/`404`/`412` whose error body names the
//! delta token. Either way this module reports
//! [`CdcError::UnusablePosition`] and lets [`SnapshotMode`] decide. Quietly
//! starting a fresh tracked read instead would republish the entire entity set
//! with nothing in the configuration having asked for it, and — worse — would
//! do it *without* the operator learning that the token had expired, so the
//! next expiry would be just as invisible.
//!
//! # What the domain sees: the `source` block does not survive the sink
//!
//! Debezium's `source` block is connector bookkeeping and is stripped when a
//! record is folded into a repository. So everything downstream needs is
//! materialised **into the envelope payload** under a reserved `_sap` object:
//! the entity set, the OData version, the canonical key parts, whether the row
//! was an upsert or a tombstone, the delta token the cycle was read from, the
//! delta token for the next cycle, and the change timestamp with an honest
//! label saying whether it came from the entity or from our own clock.
//!
//! A row that already has a `_sap` property is a hard error rather than a
//! silent overwrite — see [`ENVELOPE_META_KEY`].

use crate::config::{SapAuthConfig, SapODataVersion};
use crate::record::{ChangeRecord, Op, Snapshot, SourceInfo};
use crate::sap_auth::{AuthedClient, SapAuth};
use crate::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use futures::stream;
use meshql_core::{Envelope, Stash};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;
use quick_xml::Reader;
use reqwest::StatusCode;
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

const CONNECTOR: &str = "sap";

/// The reserved payload key holding everything the `source` block cannot carry
/// past the sink. A SAP entity with a property of this name would be silently
/// overwritten by it, so that collision is refused instead.
pub const ENVELOPE_META_KEY: &str = "_sap";

/// How long to wait before asking the delta link again when a cycle produced
/// nothing. Configurable; this is only the default.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 30_000;

// ─────────────────────────────────────────────────────────────────────────────
// The composite-key encoding
// ─────────────────────────────────────────────────────────────────────────────

/// One SAP entity's key, canonicalised.
///
/// # Why this is the sharpest problem in the connector
///
/// SAP keys are routinely composite — `A_BusinessPartnerAddress` is keyed on
/// `(BusinessPartner, AddressID)`, `A_SalesOrderItem` on `(SalesOrder,
/// SalesOrderItem)` — and a meshql envelope id is a single string. Every
/// obvious encoding is wrong in a way that does not announce itself:
///
/// - `format!("{a}-{b}")` merges `("x-y", "z")` with `("x", "y-z")`. Two
///   distinct business records become one aggregate, and every fold over them
///   is silently wrong forever.
/// - Concatenating in `$metadata` declaration order makes the id depend on a
///   document SAP can reorder across a release upgrade. The ids then *change*,
///   which forks every aggregate in the mesh at the moment of the upgrade.
/// - Passing typed JSON through makes the id depend on the OData version:
///   v2 renders `Edm.Int32` as the string `"1"`, v4 as the number `1`. The same
///   record read over the two protocols would get two ids.
///
/// # The encoding
///
/// ```text
/// <EntitySet>(<Name>='<Value>',<Name>='<Value>',…)
/// ```
///
/// - Pairs are sorted by **property name**, byte order. Deterministic
///   regardless of what order `$metadata`, the JSON payload or the config
///   listed them in.
/// - Values are canonicalised to **text** and always single-quoted, with an
///   internal `'` doubled (`''`), which is OData's own literal escaping.
/// - The entity set prefixes the whole thing, so two entity sets that share a
///   key name cannot collide.
///
/// That is OData's canonical key predicate with one deviation (name ordering
/// instead of declaration ordering), so an id is still readable as, and
/// trivially convertible back into, a SAP resource path.
///
/// ## Why it is injective
///
/// OData property names are identifiers — letters, digits and `_`, never `=`,
/// `,`, `(`, `)` or `'`. Values are the only free text, and they are always
/// quoted with the classic doubled-quote escape, so a scanner finds the closing
/// quote unambiguously (the first `'` not followed by another `'`). Every
/// structural character therefore appears outside quotes only as a separator.
/// Decoding is total, so encoding is injective.
///
/// ## What text canonicalisation costs
///
/// Integer `1` and string `"1"` encode identically. Within one entity set a key
/// property has exactly one EDM type, so those two cannot be distinct records —
/// and the entity set is part of the id. The collision is unreachable, and in
/// exchange the id is invariant across OData v2 vs v4 and across a row payload
/// vs a tombstone's key predicate, which is the drift that actually happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SapKey {
    entity_set: String,
    /// Sorted by name — `BTreeMap` is the invariant, not an implementation
    /// detail.
    parts: BTreeMap<String, String>,
}

impl SapKey {
    /// The envelope id. Stable for the life of the record.
    pub fn envelope_id(&self) -> String {
        let body = self
            .parts
            .iter()
            .map(|(name, value)| format!("{name}='{}'", value.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        format!("{}({body})", self.entity_set)
    }

    /// The key parts, for materialising into the payload.
    pub fn parts(&self) -> &BTreeMap<String, String> {
        &self.parts
    }

    /// Build a key from a delta row.
    ///
    /// Prefers the row's own properties. A v4 tombstone carries only
    /// `{"@removed": …, "@id": "A_BusinessPartner('1')"}`, so the entity id URL
    /// is the fallback — and the fallback is *only* a fallback, because the
    /// properties are the authority whenever they are present.
    ///
    /// Missing key properties with no id URL is a **hard error**. Emitting a
    /// record under a partial key would merge every row that shares the
    /// properties that did arrive, which is the failure this whole type exists
    /// to prevent, and it would do it silently.
    pub fn from_row(
        entity_set: &str,
        key_properties: &[String],
        row: &serde_json::Map<String, Json>,
        id_url: Option<&str>,
    ) -> Result<Self, CdcError> {
        let mut parts = BTreeMap::new();
        let mut missing = Vec::new();
        for name in key_properties {
            match row.get(name) {
                Some(value) => {
                    parts.insert(name.clone(), key_text(entity_set, name, value)?);
                }
                None => missing.push(name.clone()),
            }
        }

        if missing.is_empty() {
            return Ok(Self {
                entity_set: entity_set.to_string(),
                parts,
            });
        }

        let Some(id_url) = id_url else {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "a {entity_set} row is missing key {missing:?} and carries no entity id URL, \
                 so its envelope id cannot be formed. Emitting it under a partial key would \
                 silently merge it with every other row sharing the key properties that did \
                 arrive. Check `key_properties` against the service's $metadata, and check \
                 that any `$select` includes every key property."
            )));
        };

        let parts = parse_key_predicate(entity_set, key_properties, id_url)?;
        Ok(Self {
            entity_set: entity_set.to_string(),
            parts,
        })
    }
}

/// Canonicalise a key value to text.
///
/// `null` is refused: OData key properties are non-nullable, so a null one
/// means the payload is not what the configured `key_properties` claim, and
/// coercing it to `""` would merge every such row.
fn key_text(entity_set: &str, name: &str, value: &Json) -> Result<String, CdcError> {
    match value {
        Json::String(s) => Ok(s.clone()),
        Json::Number(n) => Ok(n.to_string()),
        Json::Bool(b) => Ok(b.to_string()),
        Json::Null => Err(CdcError::Backend(anyhow::anyhow!(
            "{entity_set}.{name} is configured as a key property but arrived null; OData key \
             properties are non-nullable, so either `key_properties` is wrong or the payload is"
        ))),
        Json::Array(_) | Json::Object(_) => Err(CdcError::Backend(anyhow::anyhow!(
            "{entity_set}.{name} is configured as a key property but arrived as a structured \
             value; only scalars can be part of an entity key"
        ))),
    }
}

/// Pull key parts out of an OData entity id URL — `…/A_BusinessPartner('1')` or
/// `…/A_SalesOrderItem(SalesOrder='10',SalesOrderItem='20')`.
///
/// The single-key form omits the property name, which is why
/// `key_properties` is needed here and not merely for lookup: without it, the
/// bare literal has nothing to be called.
fn parse_key_predicate(
    entity_set: &str,
    key_properties: &[String],
    id_url: &str,
) -> Result<BTreeMap<String, String>, CdcError> {
    let bad = |why: &str| {
        CdcError::Backend(anyhow::anyhow!(
            "cannot read a {entity_set} key out of the entity id {id_url:?}: {why}"
        ))
    };

    let trimmed = id_url.trim();
    let inner = trimmed
        .strip_suffix(')')
        .ok_or_else(|| bad("it does not end in a key predicate"))?;
    let open = find_unquoted(inner, '(').ok_or_else(|| bad("it has no opening parenthesis"))?;
    let predicate = &inner[open + 1..];

    let mut parts = BTreeMap::new();
    for field in split_unquoted(predicate, ',') {
        let field = field.trim();
        let (name, literal) = match find_unquoted(field, '=') {
            Some(eq) => (field[..eq].trim().to_string(), &field[eq + 1..]),
            None => {
                // The single-key shorthand. Only meaningful when there is
                // exactly one key property to give it a name.
                if key_properties.len() != 1 {
                    return Err(bad(
                        "it uses the unnamed single-key form, but the entity has a composite key",
                    ));
                }
                (key_properties[0].clone(), field)
            }
        };
        parts.insert(name, decode_literal(literal.trim(), &bad)?);
    }

    if parts.is_empty() {
        return Err(bad("the key predicate is empty"));
    }

    // A predicate naming properties we were not told about means the configured
    // key does not describe this service. Refusing beats building an id out of
    // whatever happened to be in the URL.
    for name in parts.keys() {
        if !key_properties.iter().any(|k| k == name) {
            return Err(bad(&format!(
                "it names {name:?}, which is not in the configured key_properties \
                 {key_properties:?}"
            )));
        }
    }
    for name in key_properties {
        if !parts.contains_key(name) {
            return Err(bad(&format!("it does not name the key property {name:?}")));
        }
    }

    Ok(parts)
}

/// Decode one OData literal back to the canonical text form.
///
/// URL-encoded octets are decoded here rather than left alone: a tombstone's
/// key arrives percent-encoded inside a URL while the same record's upsert
/// arrives raw in a JSON property, and leaving the two different would give one
/// business record two envelope ids — a fork rather than a merge, but just as
/// silent.
fn decode_literal(literal: &str, bad: &dyn Fn(&str) -> CdcError) -> Result<String, CdcError> {
    let raw = match literal
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        Some(quoted) => quoted.replace("''", "'"),
        // Unquoted: a numeric, boolean or guid literal. Canonicalised to text
        // exactly as `key_text` would render the JSON scalar.
        None => literal.to_string(),
    };
    percent_encoding::percent_decode_str(&raw)
        .decode_utf8()
        .map(|s| s.into_owned())
        .map_err(|e| {
            bad(&format!(
                "a key literal is not valid percent-encoded UTF-8: {e}"
            ))
        })
}

/// Index of the first `needle` that is not inside a `'…'` literal.
fn find_unquoted(haystack: &str, needle: char) -> Option<usize> {
    let mut quoted = false;
    let mut chars = haystack.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\'' {
            // A doubled quote is an escaped quote, not a state change.
            if quoted && chars.peek().map(|(_, c)| *c) == Some('\'') {
                chars.next();
                continue;
            }
            quoted = !quoted;
            continue;
        }
        if !quoted && c == needle {
            return Some(i);
        }
    }
    None
}

/// Split on `sep`, ignoring separators inside `'…'` literals.
fn split_unquoted(haystack: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut rest = haystack;
    while let Some(i) = find_unquoted(rest, sep) {
        out.push(&haystack[start..start + i]);
        start += i + sep.len_utf8();
        rest = &haystack[start..];
    }
    out.push(&haystack[start..]);
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Page parsing
// ─────────────────────────────────────────────────────────────────────────────

/// One row as the service reported it, before it becomes an envelope.
#[derive(Debug, Clone)]
struct RowEvent {
    /// A delta tombstone. meshql models a deletion as a new envelope version
    /// with `deleted: true`, so this becomes a flag on the envelope rather than
    /// an `Op::Delete` — see [`crate::record::Op`].
    deleted: bool,
    row: serde_json::Map<String, Json>,
    /// The service's own id URL for the entity, when the payload carries one.
    id_url: Option<String>,
    /// When the service says the deletion happened. Only Atom carries this —
    /// `<at:deleted-entry when="…">` — and a tombstone has no properties for
    /// `changed_at_property` to name, so this is the only chance to report a
    /// deletion's real time instead of our poll time.
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default)]
struct Page {
    rows: Vec<RowEvent>,
    next_link: Option<String>,
    delta_link: Option<String>,
}

/// OData **v2**: an Atom feed, because that is the only representation in which
/// a v2 deletion exists at all.
///
/// See the module docs for the argument. The shape:
///
/// ```xml
/// <feed xmlns="http://www.w3.org/2005/Atom"
///       xmlns:m="…/metadata" xmlns:d="…/dataservices"
///       xmlns:at="http://purl.org/atom/tombstones/1.0">
///   <link rel="delta" href="…?!deltatoken=D1"/>
///   <entry>
///     <id>https://host/svc/A_X(Key='1')</id>
///     <content type="application/xml">
///       <m:properties><d:Key>1</d:Key><d:City>Leeds</d:City></m:properties>
///     </content>
///   </entry>
///   <at:deleted-entry ref="https://host/svc/A_X(Key='2')" when="2026-07-31T10:00:00Z"/>
/// </feed>
/// ```
///
/// Three things here are load-bearing rather than incidental:
///
/// - **`<link>` is only read outside an `<entry>`.** An entry's own links are
///   `edit` and navigation-property links; mistaking one for `rel="next"` would
///   walk the cycle off into a related entity set.
/// - **A `deleted-entry` with no `ref` is a hard error.** It is the one element
///   in the whole feed whose only content is the identity of something that has
///   gone; dropping it because it was malformed would lose a deletion silently,
///   which is the exact failure this parse path exists to prevent.
/// - **A JSON body is refused**, not best-effort parsed. A v2 service answering
///   JSON has no way to have told us about deletions.
fn parse_v2_atom(body: &str) -> Result<Page, CdcError> {
    if body.trim_start().starts_with('{') {
        return Err(CdcError::Backend(anyhow::anyhow!(
            "the OData v2 service answered with JSON. OData v2 delta responses carry deletions \
             only as the Atom `deleted-entry` element and SAP does not support them in JSON, so \
             reading this body would mean never observing a deletion and never saying so. Check \
             that the service honours `Accept: application/atom+xml`, and that no `$format=json` \
             is pinned in source.service_root or in the stored delta link."
        )));
    }

    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);

    let mut feed = AtomFeed::default();
    loop {
        let event = reader.read_event().map_err(|e| {
            CdcError::Backend(anyhow::anyhow!(
                "the OData v2 service's response is not well-formed XML: {e}"
            ))
        })?;
        match event {
            Event::Start(e) => feed.start(&e)?,
            // A self-closing element — `<at:deleted-entry …/>`, `<link …/>`, an
            // empty property — is a start immediately followed by an end.
            Event::Empty(e) => {
                let local = atom_local(e.name());
                feed.start(&e)?;
                feed.end(&local);
            }
            Event::End(e) => {
                let local = atom_local(e.name());
                feed.end(&local);
            }
            Event::Text(t) => {
                let raw = t.xml_content().map_err(|e| {
                    CdcError::Backend(anyhow::anyhow!(
                        "the OData v2 service's response contains unreadable XML text: {e}"
                    ))
                })?;
                // Unescaping is not optional. A business partner called
                // `O&apos;Neill &amp; Sons` is a key value, and a key value read
                // with its entities intact is a different envelope id from the
                // same record read any other way.
                let chunk = quick_xml::escape::unescape(&raw).map_err(|e| {
                    CdcError::Backend(anyhow::anyhow!(
                        "the OData v2 service's response contains an XML entity that cannot be \
                         resolved, so a property value would be wrong rather than missing: {e}"
                    ))
                })?;
                feed.text.push_str(&chunk);
            }
            Event::CData(c) => feed.text.push_str(&String::from_utf8_lossy(&c)),
            Event::Eof => break,
            _ => {}
        }
    }

    if !feed.saw_feed {
        return Err(CdcError::Backend(anyhow::anyhow!(
            "the OData v2 service's response is XML but not an Atom <feed>. A gateway error page \
             or an HTML login redirect looks like this; so does a single-entity read, which is \
             not what this connector requests."
        )));
    }

    Ok(feed.page)
}

/// The Atom reader's state. A struct rather than a pile of locals because a
/// self-closing element has to run the start and end handling in one step, and
/// closures over a dozen `&mut`s do not.
#[derive(Default)]
struct AtomFeed {
    page: Page,
    saw_feed: bool,
    /// Depth of the element currently open; the document root is 1.
    depth: usize,
    /// Depth of the enclosing `<entry>`, when inside one.
    entry_depth: Option<usize>,
    /// Depth of the enclosing `<m:properties>`, when inside one.
    props_depth: Option<usize>,
    /// The entry being built.
    row: serde_json::Map<String, Json>,
    id_url: Option<String>,
    /// Open complex properties, innermost last. Empty means the next value
    /// belongs directly to `row`.
    objects: Vec<serde_json::Map<String, Json>>,
    names: Vec<String>,
    /// `(m:type, m:null)` per open property, parallel to `names`.
    types: Vec<(Option<String>, bool)>,
    capture_id: bool,
    text: String,
}

impl AtomFeed {
    fn start(&mut self, e: &BytesStart<'_>) -> Result<(), CdcError> {
        self.depth += 1;
        self.text.clear();
        let local = atom_local(e.name());

        // Inside `<m:properties>` every element is a property, and a property
        // is either a leaf (text) or a complex type (child elements). Which one
        // is not known until it closes, so push a place for both.
        if self.props_depth.is_some() {
            self.names.push(local);
            self.objects.push(serde_json::Map::new());
            self.types.push((
                atom_attr(e, "type")?,
                atom_attr(e, "null")?.as_deref() == Some("true"),
            ));
            return Ok(());
        }

        match local.as_str() {
            "feed" if self.depth == 1 => self.saw_feed = true,
            "entry" if self.entry_depth.is_none() => {
                self.entry_depth = Some(self.depth);
                self.row = serde_json::Map::new();
                self.id_url = None;
            }
            "properties" => self.props_depth = Some(self.depth),
            // The feed's own `<id>`, or an author's, is not an entity id.
            "id" if self.entry_depth == Some(self.depth - 1) => self.capture_id = true,
            // RFC 6721 tombstones. `ref` is the deleted entity's id URL, which
            // is the *only* thing the feed says about it, and `when` is the one
            // place a deletion's real time is ever available.
            "deleted-entry" => {
                let reference = atom_attr(e, "ref")?.ok_or_else(|| {
                    CdcError::Backend(anyhow::anyhow!(
                        "an OData v2 Atom `deleted-entry` carries no `ref` attribute, so the \
                         entity it deletes cannot be named. Skipping it would drop a deletion \
                         silently — the failure this whole Atom path exists to prevent — so the \
                         cycle stops here instead."
                    ))
                })?;
                let when = atom_attr(e, "when")?
                    .as_deref()
                    .and_then(parse_edm_datetime);
                self.page.rows.push(RowEvent {
                    deleted: true,
                    row: serde_json::Map::new(),
                    id_url: Some(reference),
                    deleted_at: when,
                });
            }
            // Only feed-level links are the cycle's. SAP spells the v2 delta
            // link `rel="delta"`; a gateway that uses a fully-qualified
            // relation URI ends it the same way.
            "link" if self.entry_depth.is_none() => {
                let rel = atom_attr(e, "rel")?.unwrap_or_default();
                if let Some(href) = atom_attr(e, "href")? {
                    if rel == "next" || rel.ends_with("/next") {
                        self.page.next_link = Some(href);
                    } else if rel == "delta" || rel.ends_with("/delta") {
                        self.page.delta_link = Some(href);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, local: &str) {
        if let Some(props_depth) = self.props_depth {
            if self.depth > props_depth {
                let object = self.objects.pop().unwrap_or_default();
                let name = self.names.pop().unwrap_or_default();
                let (edm_type, is_null) = self.types.pop().unwrap_or((None, false));
                // Child elements arrived, so this was a complex type and the
                // text buffer holds nothing but the last leaf's leftovers.
                let value = if object.is_empty() {
                    atom_value(&self.text, edm_type.as_deref(), is_null)
                } else {
                    Json::Object(object)
                };
                match self.objects.last_mut() {
                    Some(parent) => parent.insert(name, value),
                    None => self.row.insert(name, value),
                };
                self.text.clear();
                self.depth -= 1;
                return;
            }
            if self.depth == props_depth {
                self.props_depth = None;
            }
        }

        if self.capture_id && local == "id" {
            self.id_url = Some(self.text.trim().to_string());
            self.capture_id = false;
        }

        if local == "entry" && self.entry_depth == Some(self.depth) {
            self.entry_depth = None;
            self.page.rows.push(RowEvent {
                deleted: false,
                row: std::mem::take(&mut self.row),
                id_url: self.id_url.take(),
                deleted_at: None,
            });
        }

        self.text.clear();
        self.depth -= 1;
    }
}

fn atom_local(name: QName<'_>) -> String {
    String::from_utf8_lossy(name.local_name().as_ref()).into_owned()
}

/// An attribute by *local* name. Namespace prefixes are a service's choice —
/// `m:type` and `metadata:type` are the same attribute — so matching on the
/// prefix would make the parser depend on how a particular gateway declares its
/// namespaces.
fn atom_attr(e: &BytesStart<'_>, want: &str) -> Result<Option<String>, CdcError> {
    for attribute in e.attributes() {
        let attribute = attribute.map_err(|err| {
            CdcError::Backend(anyhow::anyhow!(
                "an attribute in the OData v2 Atom response is malformed: {err}"
            ))
        })?;
        if attribute.key.local_name().as_ref() == want.as_bytes() {
            let value = attribute.unescape_value().map_err(|err| {
                CdcError::Backend(anyhow::anyhow!(
                    "an attribute in the OData v2 Atom response is not valid XML text: {err}"
                ))
            })?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

/// One Atom leaf property, typed by its `m:type`.
///
/// **`Edm.Int64` and `Edm.Decimal` stay text on purpose.** OData v2's own JSON
/// renders them as strings for the same reason: a JSON number cannot hold them
/// without losing digits, and a key value that loses digits is an envelope id
/// that merges records. Everything unrecognised also stays text, which is the
/// safe direction — [`key_text`] canonicalises to text anyway, so a
/// conservative mapping never changes an id.
fn atom_value(text: &str, edm_type: Option<&str>, is_null: bool) -> Json {
    if is_null {
        return Json::Null;
    }
    let trimmed = text.trim();
    match edm_type.unwrap_or("Edm.String") {
        "Edm.Boolean" => match trimmed {
            "true" => Json::Bool(true),
            "false" => Json::Bool(false),
            other => Json::String(other.to_string()),
        },
        "Edm.Byte" | "Edm.SByte" | "Edm.Int16" | "Edm.Int32" => trimmed
            .parse::<i64>()
            .map(Json::from)
            .unwrap_or_else(|_| Json::String(text.to_string())),
        "Edm.Single" | "Edm.Double" => trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Json::Number)
            .unwrap_or_else(|| Json::String(text.to_string())),
        "Edm.DateTime" | "Edm.DateTimeOffset" => match parse_edm_datetime(trimmed) {
            Some(ts) => Json::String(ts.to_rfc3339()),
            None => Json::String(text.to_string()),
        },
        _ => Json::String(text.to_string()),
    }
}

/// Every shape SAP renders an instant in, normalised to one.
///
/// v2 JSON says `/Date(1700000000000)/`, v2 Atom says the offset-less
/// `2026-07-31T09:00:00` — `Edm.DateTime` has no time zone and SAP means UTC —
/// and v4 says RFC 3339. Normalising all three in one place is what makes
/// `changed_at` mean the same thing whichever dialect a service speaks, instead
/// of one dialect silently falling back to our poll clock.
fn parse_edm_datetime(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();

    if let Some(rest) = text
        .strip_prefix("/Date(")
        .and_then(|r| r.strip_suffix(")/"))
    {
        // v2 sometimes appends a timezone offset: `/Date(1700000000000+0060)/`.
        let millis = rest.split(['+', '-']).next().unwrap_or(rest).trim();
        return millis
            .parse::<i64>()
            .ok()
            .and_then(DateTime::<Utc>::from_timestamp_millis);
    }

    if let Ok(ts) = DateTime::parse_from_rfc3339(text) {
        return Some(ts.with_timezone(&Utc));
    }

    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Some(naive.and_utc());
        }
    }
    None
}

/// OData v4: `{"value": [...], "@odata.nextLink": "...", "@odata.deltaLink": "..."}`.
fn parse_v4(body: &Json) -> Result<Page, CdcError> {
    let mut page = Page {
        next_link: body
            .get("@odata.nextLink")
            .and_then(Json::as_str)
            .map(str::to_string),
        delta_link: body
            .get("@odata.deltaLink")
            .and_then(Json::as_str)
            .map(str::to_string),
        ..Page::default()
    };

    let value = body
        .get("value")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();

    for entry in value {
        let object = entry.as_object().ok_or_else(|| {
            CdcError::Backend(anyhow::anyhow!("an OData v4 value entry is not an object"))
        })?;
        let mut row = serde_json::Map::new();
        let mut deleted = false;
        let mut id_url = object
            .get("@id")
            .or_else(|| object.get("@odata.id"))
            .and_then(Json::as_str)
            .map(str::to_string);

        for (key, value) in object {
            if key == "@removed" || key == "@odata.removed" {
                deleted = true;
                continue;
            }
            if key.starts_with('@') {
                continue;
            }
            row.insert(key.clone(), value.clone());
        }

        // OData 4.0 spells a removed entry's identity as a plain `id` property.
        // Only trusted on a tombstone: on a live row, `id` is far more likely to
        // be a real business property.
        if deleted && id_url.is_none() {
            id_url = object.get("id").and_then(Json::as_str).map(str::to_string);
        }

        page.rows.push(RowEvent {
            deleted,
            row,
            id_url,
            // v4 tombstones name no time. `@removed` carries only a `reason`.
            deleted_at: None,
        });
    }

    Ok(page)
}

// ─────────────────────────────────────────────────────────────────────────────
// The source
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the feed needs, owned, so the returned stream is `'static`.
struct Inner {
    client: AuthedClient,
    service_root: Url,
    entity_set: String,
    entity: String,
    version: SapODataVersion,
    key_properties: Vec<String>,
    changed_at_property: Option<String>,
    authorized_tokens: Vec<String>,
    poll_interval: Duration,
}

/// One SAP entity set, replicated over OData delta.
pub struct SapSource {
    inner: Arc<Inner>,
}

impl SapSource {
    /// Open a source. Resolves credentials and builds the HTTP client, so a
    /// misconfigured deployment fails here rather than at the first poll.
    ///
    /// `key_properties` is **required and not discovered from `$metadata`**.
    /// Discovery would be easy — `sap-cdc-mcp` has the CSDL parser — and it is
    /// deliberately not used, because the key set decides the envelope id and
    /// the envelope id is a permanent identity. Deriving it from a document the
    /// SAP system rewrites on upgrade means an upgrade can silently change
    /// every id in the mesh, forking every aggregate, with nothing in the
    /// connector's configuration having changed. Naming the key in the config
    /// makes that an operator decision with a diff attached.
    #[allow(clippy::too_many_arguments)]
    pub async fn open(
        service_root: &str,
        entity_set: &str,
        entity: &str,
        version: SapODataVersion,
        key_properties: &[String],
        changed_at_property: Option<&str>,
        authorized_tokens: &[String],
        auth: &SapAuthConfig,
        poll_interval: Duration,
    ) -> Result<Self, CdcError> {
        if key_properties.is_empty() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "source.key_properties is empty; without it there is no envelope id. Take the \
                 key from the service's $metadata <Key><PropertyRef …> for {entity_set}."
            )));
        }
        if key_properties.iter().any(|k| k.trim().is_empty()) {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "source.key_properties contains an empty name"
            )));
        }

        let service_root = Url::parse(service_root).map_err(|e| {
            CdcError::Backend(anyhow::anyhow!(
                "source.service_root {service_root:?} is not a URL: {e}"
            ))
        })?;
        if service_root.cannot_be_a_base() {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "source.service_root {service_root} has no path to append an entity set to"
            )));
        }

        let client = AuthedClient::new(SapAuth::resolve(auth)?)?;

        Ok(Self {
            inner: Arc::new(Inner {
                client,
                service_root,
                entity_set: entity_set.to_string(),
                entity: entity.to_string(),
                version,
                key_properties: key_properties.to_vec(),
                changed_at_property: changed_at_property.map(str::to_string),
                auth: authorized_tokens.to_vec().into(),
                poll_interval,
            }),
        })
    }
}

impl Inner {
    /// The cold-start request: the whole entity set, with change tracking on.
    ///
    /// **No `$top`.** `sap-cdc-mcp` sends one as a page size and that is wrong
    /// for a CDC connector: OData's `$top` bounds the *whole* result, not a
    /// page, so a `$top` on the initial read silently truncates history at that
    /// row and the connector then streams forward from a snapshot that never
    /// finished. Paging is server-driven — the service hands back a `rel="next"`
    /// link / `@odata.nextLink` and this module follows it until there is none.
    ///
    /// **No `$format` either.** An earlier cut appended `$format=json` on v2,
    /// which is one of the options SAP documents as mutually exclusive with a v2
    /// delta query — and this is the read that issues the delta token, so
    /// pinning JSON on it is asking for a cursor the service may decline to give
    /// and a deletion representation that does not exist. Format is negotiated
    /// with `Accept` in [`Inner::fetch_page`], per dialect.
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
        Ok(url)
    }

    /// Fetch one page.
    ///
    /// `position` is the delta token the cycle is following, or `None` on the
    /// cold-start read. It is what decides whether a rejection is an unusable
    /// *position* or merely a backend failure: on a cold read there is no
    /// position for the service to be objecting to.
    async fn fetch_page(&self, url: &Url, position: Option<&str>) -> Result<Page, CdcError> {
        // The dialects genuinely differ here and must not be unified. A v4
        // delta response spells a deletion in JSON; a v2 one can only spell it
        // in Atom, so asking a v2 service for JSON is asking it for a feed with
        // the deletions removed and no note saying so.
        let accept = match self.version {
            SapODataVersion::V2 => "application/atom+xml,application/xml;q=0.9",
            SapODataVersion::V4 => "application/json",
        };
        let request = self
            .client
            .client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, accept)
            // v4's change-tracking opt-in. Harmless on v2, where tracking is a
            // property of the service, and sending it unconditionally keeps the
            // two paths identical.
            .header("Prefer", "odata.track-changes");
        let request = self.client.authorize(request).await?;

        let response = request
            .send()
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("requesting {}: {e}", redact(url))))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(classify_failure(status, &body, position, url));
        }

        let body = response.text().await.map_err(|e| {
            CdcError::Backend(anyhow::anyhow!(
                "reading the OData response from {}: {e}",
                redact(url)
            ))
        })?;

        match self.version {
            SapODataVersion::V2 => parse_v2_atom(&body),
            SapODataVersion::V4 => {
                let body: Json = serde_json::from_str(&body).map_err(|e| {
                    CdcError::Backend(anyhow::anyhow!(
                        "parsing the OData response from {}: {e}",
                        redact(url)
                    ))
                })?;
                parse_v4(&body)
            }
        }
    }

    /// Turn one row into a change record.
    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        event: &RowEvent,
        op: Op,
        snapshot: Snapshot,
        position: Option<String>,
        read_from: Option<&str>,
        next_delta_token: &str,
    ) -> Result<ChangeRecord, CdcError> {
        let key = SapKey::from_row(
            &self.entity_set,
            &self.key_properties,
            &event.row,
            event.id_url.as_deref(),
        )?;

        let (changed_at, changed_at_source) = self.changed_at(event);

        let mut payload: Stash = event.row.clone();
        // A silent overwrite here would put connector bookkeeping where a
        // business property used to be, and nothing downstream could tell.
        if payload.contains_key(ENVELOPE_META_KEY) {
            return Err(CdcError::Backend(anyhow::anyhow!(
                "{} has a property named {ENVELOPE_META_KEY:?}, which collides with the \
                 reserved key merkql-connect uses to carry the entity set, delta token and \
                 change timestamp into the payload. Rename the property with $select, or \
                 replicate this entity set with a different connector.",
                self.entity_set
            )));
        }
        payload.insert(
            ENVELOPE_META_KEY.to_string(),
            json!({
                "entity_set": self.entity_set,
                "odata_version": self.version.as_str(),
                "key": key.parts(),
                "op": if event.deleted { "delete" } else { "upsert" },
                "read_from_delta_token": read_from,
                "next_delta_token": next_delta_token,
                "changed_at": changed_at.to_rfc3339(),
                "changed_at_source": changed_at_source,
            }),
        );

        let mut envelope =
            Envelope::new(key.envelope_id(), payload, self.authorized_tokens.clone());
        // meshql has no delete operation: a deletion is a new envelope version
        // carrying the flag, which reaches the topic as an ordinary create.
        envelope.deleted = event.deleted;
        // `created_at` is meshql's ordering key, so it must be the domain's
        // time whenever the domain gave us one.
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

    /// The change timestamp, and an honest label for where it came from.
    ///
    /// SAP does not put a commit time on a delta row. If the entity has a
    /// last-changed property the config can name it, and then `source.ts_ms` is
    /// really the store's time. Otherwise it is *our* observation time, and the
    /// payload says so — because a consumer doing temporal reasoning needs to
    /// know it is looking at poll lag rather than business time, and a
    /// connector that reported one as the other would be undetectably wrong.
    ///
    /// A tombstone has no properties for `changed_at_property` to name, so the
    /// only chance at its real time is the Atom `<at:deleted-entry when="…">`
    /// attribute — labelled `tombstone`, because it is the service's word for
    /// when the row went, not for when the row last changed.
    fn changed_at(&self, event: &RowEvent) -> (DateTime<Utc>, &'static str) {
        if let Some(ts) = self
            .changed_at_property
            .as_deref()
            .and_then(|name| event.row.get(name))
            .and_then(parse_timestamp)
        {
            return (ts, "entity");
        }
        match event.deleted_at {
            Some(ts) => (ts, "tombstone"),
            None => (Utc::now(), "observed"),
        }
    }
}

fn parse_timestamp(value: &Json) -> Option<DateTime<Utc>> {
    if let Some(ms) = value.as_i64() {
        return DateTime::<Utc>::from_timestamp_millis(ms);
    }
    parse_edm_datetime(value.as_str()?)
}

/// A URL with its query stripped. Delta links carry the token, and a token is
/// close enough to a credential that it does not belong in a log line.
fn redact(url: &Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    format!("{url}?…")
}

/// Decide whether the service rejected *our stored position* or merely failed.
///
/// Getting this wrong is expensive in both directions, exactly as it is for
/// Mongo's resume token: calling a transient 503 an unusable position
/// re-replicates the whole entity set, while calling an expired token transient
/// retries a doomed request forever and delivers nothing while looking healthy.
///
/// `410 Gone` is what the OData spec says a service returns for a delta link it
/// no longer honours. SAP Gateway is not consistent about it and also answers
/// `400`, `404` or `412` with an error body naming the token, so the body is
/// checked too — but only when we were actually following a token. A rejection
/// on the cold-start read cannot be about a position we did not send.
fn classify_failure(status: StatusCode, body: &str, position: Option<&str>, url: &Url) -> CdcError {
    let token_rejected = status == StatusCode::GONE
        || (matches!(status.as_u16(), 400 | 404 | 412) && mentions_delta_token(body));

    match (token_rejected, position) {
        (true, Some(position)) => CdcError::UnusablePosition {
            connector: CONNECTOR,
            position: position.to_string(),
            reason: format!(
                "SAP answered {status} for the stored delta link; the token has aged past the \
                 service's change-tracking retention window, or the system was copied or \
                 upgraded since it was issued"
            ),
        },
        (true, None) => CdcError::Backend(anyhow::anyhow!(
            "SAP answered {status} for the initial tracked read of {}. This is not an unusable \
             position — no delta token was sent — so it is a service or authorisation problem.",
            redact(url)
        )),
        _ => CdcError::Backend(anyhow::anyhow!(
            "SAP answered {status} for {}: {}",
            redact(url),
            body.chars().take(400).collect::<String>()
        )),
    }
}

/// Why a link cannot be followed as an OData **v2** delta read, if it cannot.
///
/// SAP documents v2 delta queries as mutually exclusive with JSON format,
/// `$skiptoken`, `$top`, `$skip` and `$expand`. This connector adds none of them
/// — see [`Inner::initial_url`] — but a delta link is a string the *service*
/// composed and the offset store then kept, possibly across a gateway upgrade,
/// so it is read before it is followed.
///
/// `$format` is the one that matters. A v2 delta link pinning JSON puts the
/// connector back exactly where this module started: seeing every change except
/// the deletions, and reporting nothing wrong. `$top` and `$skip` truncate a
/// cycle, which is a gap rather than a duplicate. `$expand` changes the row
/// shape mid-replication.
///
/// `$skiptoken` is deliberately **not** rejected. A v2 delta response is
/// unpaged, so it should never appear — but if a gateway sends one anyway,
/// following it is doing what the service asked, and the paging loop in
/// [`Feed::cycle`] already handles it.
fn v2_delta_link_objection(url: &Url) -> Option<String> {
    for (name, value) in url.query_pairs() {
        match name.as_ref() {
            "$format" if !matches!(value.as_ref(), "atom" | "xml") => {
                return Some(format!(
                    "it pins $format={value}, and an OData v2 delta response cannot represent a \
                     deletion in anything but Atom"
                ));
            }
            "$top" | "$skip" | "$expand" => {
                return Some(format!(
                    "it carries {name}, which SAP documents as mutually exclusive with a v2 \
                     delta query"
                ));
            }
            _ => {}
        }
    }
    None
}

fn mentions_delta_token(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    ["delta token", "deltatoken", "delta_token", "delta link"]
        .iter()
        .any(|needle| lower.contains(needle))
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
    /// The delta token `next_url` came from, or `None` for the cold-start read.
    position: Option<String>,
    /// The next cycle is the initial tracked read.
    initial: bool,
    /// Emit the initial read's rows, or only harvest its delta token.
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

    /// One delta cycle: follow `next_url` through every page, then turn the
    /// whole thing into records.
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

        // No delta link means the service is not tracking changes for this
        // entity set. Without one the connector has nowhere to go next, and the
        // only thing it *could* do is re-read the entity set forever, emitting
        // every row on every cycle — a connector that looks healthy and floods
        // the topic with duplicates of history. Fatal, and named.
        let Some(delta_link) = delta_link else {
            return Err(CdcError::NoFeed {
                connector: CONNECTOR,
                reason: format!(
                    "{} returned no delta link. The service is not change-tracking this entity \
                     set, so there is no cursor to continue from and merkql-connect will not \
                     silently degrade into re-reading the whole entity set on every poll. \
                     Enable delta/change tracking for the entity set, or replicate it through \
                     an SLT or Debezium source instead.",
                    self.inner.entity_set
                ),
            });
        };

        let read_from = self.position.clone();
        let was_initial = self.initial;
        let emit = !was_initial || self.emit_initial_rows;

        if emit {
            let last = rows.len().saturating_sub(1);
            for (i, event) in rows.iter().enumerate() {
                // A snapshot cycle emits `r`; every later cycle is live traffic
                // and emits `c`, including tombstones — meshql spells a delete
                // as a new version with `deleted: true`, not as an `op: d`.
                let op = if was_initial { Op::Read } else { Op::Create };
                let snapshot = match (was_initial, i == last) {
                    (false, _) => Snapshot::False,
                    (true, false) => Snapshot::True,
                    (true, true) => Snapshot::Last,
                };
                // Only the final record of a cycle names a resumable place. See
                // the module docs: a delta cycle has no interior positions, so
                // an earlier record carrying one would let a restart resume
                // past changes it never appended.
                let position = (i == last).then(|| delta_link.clone());

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

        // The new token becomes the cursor whether or not anything was emitted.
        //
        // When a cycle is empty there is no record to hang it on, so it is only
        // held in memory and the offset store keeps the older token. That is
        // safe — SAP does not invalidate a token by issuing its successor, so
        // a restart resumes from the older one and replays the (empty)
        // interval. It is also the one place this source is weaker than the
        // others: an entity set that is idle for longer than the retention
        // window restarts onto an expired token, which surfaces as
        // `UnusablePosition` and needs `snapshot_mode = "when_needed"` to
        // recover. `CommitSource` has no way to commit a position without a
        // record, so there is nothing better available here.
        let next_url = self.resolve(&delta_link)?;
        if self.inner.version == SapODataVersion::V2 {
            if let Some(objection) = v2_delta_link_objection(&next_url) {
                return Err(CdcError::Backend(anyhow::anyhow!(
                    "{} handed back a link that cannot be followed as an OData v2 delta read: \
                     {objection}. Following it anyway would keep the connector running while \
                     deletions stopped arriving, which is the one thing merkql-connect will not \
                     do.",
                    self.inner.entity_set
                )));
            }
        }
        self.next_url = next_url;
        self.position = Some(delta_link);
        self.initial = false;
        Ok(())
    }

    /// Resolve a `__next` / `__delta` link, which a service may return relative.
    fn resolve(&self, link: &str) -> Result<Url, CdcError> {
        Url::parse(link)
            .or_else(|_| self.inner.service_root.join(link))
            .map_err(|e| {
                CdcError::Backend(anyhow::anyhow!(
                    "{} returned the unusable link {link:?}: {e}",
                    self.inner.entity_set
                ))
            })
    }
}

#[async_trait]
impl CommitSource for SapSource {
    fn connector(&self) -> &'static str {
        CONNECTOR
    }

    fn entity(&self) -> &str {
        &self.inner.entity
    }

    async fn changes(&self, from: Resume, mode: SnapshotMode) -> Result<ChangeStream, CdcError> {
        // OData's delta protocol produces no mid-snapshot position, so this
        // collapses to a cold start.
        //
        // A delta token is issued only as the *tail* of a completed read — the
        // `rel="delta"` link on the final page. Interior pages of the initial
        // read carry a `rel="next"` skiptoken, which is a paging handle, not a
        // resumable change position: it is not accepted where a delta token
        // goes, and `validate_stored_position` rejects it precisely so that a
        // paging handle can never be mistaken for a cursor. So an interrupted
        // initial read has nothing durable to resume from and must be redone.
        //
        // Resuming the *paging* would be possible if the next-link were
        // persisted, but that link is a server-side cursor with its own
        // undocumented lifetime, and a stale one silently returns a partial
        // set. Redoing the read is slower and correct; the module docs already
        // record that "re-baseline is normal operation" for this connector.
        let from = from.without_snapshot_resume();

        let (next_url, position, initial) = match &from {
            // Collapsed to `Cold` by `without_snapshot_resume` immediately
            // above. Named rather than wildcarded so that removing that call
            // is a compile error here instead of a silent behaviour change.
            Resume::Snapshotting(_) => {
                unreachable!("Resume::Snapshotting was collapsed to Cold before this match")
            }
            Resume::Cold => (self.inner.initial_url()?, None, true),
            Resume::At(link) => {
                let url = self.validate_stored_position(link)?;
                (url, Some(link.clone()), false)
            }
        };

        // `SnapshotMode::Never` still has to make the cold-start request:
        // OData's delta protocol issues a token only as the tail of a read, so
        // there is no "give me a cursor for now" call to make instead. The rows
        // are fetched and dropped, which costs one full read the operator did
        // not want but never manufactures a token — and a manufactured token is
        // a silent skip of everything between it and reality.
        let emit_initial_rows = mode.snapshots_on_cold_start();

        let feed = Feed {
            inner: self.inner.clone(),
            pending: VecDeque::new(),
            next_url,
            position,
            initial,
            emit_initial_rows,
            idle: false,
            done: false,
        };

        Ok(Box::pin(stream::unfold(feed, |mut feed| async move {
            feed.step().await.map(|item| (item, feed))
        })))
    }

    /// Nothing to release. Unlike a PostgreSQL replication slot, a delta token
    /// is a cursor the *client* holds; SAP retains change history on its own
    /// schedule and is not waiting to be told what we made durable.
    async fn durable_through(&self, _position: &str) -> Result<(), CdcError> {
        Ok(())
    }
}

impl SapSource {
    /// A stored position must be a delta link this connector could have been
    /// given.
    ///
    /// The host check is the load-bearing part. Repoint `service_root` at a
    /// different system — a copy-back from production into QA is the usual way
    /// — and the offset file still holds the old system's delta link. Following
    /// it would replicate the *old* system's changes onto the new system's
    /// topic, quietly, for as long as the old host stayed reachable. Reporting
    /// it as an unusable position instead lets `snapshot_mode` decide, which is
    /// the same treatment an expired token gets and the same treatment it
    /// deserves.
    fn validate_stored_position(&self, link: &str) -> Result<Url, CdcError> {
        let unusable = |reason: String| CdcError::UnusablePosition {
            connector: CONNECTOR,
            position: link.to_string(),
            reason,
        };

        let url = Url::parse(link).map_err(|e| {
            unusable(format!(
                "the stored position is not an absolute OData delta link ({e}); a delta link is \
                 what this connector persists, so the offset file was written by something else"
            ))
        })?;

        if url.host_str() != self.inner.service_root.host_str()
            || url.port_or_known_default() != self.inner.service_root.port_or_known_default()
        {
            return Err(unusable(format!(
                "the stored delta link points at {}, but source.service_root is {}. The \
                 connector was repointed at a different SAP system and the old system's \
                 token cannot name a position in the new one",
                url.host_str().unwrap_or("<no host>"),
                self.inner.service_root.host_str().unwrap_or("<no host>"),
            )));
        }

        if self.inner.version == SapODataVersion::V2 {
            if let Some(objection) = v2_delta_link_objection(&url) {
                return Err(unusable(format!(
                    "the stored position cannot be followed as an OData v2 delta read: \
                     {objection}. It was written by a build of this connector that requested \
                     JSON, under which no deletion was ever visible, so re-baselining is the \
                     only way to a correct topic"
                )));
            }
        }

        Ok(url)
    }
}

impl SapODataVersion {
    pub fn as_str(&self) -> &'static str {
        match self {
            SapODataVersion::V2 => "v2",
            SapODataVersion::V4 => "v4",
        }
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

    fn key_of(entity_set: &str, keys: &[&str], pairs: &[(&str, Json)]) -> String {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_string()).collect();
        SapKey::from_row(entity_set, &keys, &row(pairs), None)
            .expect("the key must form")
            .envelope_id()
    }

    // ── The composite-key encoding ──────────────────────────────────────

    /// The failure the whole encoding exists to prevent: a separator that can
    /// also occur inside a value merges two distinct business records into one
    /// aggregate, permanently and silently.
    #[test]
    fn a_separator_inside_a_value_cannot_forge_another_records_id() {
        let a = key_of(
            "A_SalesOrderItem",
            &["SalesOrder", "SalesOrderItem"],
            &[
                ("SalesOrder", json!("10,SalesOrderItem='20")),
                ("SalesOrderItem", json!("30")),
            ],
        );
        let b = key_of(
            "A_SalesOrderItem",
            &["SalesOrder", "SalesOrderItem"],
            &[("SalesOrder", json!("10")), ("SalesOrderItem", json!("20"))],
        );
        assert_ne!(
            a, b,
            "a value containing the separators must not forge an id"
        );
    }

    /// A quote inside a value is the other half of the same attack, and the
    /// doubled-quote escape is what closes it.
    #[test]
    fn a_quote_inside_a_value_is_escaped_rather_than_ending_the_literal() {
        let quoted = key_of(
            "A_BusinessPartner",
            &["BusinessPartner"],
            &[("BusinessPartner", json!("O'Neill"))],
        );
        assert_eq!(quoted, "A_BusinessPartner(BusinessPartner='O''Neill')");

        let forged = key_of(
            "A_BusinessPartner",
            &["BusinessPartner"],
            &[("BusinessPartner", json!("O''Neill"))],
        );
        assert_ne!(quoted, forged);
    }

    /// Exhaustive over a small alphabet of nasty values: no two distinct
    /// composite keys may share an id. This is the property, not an example of
    /// it.
    #[test]
    fn distinct_composite_keys_never_share_an_envelope_id() {
        let values = [
            "", "a", "b", "a'", "'a", "a''", "a,b", "a=b", "a)b", "a(b", "a'),b='c", "1", "01",
        ];
        let mut seen: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        for first in values {
            for second in values {
                let id = key_of(
                    "A_BusinessPartnerAddress",
                    &["BusinessPartner", "AddressID"],
                    &[
                        ("BusinessPartner", json!(first)),
                        ("AddressID", json!(second)),
                    ],
                );
                let key = (first.to_string(), second.to_string());
                if let Some(clash) = seen.insert(id.clone(), key.clone()) {
                    panic!("{key:?} and {clash:?} both encode to {id}");
                }
            }
        }
        assert_eq!(seen.len(), values.len() * values.len());
    }

    /// The id must not depend on the order the properties happened to arrive
    /// in, because `$metadata` and a JSON payload can order them differently
    /// and SAP can reorder them across a release upgrade. An id that moves
    /// forks every aggregate that carries it.
    #[test]
    fn the_id_is_independent_of_property_order() {
        let forwards = key_of(
            "A_BusinessPartnerAddress",
            &["BusinessPartner", "AddressID"],
            &[("BusinessPartner", json!("1")), ("AddressID", json!("2"))],
        );
        let backwards = key_of(
            "A_BusinessPartnerAddress",
            &["AddressID", "BusinessPartner"],
            &[("AddressID", json!("2")), ("BusinessPartner", json!("1"))],
        );
        assert_eq!(forwards, backwards);
        assert_eq!(
            forwards,
            "A_BusinessPartnerAddress(AddressID='2',BusinessPartner='1')"
        );
    }

    /// v2 renders `Edm.Int32` as a JSON string and v4 as a JSON number. The
    /// same record read over the two protocols must get the same envelope id.
    #[test]
    fn the_id_is_invariant_across_odata_v2_and_v4_json_typing() {
        let v2 = key_of(
            "A_SalesOrder",
            &["SalesOrder"],
            &[("SalesOrder", json!("10"))],
        );
        let v4 = key_of(
            "A_SalesOrder",
            &["SalesOrder"],
            &[("SalesOrder", json!(10))],
        );
        assert_eq!(v2, v4);
    }

    /// Two entity sets sharing a key name must not share ids.
    #[test]
    fn the_entity_set_is_part_of_the_id() {
        let a = key_of(
            "A_BusinessPartner",
            &["BusinessPartner"],
            &[("BusinessPartner", json!("1"))],
        );
        let b = key_of(
            "A_BusinessPartnerBank",
            &["BusinessPartner"],
            &[("BusinessPartner", json!("1"))],
        );
        assert_ne!(a, b);
    }

    /// A partial key must stop the record, not produce an id. This is the
    /// silent-merge failure in its most likely real form: a `$select` that
    /// forgot a key property.
    #[test]
    fn a_missing_key_property_is_refused_rather_than_guessed() {
        let keys = ["BusinessPartner".to_string(), "AddressID".to_string()];
        let err = SapKey::from_row(
            "A_BusinessPartnerAddress",
            &keys,
            &row(&[("BusinessPartner", json!("1"))]),
            None,
        )
        .expect_err("a partial key must not produce an envelope id");
        assert!(err.to_string().contains("AddressID"), "got: {err}");
    }

    #[test]
    fn a_null_key_property_is_refused() {
        let keys = ["BusinessPartner".to_string()];
        let err = SapKey::from_row(
            "A_BusinessPartner",
            &keys,
            &row(&[("BusinessPartner", Json::Null)]),
            None,
        )
        .expect_err("a null key must be refused");
        assert!(err.to_string().contains("non-nullable"), "got: {err}");
    }

    // ── Key predicates from tombstones ──────────────────────────────────

    /// A v4 tombstone carries no properties at all, only an id URL. Its
    /// envelope id must be byte-identical to the one the upsert produced, or
    /// the delete lands on a different aggregate than the record it deletes.
    #[test]
    fn a_tombstones_id_url_produces_the_same_id_as_the_row_did() {
        let keys = vec!["SalesOrder".to_string(), "SalesOrderItem".to_string()];
        let from_row = SapKey::from_row(
            "A_SalesOrderItem",
            &keys,
            &row(&[("SalesOrder", json!("10")), ("SalesOrderItem", json!("20"))]),
            None,
        )
        .unwrap();
        let from_url = SapKey::from_row(
            "A_SalesOrderItem",
            &keys,
            &serde_json::Map::new(),
            Some("https://s4.example.com/sap/opu/odata4/x/A_SalesOrderItem(SalesOrder='10',SalesOrderItem='20')"),
        )
        .unwrap();
        assert_eq!(from_row.envelope_id(), from_url.envelope_id());
    }

    /// The single-key shorthand omits the property name; the configured key is
    /// what supplies it.
    #[test]
    fn the_unnamed_single_key_predicate_form_is_understood() {
        let keys = vec!["BusinessPartner".to_string()];
        let key = SapKey::from_row(
            "A_BusinessPartner",
            &keys,
            &serde_json::Map::new(),
            Some("A_BusinessPartner('1000')"),
        )
        .unwrap();
        assert_eq!(
            key.envelope_id(),
            "A_BusinessPartner(BusinessPartner='1000')"
        );
    }

    /// A percent-encoded key in a URL and the raw value in a row payload are
    /// the same business key, so they must produce the same id — otherwise one
    /// record acquires two aggregates.
    #[test]
    fn a_percent_encoded_key_matches_its_raw_form() {
        let keys = vec!["MaterialText".to_string()];
        let from_row = SapKey::from_row(
            "A_Material",
            &keys,
            &row(&[("MaterialText", json!("steel rod"))]),
            None,
        )
        .unwrap();
        let from_url = SapKey::from_row(
            "A_Material",
            &keys,
            &serde_json::Map::new(),
            Some("A_Material('steel%20rod')"),
        )
        .unwrap();
        assert_eq!(from_row.envelope_id(), from_url.envelope_id());
    }

    /// A predicate naming something the config did not is a mismatch between
    /// the connector and the service, not a key to be used.
    #[test]
    fn a_predicate_that_disagrees_with_the_configured_key_is_refused() {
        let keys = vec!["BusinessPartner".to_string()];
        let err = SapKey::from_row(
            "A_BusinessPartner",
            &keys,
            &serde_json::Map::new(),
            Some("A_BusinessPartner(Customer='1')"),
        )
        .expect_err("a predicate naming an unconfigured property must be refused");
        assert!(err.to_string().contains("key_properties"), "got: {err}");
    }

    // ── Page parsing ────────────────────────────────────────────────────

    const V2_FEED: &str = r#"<?xml version="1.0" encoding="utf-8"?>
        <feed xmlns="http://www.w3.org/2005/Atom"
              xmlns:m="http://schemas.microsoft.com/ado/2007/08/dataservices/metadata"
              xmlns:d="http://schemas.microsoft.com/ado/2007/08/dataservices"
              xmlns:at="http://purl.org/atom/tombstones/1.0">
          <id>https://s4.example.com/svc/A_BusinessPartner</id>
          <link rel="next" href="https://s4.example.com/next"/>
          <link rel="delta" href="https://s4.example.com/delta?!deltatoken=D1"/>
          <entry>
            <id>https://s4.example.com/svc/A_BusinessPartner('1')</id>
            <link rel="edit" href="A_BusinessPartner('1')"/>
            <link rel="http://…/related/ToAddress" href="A_BusinessPartner('1')/ToAddress"/>
            <content type="application/xml">
              <m:properties>
                <d:BusinessPartner>1</d:BusinessPartner>
                <d:BusinessPartnerName>ACME</d:BusinessPartnerName>
                <d:CreditLimit m:type="Edm.Int32">4200</d:CreditLimit>
                <d:LastChangeDateTime m:type="Edm.DateTime">2023-11-14T22:13:20</d:LastChangeDateTime>
                <d:MiddleName m:null="true"/>
              </m:properties>
            </content>
          </entry>
          <at:deleted-entry
            ref="https://s4.example.com/svc/A_BusinessPartner('2')"
            when="2026-07-31T10:00:00Z"/>
        </feed>"#;

    /// The whole point of the Atom path: a v2 feed's rows, its paging and delta
    /// links, and — the thing JSON could never have carried — its tombstone.
    #[test]
    fn a_v2_atom_feed_yields_rows_links_and_deleted_entries() {
        let page = parse_v2_atom(V2_FEED).unwrap();
        assert_eq!(page.rows.len(), 2);

        assert!(!page.rows[0].deleted);
        assert_eq!(page.rows[0].row.get("BusinessPartner"), Some(&json!("1")));
        assert_eq!(
            page.rows[0].id_url.as_deref(),
            Some("https://s4.example.com/svc/A_BusinessPartner('1')")
        );

        let deleted = &page.rows[1];
        assert!(deleted.deleted, "a deleted-entry must be a tombstone");
        assert_eq!(
            deleted.id_url.as_deref(),
            Some("https://s4.example.com/svc/A_BusinessPartner('2')")
        );
        assert_eq!(
            deleted.deleted_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-07-31T10:00:00+00:00")
        );

        // Only the feed's own links are the cycle's; the entry's `edit` and
        // navigation links must not be mistaken for one.
        assert_eq!(
            page.next_link.as_deref(),
            Some("https://s4.example.com/next")
        );
        assert_eq!(
            page.delta_link.as_deref(),
            Some("https://s4.example.com/delta?!deltatoken=D1")
        );
    }

    /// `m:type` decides the JSON shape, and `Edm.DateTime`'s offset-less text is
    /// normalised so `changed_at` parses identically under v2 and v4.
    #[test]
    fn atom_properties_are_typed_by_their_edm_type() {
        let row = &parse_v2_atom(V2_FEED).unwrap().rows[0].row;
        assert_eq!(row.get("CreditLimit"), Some(&json!(4200)));
        assert_eq!(row.get("MiddleName"), Some(&Json::Null));
        let changed = row.get("LastChangeDateTime").unwrap();
        assert_eq!(changed, &json!("2023-11-14T22:13:20+00:00"));
        assert_eq!(
            parse_timestamp(changed).unwrap().timestamp_millis(),
            1_700_000_000_000
        );
    }

    /// `Edm.Int64` and `Edm.Decimal` do not fit a JSON number without losing
    /// digits, and a key value that loses digits is an envelope id that merges
    /// two records.
    #[test]
    fn wide_numeric_edm_types_stay_text() {
        let feed = r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><content><m:properties
            xmlns:m="m" xmlns:d="d">
              <d:Id m:type="Edm.Int64">9007199254740993</d:Id>
              <d:Amount m:type="Edm.Decimal">12345678901234567890.99</d:Amount>
            </m:properties></content></entry></feed>"#;
        let row = &parse_v2_atom(feed).unwrap().rows[0].row;
        assert_eq!(row.get("Id"), Some(&json!("9007199254740993")));
        assert_eq!(row.get("Amount"), Some(&json!("12345678901234567890.99")));
    }

    /// A complex property is an object, not a flattened mess that could collide
    /// with a sibling scalar of the same leaf name.
    #[test]
    fn complex_atom_properties_nest() {
        let feed = r#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><content><m:properties
            xmlns:m="m" xmlns:d="d">
              <d:Address><d:City>Leeds</d:City><d:Country>GB</d:Country></d:Address>
              <d:City>elsewhere</d:City>
            </m:properties></content></entry></feed>"#;
        let row = &parse_v2_atom(feed).unwrap().rows[0].row;
        assert_eq!(
            row.get("Address"),
            Some(&json!({"City":"Leeds","Country":"GB"}))
        );
        assert_eq!(row.get("City"), Some(&json!("elsewhere")));
    }

    /// The failure that motivated this whole path. A v2 service answering JSON
    /// cannot have told us about a deletion, so reading it would mean losing
    /// deletions forever while looking healthy.
    #[test]
    fn a_v2_json_body_is_refused_rather_than_read_without_its_deletions() {
        let err = parse_v2_atom(r#"{"d":{"results":[{"BusinessPartner":"1"}]}}"#)
            .expect_err("a v2 JSON body must be refused");
        assert!(err.to_string().contains("deletions"), "got: {err}");
    }

    /// A tombstone's `ref` is the only thing the feed says about the deleted
    /// entity. Skipping a malformed one would drop a deletion in silence.
    #[test]
    fn a_deleted_entry_without_a_ref_stops_the_cycle() {
        let feed = r#"<feed xmlns="http://www.w3.org/2005/Atom"
            xmlns:at="http://purl.org/atom/tombstones/1.0">
            <at:deleted-entry when="2026-07-31T10:00:00Z"/></feed>"#;
        let err = parse_v2_atom(feed).expect_err("an unnamed deletion must not be skipped");
        assert!(err.to_string().contains("deleted-entry"), "got: {err}");
    }

    /// A gateway error page or an HTML login redirect is XML-ish and would
    /// otherwise parse into an empty feed — which reads as "nothing changed".
    #[test]
    fn a_response_that_is_not_an_atom_feed_is_refused() {
        let err = parse_v2_atom("<html><body>Login required</body></html>")
            .expect_err("a non-feed body must be refused");
        assert!(err.to_string().contains("Atom"), "got: {err}");
    }

    /// A v2 tombstone must land on the id its upsert used, exactly as a v4 one
    /// must — the Atom `ref` is a full URL and the row's key is raw properties,
    /// so this is the encoding doing real work rather than a tautology.
    #[test]
    fn a_v2_tombstone_and_its_upsert_share_an_envelope_id() {
        let keys = vec!["BusinessPartner".to_string()];
        let page = parse_v2_atom(V2_FEED).unwrap();
        let upsert = SapKey::from_row(
            "A_BusinessPartner",
            &keys,
            &page.rows[0].row,
            page.rows[0].id_url.as_deref(),
        )
        .unwrap();
        let tombstone = SapKey::from_row(
            "A_BusinessPartner",
            &keys,
            &page.rows[1].row,
            page.rows[1].id_url.as_deref(),
        )
        .unwrap();
        assert_eq!(
            upsert.envelope_id(),
            "A_BusinessPartner(BusinessPartner='1')"
        );
        assert_eq!(
            tombstone.envelope_id(),
            "A_BusinessPartner(BusinessPartner='2')"
        );
    }

    /// A v2 delta link that pins JSON is the bug reintroducing itself, and it
    /// must not be followed however it got there.
    #[test]
    fn a_v2_delta_link_pinning_json_is_objected_to() {
        let json =
            Url::parse("https://s4.example.com/svc/A_X?$format=json&!deltatoken=D1").unwrap();
        assert!(v2_delta_link_objection(&json).is_some());

        let truncating = Url::parse("https://s4.example.com/svc/A_X?$top=100").unwrap();
        assert!(v2_delta_link_objection(&truncating).is_some());

        // What SAP actually hands back, plus the paging option v2 delta is
        // documented not to use but which is harmless to follow if it appears.
        let ok = Url::parse("https://s4.example.com/svc/A_X?!deltatoken=D1&$skiptoken=5").unwrap();
        assert!(v2_delta_link_objection(&ok).is_none());
    }

    #[test]
    fn v4_pages_yield_rows_links_and_removed_entries() {
        let body: Json = serde_json::from_str(
            r#"{"value":[
                {"SalesOrder":"10","OrderTotal":99.5},
                {"@removed":{"reason":"deleted"},"id":"A_SalesOrder('11')"}
            ],
            "@odata.deltaLink":"https://s4.example.com/delta?$deltatoken=D1"}"#,
        )
        .unwrap();
        let page = parse_v4(&body).unwrap();
        assert_eq!(page.rows.len(), 2);
        assert!(!page.rows[0].deleted);
        assert!(page.rows[1].deleted);
        assert_eq!(page.rows[1].id_url.as_deref(), Some("A_SalesOrder('11')"));
        assert!(page.next_link.is_none());
    }

    /// On a live row `id` is far more likely to be a business property than an
    /// OData identity, so it must stay in the payload and not be mistaken for
    /// an id URL.
    #[test]
    fn a_live_rows_id_property_is_not_treated_as_an_entity_id_url() {
        let body: Json =
            serde_json::from_str(r#"{"value":[{"SalesOrder":"10","id":"not-a-url"}]}"#).unwrap();
        let page = parse_v4(&body).unwrap();
        assert!(page.rows[0].id_url.is_none());
        assert_eq!(page.rows[0].row.get("id").unwrap(), &json!("not-a-url"));
    }

    // ── Unusable-position classification ────────────────────────────────

    #[test]
    fn a_410_on_a_stored_delta_link_is_an_unusable_position() {
        let url = Url::parse("https://s4.example.com/x").unwrap();
        let err = classify_failure(StatusCode::GONE, "", Some("https://s4/delta?t=1"), &url);
        assert!(
            matches!(err, CdcError::UnusablePosition { .. }),
            "got: {err}"
        );
    }

    /// SAP Gateway does not always use 410; a 400 whose body names the token is
    /// the same failure and must get the same policy, or `when_needed` cannot
    /// recover from the most common real form of it.
    #[test]
    fn a_400_naming_the_delta_token_is_an_unusable_position() {
        let url = Url::parse("https://s4.example.com/x").unwrap();
        let err = classify_failure(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"/IWBEP/CM_MGW_RT/023","message":"Delta token is no longer valid"}}"#,
            Some("https://s4/delta?t=1"),
            &url,
        );
        assert!(
            matches!(err, CdcError::UnusablePosition { .. }),
            "got: {err}"
        );
    }

    /// The other direction, which is the expensive one: a transient failure
    /// must not be reported as an unusable position, or a blip re-replicates
    /// the entire entity set.
    #[test]
    fn a_transient_failure_is_not_an_unusable_position() {
        let url = Url::parse("https://s4.example.com/x").unwrap();
        for status in [
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::UNAUTHORIZED,
            StatusCode::BAD_REQUEST,
        ] {
            let err = classify_failure(status, "upstream is having a day", Some("t"), &url);
            assert!(
                !matches!(err, CdcError::UnusablePosition { .. }),
                "{status} must not be an unusable position, got: {err}"
            );
        }
    }

    /// There was no token to reject, so a 410 on the cold read is a service
    /// problem. Reporting it as an unusable position would make `when_needed`
    /// re-snapshot in a loop against a service that is simply broken.
    #[test]
    fn a_410_on_the_cold_start_read_is_not_an_unusable_position() {
        let url = Url::parse("https://s4.example.com/x").unwrap();
        let err = classify_failure(StatusCode::GONE, "", None, &url);
        assert!(
            !matches!(err, CdcError::UnusablePosition { .. }),
            "got: {err}"
        );
    }

    /// The error text must never carry the query string, because the delta
    /// token lives there and it is close enough to a credential to keep out of
    /// logs.
    #[test]
    fn failure_messages_do_not_leak_the_delta_token() {
        let url = Url::parse("https://s4.example.com/delta?$deltatoken=SECRET").unwrap();
        let err = classify_failure(StatusCode::IM_A_TEAPOT, "nope", None, &url);
        assert!(!err.to_string().contains("SECRET"), "got: {err}");
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    #[test]
    fn quote_aware_scanning_ignores_separators_inside_literals() {
        assert_eq!(find_unquoted("a='b,c',d='e'", ','), Some(7));
        assert_eq!(
            split_unquoted("a='b,c',d='e'", ','),
            vec!["a='b,c'", "d='e'"]
        );
        // A doubled quote is an escape, not the end of the literal.
        assert_eq!(split_unquoted("a='b''c,d'", ','), vec!["a='b''c,d'"]);
    }

    #[test]
    fn secrets_do_not_print_themselves() {
        let secret = crate::sap_auth::Secret::new("hunter2");
        assert!(!format!("{secret:?}").contains("hunter2"));
    }
}
