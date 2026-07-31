//! SAP S/4HANA CDC over OData delta: the delta token *is* the cursor, and the
//! entity key predicate *is* the envelope id.
//!
//! # Where this logic came from, and why it is duplicated rather than shared
//!
//! The OData v2/v4 delta walk and all of the SAP auth modes below are
//! **deliberately duplicated from `tailoredshapes/sap-cdc-mcp`**
//! (`crates/sap-cdc/src/source/odata.rs` and `crates/sap-cdc/src/auth.rs`),
//! which already solved them for a different sink. Extracting them into a
//! crate both repos depend on is the architecturally cleaner move and it was
//! considered and rejected for this first cut:
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
//! - Two divergences below are *corrections*, not ports (see `$top` and key
//!   canonicalisation), and shipping them as a shared-crate change would mean
//!   changing `sap-cdc-mcp`'s behaviour as a side effect of writing a merkql
//!   connector.
//!
//! When a third consumer of SAP OData appears, extract then. Until then this
//! note is the pointer: **a fix to the delta walk or to auth probably belongs
//! in both places.**
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
use crate::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream;
use meshql_core::{Envelope, Stash};
use reqwest::{Client, ClientBuilder, RequestBuilder, StatusCode};
use serde_json::{json, Value as Json};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
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
// Secrets
// ─────────────────────────────────────────────────────────────────────────────

/// A string that does not print itself.
///
/// Credentials reach this module from the environment and then sit inside a
/// long-lived struct that participates in `{:?}` error context. `SecretString`
/// from the `secrecy` crate does the same job; a ten-line newtype does it
/// without a dependency the workspace does not otherwise carry.
#[derive(Clone)]
struct Secret(String);

