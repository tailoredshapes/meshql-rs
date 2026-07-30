//! The physical layer: table shape, key encoding, temporal arithmetic, and the
//! two read primitives (`query_latest`, `scan_latest`) both traits share.
//!
//! # Table shape — one table per collection
//!
//! | attribute | type | meaning |
//! |---|---|---|
//! | `pk`      | S (hash)  | the Envelope `id` |
//! | `sk`      | S (range) | `{created_at_nanos:019}#{uuid}` |
//! | `ca_ns`   | N         | `created_at` nanos since epoch |
//! | `ca`      | S         | `created_at.to_rfc3339()` — authoritative |
//! | `del`     | BOOL      | `deleted` |
//! | `toks`    | L of S    | `authorized_tokens` |
//! | `payload` | M         | the payload Stash |
//!
//! The sort key is zero-padded to 19 digits so lexicographic order **is**
//! nanosecond order, which is what makes "latest version at-or-before a cutoff"
//! a single `query` with `scan_index_forward(false).limit(1)`. The uuid suffix
//! makes two same-nanosecond writes to the same id two distinct items instead of
//! one silently overwriting the other — meshql is append-only, so losing a
//! version is a correctness bug, not a race to shrug at.
//!
//! `#` is 0x23, below every decimal digit, so `{cutoff+1:019}#` is a correct
//! *strict* upper bound on "nanos <= cutoff". See
//! `tests::hash_separator_makes_the_upper_bound_exact` — that claim is right by
//! accident unless it is pinned.

use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, KeySchemaElement, KeyType,
    ScalarAttributeType, TableStatus,
};
use aws_sdk_dynamodb::Client;
use chrono::{DateTime, Utc};
use meshql_core::{Envelope, MeshqlError, Result};
use std::collections::HashMap;
use std::time::Duration;

use crate::convert;

pub const PK: &str = "pk";
pub const SK: &str = "sk";
pub const CA_NS: &str = "ca_ns";
pub const CA: &str = "ca";
pub const DEL: &str = "del";
pub const TOKS: &str = "toks";
pub const PAYLOAD: &str = "payload";

/// Width the nanosecond component of the sort key is zero-padded to. `i64::MAX`
/// nanos since the epoch is 9223372036854775807 — 19 digits — so every
/// representable instant pads to exactly this width and lexicographic order over
/// the padded strings is numeric order.
const NANOS_WIDTH: usize = 19;

/// Separator between the nanosecond component and the uniquifier. 0x23, which
/// sorts below `0`..`9`, so a bound of `{n:019}#` excludes every sort key whose
/// nanos component is `n`.
const SEP: char = '#';

// ---- key encoding ----