impl Secret {
    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

fn env_var(name: &str) -> Result<String, CdcError> {
    std::env::var(name).map_err(|_| {
        CdcError::Backend(anyhow::anyhow!(
            "SAP auth needs environment variable {name}, which is unset. Credentials are \
             deliberately not readable from the connector TOML — the config names the \
             variable, the deployment supplies the value."
        ))
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Auth
// ─────────────────────────────────────────────────────────────────────────────

/// A resolved SAP auth strategy, with the secret material already pulled out of
/// the environment.
///
/// Ported from `sap-cdc-mcp`'s `auth.rs`; the four modes SAP deployments
/// actually use are Basic (on-prem gateway), OAuth2 client-credentials (BTP
/// destination service), OAuth2 SAML-bearer (principal propagation) and X.509
/// mTLS (BTP mTLS destinations). `Bearer` and `None` are here because an SLT
/// proxy in front of a private network is a real deployment and because the
/// tests need a mode with no ceremony.
///
/// Not `pub`: it holds live credentials, and a type holding live credentials
/// that anything outside this module can pattern-match is a credential one
/// `{:?}` away from a log file.
#[derive(Debug, Clone)]
enum SapAuth {
    /// No credentials. Only correct when something else — mTLS at a proxy, a
    /// private network — is doing the authenticating.
    None,
    Basic {
        user: String,
        pass: Secret,
    },
    Bearer {
        token: Secret,
    },
    Oauth2ClientCredentials {
        token_url: String,
        client_id: String,
        client_secret: Secret,
        scope: Option<String>,
    },
    Oauth2SamlBearer {
        token_url: String,
        assertion: Secret,
        client_id: String,
    },
    /// A PEM cert + key pair on disk. Not an env var, because a certificate is
    /// a file and stuffing PEM into the environment is how a private key ends
    /// up in a process listing.
    MTls {
        cert_pem: Vec<u8>,
    },
}

impl SapAuth {
    /// Resolve a config-declared mode into live credentials.
    fn resolve(config: &SapAuthConfig) -> Result<Self, CdcError> {
        Ok(match config {
            SapAuthConfig::None => SapAuth::None,
            SapAuthConfig::Basic { user_env, pass_env } => SapAuth::Basic {
                user: env_var(user_env)?,
                pass: Secret(env_var(pass_env)?),
            },
            SapAuthConfig::Bearer { token_env } => SapAuth::Bearer {
                token: Secret(env_var(token_env)?),
            },
            SapAuthConfig::Oauth2Cc {
                token_url,
                client_id_env,
                client_secret_env,
                scope,
            } => SapAuth::Oauth2ClientCredentials {
                token_url: token_url.clone(),
                client_id: env_var(client_id_env)?,
                client_secret: Secret(env_var(client_secret_env)?),
                scope: scope.clone(),
            },
            SapAuthConfig::Oauth2SamlBearer {
                token_url,
                assertion_env,
                client_id_env,
            } => SapAuth::Oauth2SamlBearer {
                token_url: token_url.clone(),
                assertion: Secret(env_var(assertion_env)?),
                client_id: env_var(client_id_env)?,
            },
            SapAuthConfig::Mtls {
                cert_path,
                key_path,
            } => SapAuth::MTls {
                cert_pem: read_identity_pem(cert_path, key_path)?,
            },
        })
    }
}

/// `reqwest::Identity::from_pem` wants the certificate and the key in one blob.
/// Read them at *startup* rather than at first request: a deployment with an
/// unreadable key should fail before it claims a merkql topic, not an hour
/// later on the first poll.
fn read_identity_pem(cert_path: &Path, key_path: &Path) -> Result<Vec<u8>, CdcError> {
    let mut pem = std::fs::read(cert_path).map_err(|e| {
        CdcError::Backend(anyhow::anyhow!(
            "reading mTLS certificate {}: {e}",
            cert_path.display()
        ))
    })?;
    let key = std::fs::read(key_path).map_err(|e| {
        CdcError::Backend(anyhow::anyhow!(
            "reading mTLS private key {}: {e}",
            key_path.display()
        ))
    })?;
    pem.push(b'\n');
    pem.extend_from_slice(&key);
    Ok(pem)
}

#[derive(Clone)]
struct CachedToken {
    bearer: Secret,
    expires_at: Instant,
}

/// A `reqwest::Client` that knows how to authenticate to SAP, and caches an
/// OAuth token until a minute before it expires.
struct AuthedClient {
    client: Client,
    auth: SapAuth,
    token: Mutex<Option<CachedToken>>,
}

impl AuthedClient {
    fn new(auth: SapAuth) -> Result<Self, CdcError> {
        let mut builder = ClientBuilder::new()
            .user_agent(concat!("merkql-connect/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(120));

        if let SapAuth::MTls { cert_pem } = &auth {
            let identity = reqwest::Identity::from_pem(cert_pem).map_err(|e| {
                CdcError::Backend(anyhow::anyhow!(
                    "the configured mTLS certificate and key are not a usable identity: {e}"
                ))
            })?;
            builder = builder.identity(identity);
        }

        let client = builder
            .build()
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("building the SAP HTTP client: {e}")))?;

        Ok(Self {
            client,
            auth,
            token: Mutex::new(None),
        })
    }

    /// Attach whatever this strategy needs to a request.
    async fn authorize(&self, request: RequestBuilder) -> Result<RequestBuilder, CdcError> {
        use base64::Engine;
        Ok(match &self.auth {
            // mTLS authenticates at the TLS handshake; there is no header.
            SapAuth::None | SapAuth::MTls { .. } => request,
            SapAuth::Basic { user, pass } => {
                let creds = base64::engine::general_purpose::STANDARD
                    .encode(format!("{user}:{}", pass.expose()));
                request.header(reqwest::header::AUTHORIZATION, format!("Basic {creds}"))
            }
            SapAuth::Bearer { token } => request.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", token.expose()),
            ),
            SapAuth::Oauth2ClientCredentials { .. } | SapAuth::Oauth2SamlBearer { .. } => {
                let bearer = self.oauth_token().await?;
                request.header(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", bearer.expose()),
                )
            }
        })
    }

    async fn oauth_token(&self) -> Result<Secret, CdcError> {
        // Refresh a minute early. A token that expires between the check and
        // the request comes back as a 401 that this module would classify as a
        // backend error, so the margin is what keeps a long-running connector
        // from a periodic self-inflicted outage.
        //
        // The guard is dropped before the await deliberately: holding a
        // `std::sync::Mutex` across an await point is how a runtime deadlocks.
        {
            let cached = self.token.lock().expect("token cache poisoned").clone();
            if let Some(cached) = cached {
                if cached.expires_at > Instant::now() + Duration::from_secs(60) {
                    return Ok(cached.bearer);
                }
            }
        }

        let (token_url, form): (&str, Vec<(&str, String)>) = match &self.auth {
            SapAuth::Oauth2ClientCredentials {
                token_url,
                client_id,
                client_secret,
                scope,
            } => {
                let mut form = vec![
                    ("grant_type", "client_credentials".to_string()),
                    ("client_id", client_id.clone()),
                    ("client_secret", client_secret.expose().to_string()),
                ];
                if let Some(scope) = scope.as_deref().filter(|s| !s.is_empty()) {
                    form.push(("scope", scope.to_string()));
                }
                (token_url, form)
            }
            SapAuth::Oauth2SamlBearer {
                token_url,
                assertion,
                client_id,
            } => (
                token_url,
                vec![
                    (
                        "grant_type",
                        "urn:ietf:params:oauth:grant-type:saml2-bearer".to_string(),
                    ),
                    ("assertion", assertion.expose().to_string()),
                    ("client_id", client_id.clone()),
                ],
            ),
            _ => {
                return Err(CdcError::Backend(anyhow::anyhow!(
                    "oauth_token called for a non-OAuth strategy"
                )))
            }
        };

        let response = self
            .client
            .post(token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("requesting an OAuth token: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            // Never include the body: an OAuth error response can echo the
            // assertion back.
            return Err(CdcError::Backend(anyhow::anyhow!(
                "the OAuth token endpoint {token_url} answered {status}"
            )));
        }

        let body: Json = response
            .json()
            .await
            .map_err(|e| CdcError::Backend(anyhow::anyhow!("parsing the OAuth token: {e}")))?;
        let access = body
            .get("access_token")
            .and_then(Json::as_str)
            .ok_or_else(|| {
                CdcError::Backend(anyhow::anyhow!(
                    "the OAuth token endpoint returned no access_token"
                ))
            })?;
        let ttl = body
            .get("expires_in")
            .and_then(Json::as_u64)
            .unwrap_or(3_600);

        let bearer = Secret(access.to_string());
        *self.token.lock().expect("token cache poisoned") = Some(CachedToken {
            bearer: bearer.clone(),
            expires_at: Instant::now() + Duration::from_secs(ttl),
        });
        Ok(bearer)
    }
}

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
}

#[derive(Debug, Default)]
struct Page {
    rows: Vec<RowEvent>,
    next_link: Option<String>,
    delta_link: Option<String>,
}

/// OData v2: `{"d": {"results": [...], "__next": "...", "__delta": "..."}}`.
fn parse_v2(body: &Json) -> Result<Page, CdcError> {
    let d = body.get("d").ok_or_else(|| {
        CdcError::Backend(anyhow::anyhow!(
            "an OData v2 response has no 'd' wrapper; is the service really v2, and did the \
             request reach SAP rather than a proxy error page?"
        ))
    })?;

    let mut page = Page {
        next_link: d.get("__next").and_then(Json::as_str).map(str::to_string),
        delta_link: d.get("__delta").and_then(Json::as_str).map(str::to_string),
        ..Page::default()
    };

    let results = d
        .get("results")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();

    for entry in results {
        let object = entry.as_object().ok_or_else(|| {
            CdcError::Backend(anyhow::anyhow!("an OData v2 result entry is not an object"))
        })?;
        let mut row = serde_json::Map::new();
        let mut deleted = false;
        let mut id_url = None;

        for (key, value) in object {
            if key == "__metadata" {
                if let Some(meta) = value.as_object() {
                    id_url = meta.get("id").and_then(Json::as_str).map(str::to_string);
                    deleted |= meta
                        .get("deleted_entity")
                        .and_then(Json::as_bool)
                        .unwrap_or(false);
                }
                continue;
            }
            if key == "@sap.deleted_entity" {
                deleted |= value.as_bool().unwrap_or(false);
                continue;
            }
            // `__deferred` navigation stubs are links, not data.
            if key.starts_with('@') || value.get("__deferred").is_some() {
                continue;
            }
            row.insert(key.clone(), normalize_v2_value(value));
        }

        page.rows.push(RowEvent {
            deleted,
            row,
            id_url,
        });
    }

    Ok(page)
}

/// v2 renders dates as `/Date(1700000000000)/`. Normalising here rather than
/// downstream is what makes an entity's `changed_at` parse identically under v2
/// and v4.
fn normalize_v2_value(value: &Json) -> Json {
    let Some(s) = value.as_str() else {
        return value.clone();
    };
    let epoch_ms = s
        .strip_prefix("/Date(")
        .and_then(|rest| rest.strip_suffix(")/"))
        // v2 sometimes appends a timezone offset: `/Date(1700000000000+0060)/`.
        .map(|num| num.split(['+', '-']).next().unwrap_or(num))
        .and_then(|num| num.trim().parse::<i64>().ok());
    match epoch_ms.and_then(DateTime::<Utc>::from_timestamp_millis) {
        Some(ts) => Json::String(ts.to_rfc3339()),
        None => value.clone(),
    }
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
                authorized_tokens: authorized_tokens.to_vec(),
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
    /// finished. Paging is server-driven — the service hands back `__next` /
    /// `@odata.nextLink` and this module follows it until there is none.
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

        if self.version == SapODataVersion::V2 {
            // v2 negotiates JSON with a query option, not with Accept.
            url.query_pairs_mut().append_pair("$format", "json");
        }
        Ok(url)
    }

    /// Fetch one page.
    ///
    /// `position` is the delta token the cycle is following, or `None` on the
    /// cold-start read. It is what decides whether a rejection is an unusable
    /// *position* or merely a backend failure: on a cold read there is no
    /// position for the service to be objecting to.
    async fn fetch_page(&self, url: &Url, position: Option<&str>) -> Result<Page, CdcError> {
        let request = self
            .client
            .client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
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

        let body: Json = response.json().await.map_err(|e| {
            CdcError::Backend(anyhow::anyhow!(
                "parsing the OData response from {}: {e}",
                redact(url)
            ))
        })?;

        match self.version {
            SapODataVersion::V2 => parse_v2(&body),
            SapODataVersion::V4 => parse_v4(&body),
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

        let (changed_at, changed_at_source) = self.changed_at(&event.row);

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
    fn changed_at(&self, row: &serde_json::Map<String, Json>) -> (DateTime<Utc>, &'static str) {
        let parsed = self
            .changed_at_property
            .as_deref()
            .and_then(|name| row.get(name))
            .and_then(parse_timestamp);
        match parsed {
            Some(ts) => (ts, "entity"),
            None => (Utc::now(), "observed"),
        }
    }
}

fn parse_timestamp(value: &Json) -> Option<DateTime<Utc>> {
    if let Some(ms) = value.as_i64() {
        return DateTime::<Utc>::from_timestamp_millis(ms);
    }
    let s = value.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
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
        self.next_url = self.resolve(&delta_link)?;
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
        let (next_url, position, initial) = match &from {
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

    #[test]
    fn v2_pages_yield_rows_links_and_tombstones() {
        let body: Json = serde_json::from_str(
            r#"{"d":{
                "results":[
                    {"__metadata":{"id":"A_BusinessPartner('1')"},
                     "BusinessPartner":"1","BusinessPartnerName":"ACME",
                     "LastChangeDateTime":"/Date(1700000000000)/"},
                    {"__metadata":{"id":"A_BusinessPartner('2')","deleted_entity":true}}
                ],
                "__next":"https://s4.example.com/next",
                "__delta":"https://s4.example.com/delta?!deltatoken=D1"
            }}"#,
        )
        .unwrap();
        let page = parse_v2(&body).unwrap();
        assert_eq!(page.rows.len(), 2);
        assert!(!page.rows[0].deleted);
        assert!(page.rows[1].deleted);
        assert_eq!(
            page.next_link.as_deref(),
            Some("https://s4.example.com/next")
        );
        assert!(page.delta_link.is_some());
        // `/Date(...)/` must already be RFC 3339 by the time anything else sees
        // it, so `changed_at` parses the same way under v2 and v4.
        let changed = page.rows[0].row.get("LastChangeDateTime").unwrap();
        assert!(changed.as_str().unwrap().contains('T'), "got {changed}");
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
        let secret = Secret("hunter2".into());
        assert!(!format!("{secret:?}").contains("hunter2"));
    }
}