/// `created_at` as nanoseconds since the epoch, saturating at the i64 bounds
/// rather than panicking on an out-of-range date.
pub fn created_at_nanos(created_at: DateTime<Utc>) -> i64 {
    created_at.timestamp_nanos_opt().unwrap_or_else(|| {
        if created_at.timestamp() < 0 {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

/// Sort key for a new version at `nanos`. Freshly random per call — two writes
/// in the same nanosecond must land on different items.
pub fn sort_key(nanos: i64) -> String {
    format!(
        "{:0width$}{SEP}{}",
        nanos,
        uuid::Uuid::new_v4().simple(),
        width = NANOS_WIDTH
    )
}

/// Strict upper bound on the sort keys of versions with `nanos <= cutoff_nanos`.
/// Used as `sk < upper_bound(cutoff)`.
pub fn upper_bound(cutoff_nanos: i64) -> String {
    format!(
        "{:0width$}{SEP}",
        cutoff_nanos.saturating_add(1),
        width = NANOS_WIDTH
    )
}

/// The contract compares at *millisecond* precision: `read(.., at: Some(t))`
/// resolves the latest version with `created_at_ms <= t_ms`. So a millisecond
/// cutoff becomes the **last nanosecond of that millisecond** — anything less
/// would drop a version written later within the cutoff's own millisecond, and
/// `test_temporal_versioning` /
/// `test_searcher_ordering_as_of_uses_version_resolved_at_cutoff` both address a
/// cutoff that is exactly some version's timestamp.
pub fn cutoff_nanos_from_millis(at_ms: i64) -> i64 {
    at_ms
        .saturating_add(1)
        .saturating_mul(1_000_000)
        .saturating_sub(1)
}

/// The "now" cutoff, for the read paths that take no `at`.
pub fn now_cutoff_nanos() -> i64 {
    cutoff_nanos_from_millis(Utc::now().timestamp_millis())
}

// ---- item <-> envelope ----

pub fn envelope_to_item(env: &Envelope) -> HashMap<String, AttributeValue> {
    let nanos = created_at_nanos(env.created_at);
    let mut item = HashMap::new();
    item.insert(PK.to_string(), AttributeValue::S(env.id.clone()));
    item.insert(SK.to_string(), AttributeValue::S(sort_key(nanos)));
    item.insert(CA_NS.to_string(), AttributeValue::N(nanos.to_string()));
    item.insert(
        CA.to_string(),
        AttributeValue::S(env.created_at.to_rfc3339()),
    );
    item.insert(DEL.to_string(), AttributeValue::Bool(env.deleted));
    item.insert(
        TOKS.to_string(),
        AttributeValue::L(
            env.authorized_tokens
                .iter()
                .map(|t| AttributeValue::S(t.clone()))
                .collect(),
        ),
    );
    item.insert(
        PAYLOAD.to_string(),
        AttributeValue::M(convert::object_to_map(&env.payload)),
    );
    item
}

fn missing(attr: &str) -> MeshqlError {
    MeshqlError::Storage(format!("item is missing the {attr:?} attribute"))
}

pub fn item_to_envelope(item: &HashMap<String, AttributeValue>) -> Result<Envelope> {
    let id = match item.get(PK) {
        Some(AttributeValue::S(s)) => s.clone(),
        _ => return Err(missing(PK)),
    };

    // `ca` is authoritative for reconstruction: it carries the full RFC3339
    // rendering, so `created_at` comes back byte-identical to what a client was
    // handed on write. `ca_ns` exists for range arithmetic, not reconstruction.
    let created_at = match item.get(CA) {
        Some(AttributeValue::S(s)) => DateTime::parse_from_rfc3339(s)
            .map_err(|e| MeshqlError::Parse(format!("{CA} is not RFC3339 ({s:?}): {e}")))?
            .with_timezone(&Utc),
        _ => return Err(missing(CA)),
    };

    let deleted = match item.get(DEL) {
        Some(AttributeValue::Bool(b)) => *b,
        None => false,
        Some(other) => {
            return Err(MeshqlError::Storage(format!(
                "{DEL} must be BOOL, got {other:?}"
            )))
        }
    };

    let authorized_tokens = match item.get(TOKS) {
        Some(AttributeValue::L(items)) => items
            .iter()
            .map(|a| match a {
                AttributeValue::S(s) => Ok(s.clone()),
                other => Err(MeshqlError::Storage(format!(
                    "{TOKS} must be a list of S, got {other:?}"
                ))),
            })
            .collect::<Result<Vec<String>>>()?,
        None => Vec::new(),
        Some(other) => {
            return Err(MeshqlError::Storage(format!(
                "{TOKS} must be L, got {other:?}"
            )))
        }
    };

    let payload = match item.get(PAYLOAD) {
        Some(AttributeValue::M(m)) => convert::map_to_object(m)?,
        None => meshql_core::Stash::new(),
        Some(other) => {
            return Err(MeshqlError::Storage(format!(
                "{PAYLOAD} must be M, got {other:?}"
            )))
        }
    };

    Ok(Envelope {
        id,
        payload,
        created_at,
        deleted,
        authorized_tokens,
    })
}

fn item_sort_key(item: &HashMap<String, AttributeValue>) -> Result<String> {
    match item.get(SK) {
        Some(AttributeValue::S(s)) => Ok(s.clone()),
        _ => Err(missing(SK)),
    }
}

// ---- client construction ----

/// Build a DynamoDB client.
///
/// `endpoint: None` means "real AWS from the ambient config" — region,
/// credentials and everything else come from the standard provider chain.
///
/// `endpoint: Some(url)` is the DynamoDB Local escape hatch, and it also
/// substitutes a dummy region and dummy static credentials so a developer with
/// no AWS profile can run the tests. If you need a custom endpoint *with* real
/// credentials (a VPC endpoint, say), build the client yourself and use
/// [`crate::DynamoRepository::new_with_client`].
pub async fn make_client(endpoint: Option<&str>) -> Client {
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest());

    if let Some(url) = endpoint {
        loader = loader
            .endpoint_url(url)
            .region(aws_sdk_dynamodb::config::Region::new(
                std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
            ))
            .credentials_provider(aws_sdk_dynamodb::config::Credentials::new(
                "local",
                "local",
                None,
                None,
                "meshql-dynamo-local",
            ));
    }

    Client::new(&loader.load().await)
}

// ---- table lifecycle ----

/// Create the table with `PAY_PER_REQUEST` billing if it is absent, then wait
/// until it reports `ACTIVE`.
///
/// Idempotent and safe to call concurrently: a losing racer gets
/// `ResourceInUseException` from `CreateTable` and falls through to the same
/// wait as the winner.
pub async fn ensure_table(client: &Client, table: &str) -> Result<()> {
    if table_status(client, table).await? == Some(TableStatus::Active) {
        return Ok(());
    }

    let key = |name: &str, kind: KeyType| {
        KeySchemaElement::builder()
            .attribute_name(name)
            .key_type(kind)
            .build()
            .map_err(|e| MeshqlError::Storage(e.to_string()))
    };
    let attr = |name: &str| {
        AttributeDefinition::builder()
            .attribute_name(name)
            .attribute_type(ScalarAttributeType::S)
            .build()
            .map_err(|e| MeshqlError::Storage(e.to_string()))
    };

    let created = client
        .create_table()
        .table_name(table)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(key(PK, KeyType::Hash)?)
        .key_schema(key(SK, KeyType::Range)?)
        .attribute_definitions(attr(PK)?)
        .attribute_definitions(attr(SK)?)
        .send()
        .await;

    if let Err(e) = created {
        let already_there = e
            .as_service_error()
            .map(|se| se.is_resource_in_use_exception())
            .unwrap_or(false);
        if !already_there {
            return Err(MeshqlError::Storage(format!(
                "create_table {table}: {}",
                describe_sdk_error(&e)
            )));
        }
    }

    // Poll describe_table. DynamoDB Local goes ACTIVE immediately; real
    // DynamoDB takes seconds.
    for _ in 0..300 {
        if table_status(client, table).await? == Some(TableStatus::Active) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Err(MeshqlError::Storage(format!(
        "table {table} did not become ACTIVE within 30s"
    )))
}

async fn table_status(client: &Client, table: &str) -> Result<Option<TableStatus>> {
    match client.describe_table().table_name(table).send().await {
        Ok(out) => Ok(out.table().and_then(|t| t.table_status()).cloned()),
        Err(e) => {
            if e.as_service_error()
                .map(|se| se.is_resource_not_found_exception())
                .unwrap_or(false)
            {
                Ok(None)
            } else {
                Err(MeshqlError::Storage(format!(
                    "describe_table {table}: {}",
                    describe_sdk_error(&e)
                )))
            }
        }
    }
}

/// Drop the table. Not part of the `Repository` contract — exposed for test
/// fixtures, which give every test its own table.
pub async fn drop_table(client: &Client, table: &str) -> Result<()> {
    match client.delete_table().table_name(table).send().await {
        Ok(_) => Ok(()),
        Err(e) => {
            if e.as_service_error()
                .map(|se| se.is_resource_not_found_exception())
                .unwrap_or(false)
            {
                Ok(())
            } else {
                Err(MeshqlError::Storage(format!(
                    "delete_table {table}: {}",
                    describe_sdk_error(&e)
                )))
            }
        }
    }
}

/// `SdkError`'s `Display` is famously terse ("service error"); the useful text
/// is on the source chain.
pub fn describe_sdk_error<E: std::fmt::Debug, R: std::fmt::Debug>(e: &SdkError<E, R>) -> String {
    format!("{e:?}")
}

// ---- read primitives ----

/// Resolve one id to its latest version at-or-before `cutoff_nanos`, in a single
/// round trip.
///
/// `pk` is the hash key and the sort key is nanosecond-ordered, so
/// `pk = :pk AND sk < :hi` walked backwards with `limit(1)` *is* the answer.
/// Deletion and visibility are deliberately **not** applied here — the caller
/// applies them to the resolved version. Applying them first would resurface an
/// older visible version of a now-restricted record.
pub async fn query_latest(
    client: &Client,
    table: &str,
    id: &str,
    cutoff_nanos: i64,
) -> Result<Option<Envelope>> {
    let out = client
        .query()
        .table_name(table)
        .key_condition_expression("#pk = :pk AND #sk < :hi")
        .expression_attribute_names("#pk", PK)
        .expression_attribute_names("#sk", SK)
        .expression_attribute_values(":pk", AttributeValue::S(id.to_string()))
        .expression_attribute_values(":hi", AttributeValue::S(upper_bound(cutoff_nanos)))
        .scan_index_forward(false)
        .limit(1)
        .send()
        .await
        .map_err(|e| {
            MeshqlError::Storage(format!("query {table} pk={id}: {}", describe_sdk_error(&e)))
        })?;

    match out.items().first() {
        None => Ok(None),
        Some(item) => Ok(Some(item_to_envelope(item)?)),
    }
}

/// Resolve *every* id to its latest version at-or-before `cutoff_nanos`, drop
/// the ones whose resolved version is a tombstone, and return the survivors in
/// canonical order (`meshql_core::envelope_order`).
///
/// A full table `scan`, paginated, with `sk < :hi` as the only filter. Nothing
/// else is pushed down: the predicate must be applied to the *resolved* version,
/// so filtering versions on the way out of the scan would resolve the wrong one
/// whenever an older version matches and the current one does not.
///
/// Visibility is not applied here either, for the same reason as
/// [`query_latest`] — and because a `limit` must never be able to consume an
/// invisible row.
pub async fn scan_latest(client: &Client, table: &str, cutoff_nanos: i64) -> Result<Vec<Envelope>> {
    let hi = upper_bound(cutoff_nanos);
    let mut latest: HashMap<String, (String, Envelope)> = HashMap::new();
    let mut start_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut req = client
            .scan()
            .table_name(table)
            .filter_expression("#sk < :hi")
            .expression_attribute_names("#sk", SK)
            .expression_attribute_values(":hi", AttributeValue::S(hi.clone()));
        if let Some(key) = start_key.take() {
            req = req.set_exclusive_start_key(Some(key));
        }

        let out = req.send().await.map_err(|e| {
            MeshqlError::Storage(format!("scan {table}: {}", describe_sdk_error(&e)))
        })?;

        for item in out.items() {
            let sk = item_sort_key(item)?;
            let env = item_to_envelope(item)?;
            match latest.get(&env.id) {
                Some((seen, _)) if *seen >= sk => {}
                _ => {
                    latest.insert(env.id.clone(), (sk, env));
                }
            }
        }

        match out.last_evaluated_key() {
            Some(key) if !key.is_empty() => start_key = Some(key.clone()),
            _ => break,
        }
    }

    let mut resolved: Vec<Envelope> = latest
        .into_values()
        .map(|(_, env)| env)
        .filter(|env| !env.deleted)
        .collect();

    // `latest` is a HashMap, so restore the canonical order before anything can
    // apply a limit to it.
    resolved.sort_by(meshql_core::envelope_order);
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim the whole sort-key design rests on: for the string bound
    /// `{cutoff+1:019}#`, `sk < bound` is *exactly* `nanos <= cutoff`. It is the
    /// kind of thing that is right by accident, so pin it.
    #[test]
    fn hash_separator_makes_the_upper_bound_exact() {
        assert!(SEP < '0', "the separator must sort below every digit");
        assert!(SEP < '9');

        for cutoff in [0i64, 1, 999_999, 1_704_067_200_000_000_000, i64::MAX - 4] {
            let bound = upper_bound(cutoff);
            for delta in [-2i64, -1, 0] {
                let n = cutoff.saturating_add(delta);
                let sk = sort_key(n);
                assert!(
                    sk < bound,
                    "nanos {n} <= cutoff {cutoff} must be inside the bound ({sk} vs {bound})"
                );
            }
            for delta in [1i64, 2] {
                let n = cutoff.saturating_add(delta);
                if n <= cutoff {
                    continue; // saturated at i64::MAX
                }
                let sk = sort_key(n);
                assert!(
                    sk >= bound,
                    "nanos {n} > cutoff {cutoff} must be outside the bound ({sk} vs {bound})"
                );
            }
        }
    }

    /// The separator matters because the nanos component of `sk` is followed by
    /// more characters. A separator that sorted *above* a digit would let a
    /// version written at exactly `cutoff+1` slip inside the bound.
    #[test]
    fn a_digit_separator_would_break_the_bound() {
        let cutoff = 1_704_067_200_000_000_000i64;
        let equal = format!("{:0width$}", cutoff + 1, width = NANOS_WIDTH);
        let bound = upper_bound(cutoff);

        // With `#`, the same-nanos key is outside the bound...
        assert!(format!("{equal}#deadbeef") >= bound);
        // ...whereas a separator that sorts above `#` but below the digits of a
        // uuid would not be enough on its own; what saves us is that any suffix
        // is greater than the empty suffix.
        assert_eq!(bound, format!("{equal}#"));
        assert!(bound < format!("{equal}#0"));
    }

    #[test]
    fn zero_padding_makes_lexicographic_order_numeric_order() {
        let mut nanos = [
            1i64,
            10,
            2,
            999_999_999,
            1_000_000_000,
            1_704_067_200_000_000_000,
            i64::MAX,
        ];
        nanos.sort();
        let keys: Vec<String> = nanos.iter().map(|n| sort_key(*n)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(
            keys, sorted,
            "19-digit zero padding must make string order agree with numeric order"
        );
        for k in &keys {
            assert_eq!(
                k.split(SEP).next().unwrap().len(),
                NANOS_WIDTH,
                "every nanos component pads to the same width: {k}"
            );
        }
    }

    /// The contract compares at millisecond precision, so the cutoff must be the
    /// *last* nanosecond of the cutoff millisecond. `at_ms * 1_000_000` — the
    /// obvious wrong answer — drops a version written 400µs into that same
    /// millisecond.
    #[test]
    fn cutoff_covers_the_whole_cutoff_millisecond() {
        let at_ms = 1_704_067_205_000i64;
        let cutoff = cutoff_nanos_from_millis(at_ms);
        // The last nanosecond of millisecond 1_704_067_205_000.
        assert_eq!(cutoff, 1_704_067_205_000_999_999);
        assert_eq!(cutoff, at_ms * 1_000_000 + 999_999);

        let bound = upper_bound(cutoff);
        // Anything within the cutoff millisecond is included...
        for offset in [0i64, 1, 400_000, 999_999] {
            let sk = sort_key(at_ms * 1_000_000 + offset);
            assert!(
                sk < bound,
                "offset {offset} within the cutoff ms must count"
            );
        }
        // ...and the first nanosecond of the next millisecond is not.
        let sk = sort_key((at_ms + 1) * 1_000_000);
        assert!(sk >= bound, "the next millisecond must be excluded");
    }

    #[test]
    fn envelope_round_trips_through_an_item() {
        let mut payload = meshql_core::Stash::new();
        payload.insert("name".to_string(), serde_json::json!("alpha"));
        payload.insert("count".to_string(), serde_json::json!(3));
        let env = Envelope {
            id: "id-1".to_string(),
            payload,
            // Deliberately off a second boundary and off a millisecond boundary.
            created_at: DateTime::parse_from_rfc3339("2024-06-01T12:34:56.789+00:00")
                .unwrap()
                .with_timezone(&Utc),
            deleted: false,
            authorized_tokens: vec!["alice".to_string()],
        };

        let item = envelope_to_item(&env);
        let back = item_to_envelope(&item).unwrap();

        assert_eq!(back.id, env.id);
        assert_eq!(back.payload, env.payload);
        assert_eq!(back.deleted, env.deleted);
        assert_eq!(back.authorized_tokens, env.authorized_tokens);
        assert_eq!(back.created_at, env.created_at);
        assert_eq!(
            back.created_at.to_rfc3339(),
            env.created_at.to_rfc3339(),
            "the searcher result shape cert compares the rendered string, not the instant"
        );
    }

    #[test]
    fn empty_tokens_round_trip_as_empty_not_missing() {
        let env = Envelope {
            id: "public".to_string(),
            payload: meshql_core::Stash::new(),
            created_at: Utc::now(),
            deleted: false,
            authorized_tokens: vec![],
        };
        let back = item_to_envelope(&envelope_to_item(&env)).unwrap();
        assert!(back.authorized_tokens.is_empty());
    }

    #[test]
    fn two_versions_in_the_same_nanosecond_get_distinct_sort_keys() {
        let nanos = 1_704_067_200_000_000_000i64;
        let a = sort_key(nanos);
        let b = sort_key(nanos);
        assert_ne!(a, b, "the uuid suffix is what stops a version being lost");
        assert_eq!(a.split(SEP).next(), b.split(SEP).next());
    }
}
