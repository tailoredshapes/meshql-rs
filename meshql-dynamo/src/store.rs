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
//!
//! # Indexed fields
//!
//! An [`IndexPlan`] adds one promoted attribute per indexed payload field
//! (`ix_{field}`) and one `KEYS_ONLY` global secondary index over it, hash key
//! `ix_{field}` and range key `sk`. The base table shape is unchanged, so a
//! table with indexes and one without hold byte-identical envelopes and
//! [`item_to_envelope`] does not know the difference. See [`crate::index`].

use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, CreateGlobalSecondaryIndexAction,
    GlobalSecondaryIndex, GlobalSecondaryIndexUpdate, IndexStatus, KeySchemaElement, KeyType,
    Projection, ProjectionType, ScalarAttributeType, TableDescription, TableStatus,
};
use aws_sdk_dynamodb::Client;
use chrono::{DateTime, Utc};
use meshql_core::{Envelope, MeshqlError, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use crate::convert;
use crate::index::{self, IndexPlan};
use crate::metering::{return_consumed_capacity, CapacityMeter, Op};

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

/// [`envelope_to_item`] plus the promoted attributes `plan` calls for.
///
/// The promotion is *additive*: the envelope attributes are untouched, so a
/// table that gains an index still reads back exactly the envelopes it held.
pub fn envelope_to_indexed_item(
    env: &Envelope,
    plan: &IndexPlan,
) -> HashMap<String, AttributeValue> {
    let mut item = envelope_to_item(env);
    plan.promote(&env.payload, &mut item);
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
///
/// Equivalent to [`ensure_indexed_table`] with an empty plan, which means it
/// also **refuses a table that carries indexes this handle does not maintain**.
/// That is not pedantry: a repository with no plan writes no promoted
/// attributes, so pairing one with an indexed searcher over the same table
/// yields searches that silently return nothing. Failing to open is the only
/// outcome that cannot be mistaken for working.
pub async fn ensure_table(client: &Client, table: &str) -> Result<()> {
    ensure_indexed_table(client, table, &IndexPlan::default()).await
}

/// Create or verify the table *and* the global secondary indexes `plan` calls
/// for, then wait until every one of them reports `ACTIVE`.
///
/// - **Absent table** → `CreateTable` with the indexes in place.
/// - **Present, indexes match** → nothing to do; this is every restart after
///   the first.
/// - **Present, an index is missing and the table is empty** → `UpdateTable`
///   adds it. An empty table has nothing to be wrong about.
/// - **Present, an index is missing and the table holds data** → **error**.
///   Promotion happens on write, so items written before the field was indexed
///   carry no `ix_{field}` and are invisible to the new index. Creating it
///   anyway would produce a search that silently omits every historical record.
///   [`migrate_indexes`] is the fix, and the error says so.
/// - **Present, and it carries a `meshql_ix_*` index the plan does not have** →
///   **error**. Either this handle is not promoting a field something else
///   indexes, or the configuration shrank and left an index behind that is now
///   costing a write unit per write for nothing.
///
/// Indexes this crate does not name (no `meshql_ix_` prefix) are ignored
/// entirely — a client's own index is their business.
pub async fn ensure_indexed_table(client: &Client, table: &str, plan: &IndexPlan) -> Result<()> {
    let described = match wait_for_active(client, table).await? {
        Some(description) => description,
        None => {
            create_table(client, table, plan).await?;
            wait_for_active(client, table).await?.ok_or_else(|| {
                MeshqlError::Storage(format!("table {table} vanished immediately after creation"))
            })?
        }
    };

    let present = managed_indexes(&described);
    let wanted: BTreeSet<String> = plan.fields().map(String::from).collect();

    let unexpected: Vec<&String> = present.difference(&wanted).collect();
    if !unexpected.is_empty() {
        return Err(MeshqlError::Storage(format!(
            "table {table} carries global secondary index(es) on {:?} that this handle does \
             not maintain (its plan indexes: {}). A repository that does not promote a field \
             another handle indexes writes versions the index cannot see, so the searches \
             that use it return silently incomplete results. Derive both from the same \
             RootConfig — see DynamoCollection — or drop the stale index.",
            unexpected,
            plan.describe(),
        )));
    }

    let missing: Vec<&String> = wanted.difference(&present).collect();
    if missing.is_empty() {
        return Ok(());
    }

    if table_has_items(client, table).await? {
        return Err(MeshqlError::Storage(format!(
            "table {table} needs new index(es) on {missing:?} and already holds data. Items \
             written before a field was indexed carry no ix_ attribute, so the new index \
             cannot see them and every search on that field would silently omit its \
             history. Run meshql_dynamo::migrate_indexes(&client, {table:?}, &plan) once — \
             it rewrites the promoted attributes and then creates the indexes — and start \
             again."
        )));
    }

    // `add_index` waits for each build to finish, which is what makes a
    // multi-field plan legal: DynamoDB allows only one online index build per
    // table at a time.
    for field in missing {
        add_index(client, table, field).await?;
    }
    Ok(())
}

/// Bring an existing, populated table up to `plan`: promote the attributes on
/// every stored item, then create the missing indexes.
///
/// Run once, out of band, when a deployment starts filtering on a field it did
/// not filter on before. It is `O(V)`: one full `Scan` plus one `PutItem` per
/// stored version, which at the on-demand rate is about **$0.63 per million
/// versions** and is paid once, not per query.
///
/// **The order matters.** Attributes are promoted *before* the index exists, so
/// the index is built by DynamoDB's own backfill over an already-complete table
/// and there is never an interval in which the index exists and is missing
/// history. A concurrent process starting mid-migration sees no index and
/// refuses to start (see [`ensure_indexed_table`]) rather than serving from a
/// half-built one.
///
/// Rewrites are `PutItem` over the *same* `(pk, sk)`, so they overwrite a
/// version in place. No new version is created and no history is disturbed.
///
/// Returns the number of items rewritten.
pub async fn migrate_indexes(client: &Client, table: &str, plan: &IndexPlan) -> Result<u64> {
    wait_for_active(client, table).await?.ok_or_else(|| {
        MeshqlError::Storage(format!("cannot migrate {table}: it does not exist"))
    })?;

    let mut rewritten = 0u64;
    let mut start_key: Option<HashMap<String, AttributeValue>> = None;
    loop {
        let mut req = client.scan().table_name(table);
        if let Some(key) = start_key.take() {
            req = req.set_exclusive_start_key(Some(key));
        }
        let out = req.send().await.map_err(|e| {
            MeshqlError::Storage(format!("scan {table}: {}", describe_sdk_error(&e)))
        })?;

        for item in out.items() {
            let env = item_to_envelope(item)?;
            let mut promoted = item.clone();
            plan.promote(&env.payload, &mut promoted);
            if promoted == *item {
                continue; // already promoted; nothing to write
            }
            client
                .put_item()
                .table_name(table)
                .set_item(Some(promoted))
                .send()
                .await
                .map_err(|e| {
                    MeshqlError::Storage(format!(
                        "put_item {table} during migration: {}",
                        describe_sdk_error(&e)
                    ))
                })?;
            rewritten += 1;
        }

        match out.last_evaluated_key() {
            Some(key) if !key.is_empty() => start_key = Some(key.clone()),
            _ => break,
        }
    }

    let described = wait_for_active(client, table).await?.ok_or_else(|| {
        MeshqlError::Storage(format!("cannot migrate {table}: it does not exist"))
    })?;
    let present = managed_indexes(&described);
    for field in plan.fields() {
        if !present.contains(field) {
            add_index(client, table, field).await?;
        }
    }
    Ok(rewritten)
}

fn key_schema(name: &str, kind: KeyType) -> Result<KeySchemaElement> {
    KeySchemaElement::builder()
        .attribute_name(name)
        .key_type(kind)
        .build()
        .map_err(|e| MeshqlError::Storage(e.to_string()))
}

fn string_attribute(name: &str) -> Result<AttributeDefinition> {
    AttributeDefinition::builder()
        .attribute_name(name)
        .attribute_type(ScalarAttributeType::S)
        .build()
        .map_err(|e| MeshqlError::Storage(e.to_string()))
}

/// Hash `ix_{field}`, range `sk`, projection `KEYS_ONLY`.
///
/// `sk` as the range key is what keeps a temporal `at:` cutoff a *key
/// condition* on the index instead of a filter. `KEYS_ONLY` is right because
/// the two-phase search reads every candidate from the base table anyway — a
/// wider projection would be written on every write and read never.
fn index_definition(field: &str) -> Result<GlobalSecondaryIndex> {
    GlobalSecondaryIndex::builder()
        .index_name(index::index_name(field))
        .key_schema(key_schema(&index::attribute_name(field), KeyType::Hash)?)
        .key_schema(key_schema(SK, KeyType::Range)?)
        .projection(
            Projection::builder()
                .projection_type(ProjectionType::KeysOnly)
                .build(),
        )
        .build()
        .map_err(|e| MeshqlError::Storage(e.to_string()))
}

async fn create_table(client: &Client, table: &str, plan: &IndexPlan) -> Result<()> {
    let mut req = client
        .create_table()
        .table_name(table)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(key_schema(PK, KeyType::Hash)?)
        .key_schema(key_schema(SK, KeyType::Range)?)
        .attribute_definitions(string_attribute(PK)?)
        .attribute_definitions(string_attribute(SK)?);

    for field in plan.fields() {
        req = req
            .attribute_definitions(string_attribute(&index::attribute_name(field))?)
            .global_secondary_indexes(index_definition(field)?);
    }

    if let Err(e) = req.send().await {
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
    Ok(())
}

/// How long to keep waiting for the control plane to have room for one more
/// index build before giving up and saying so.
const INDEX_BUILD_CAPACITY_TIMEOUT: Duration = Duration::from_secs(600);

/// Add one index to a live table, **and wait until it is usable**.
///
/// # The wait belongs here, not in the caller
///
/// DynamoDB permits only **one** online index build per table at a time, so a
/// plan adding two fields must let the first finish before asking for the
/// second. That was documented on this function and then not honoured by
/// [`ensure_indexed_table`], which fired both `UpdateTable`s back to back;
/// [`migrate_indexes`] did honour it. An invariant that holds at one call site
/// and not another is not an invariant, so the wait now lives inside the
/// operation it constrains and a third caller cannot forget it.
///
/// It showed up as a *flaky test*, which is the expensive way to find it: it
/// only fails when the first build has not finished by the time the second
/// request lands, so it passes on an idle machine and fails under load.
/// Reproduced at **12 failures in 90 attempts** with six concurrent openers;
/// zero after this change.
///
/// # `LimitExceededException` is backpressure, not failure
///
/// There is a second, *account-wide* limit — at most five tables may be
/// building indexes at once — and unlike the per-table one it cannot be
/// serialised away, because other processes in the same account contribute to
/// it. It is not observable except by asking: the attempt *is* the check. So
/// this waits and asks again, which is the documented handling (the SDK itself
/// marks the response retryable) and is the same shape as `create_table`
/// already tolerating `ResourceInUseException` from a concurrent creator.
///
/// This is deliberately **not** a general retry. Exactly one error code is
/// treated as "ask later"; every other failure propagates on the first
/// attempt, and running out of patience is a distinct error that names the
/// limit rather than a generic timeout. A retry loop that swallowed anything
/// else would hide precisely the provisioning bugs this module exists to
/// surface.
///
/// Transient 5xx — `InternalFailure` and friends — are **not** handled here on
/// purpose. The SDK's own retry policy owns that layer and already retries
/// them; duplicating it would mean two backoffs stacked on one request, and
/// widening this loop to "anything the SDK calls retryable" is how a targeted
/// wait becomes a retry-everything that hides real errors.
async fn add_index(client: &Client, table: &str, field: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + INDEX_BUILD_CAPACITY_TIMEOUT;

    loop {
        let outcome = client
            .update_table()
            .table_name(table)
            .attribute_definitions(string_attribute(&index::attribute_name(field))?)
            .global_secondary_index_updates(
                GlobalSecondaryIndexUpdate::builder()
                    .create(
                        CreateGlobalSecondaryIndexAction::builder()
                            .index_name(index::index_name(field))
                            .key_schema(key_schema(&index::attribute_name(field), KeyType::Hash)?)
                            .key_schema(key_schema(SK, KeyType::Range)?)
                            .projection(
                                Projection::builder()
                                    .projection_type(ProjectionType::KeysOnly)
                                    .build(),
                            )
                            .build()
                            .map_err(|e| MeshqlError::Storage(e.to_string()))?,
                    )
                    .build(),
            )
            .send()
            .await;

        match outcome {
            Ok(_) => break,
            Err(e) => {
                let at_capacity = e
                    .as_service_error()
                    .map(|se| se.is_limit_exceeded_exception())
                    .unwrap_or(false);
                if !at_capacity {
                    return Err(MeshqlError::Storage(format!(
                        "update_table {table}: adding index on {field:?}: {}",
                        describe_sdk_error(&e)
                    )));
                }
                if std::time::Instant::now() >= deadline {
                    return Err(MeshqlError::Storage(format!(
                        "update_table {table}: adding index on {field:?}: DynamoDB reported \
                         no capacity for another index build for {}s. At most one index per \
                         table and five tables per account may build at once; something else \
                         is holding that budget. Detail: {}",
                        INDEX_BUILD_CAPACITY_TIMEOUT.as_secs(),
                        describe_sdk_error(&e)
                    )));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }

    // The index is `CREATING` until DynamoDB has backfilled it. Returning here
    // would let the caller add the next one — which is the per-table limit
    // above — and would let a searcher query an index that is not yet complete.
    wait_for_active(client, table).await?;
    Ok(())
}

/// The payload fields this crate's indexes on `description` cover. Indexes
/// without the [`index::INDEX_PREFIX`] belong to someone else and are ignored.
fn managed_indexes(description: &TableDescription) -> BTreeSet<String> {
    description
        .global_secondary_indexes()
        .iter()
        .filter_map(|gsi| gsi.index_name())
        .filter_map(index::field_of_index)
        .map(String::from)
        .collect()
}

/// Is there at least one item? One `Scan` with `Limit(1)` — half a read unit,
/// regardless of how large the table is.
async fn table_has_items(client: &Client, table: &str) -> Result<bool> {
    let out = client
        .scan()
        .table_name(table)
        .limit(1)
        .send()
        .await
        .map_err(|e| {
            MeshqlError::Storage(format!(
                "scan {table} to check for data: {}",
                describe_sdk_error(&e)
            ))
        })?;
    Ok(!out.items().is_empty())
}

/// Poll until the table *and every one of its indexes* is `ACTIVE`, returning
/// the final description — or `None` if the table does not exist.
///
/// Indexes are polled as well as the table because a table can report `ACTIVE`
/// while a `CREATING` index is still backfilling, and querying an index in that
/// state is a `ValidationException` at best and an incomplete answer at worst.
async fn wait_for_active(client: &Client, table: &str) -> Result<Option<TableDescription>> {
    // Real DynamoDB takes seconds to create a table and minutes to backfill a
    // large index; DynamoDB Local is immediate.
    for _ in 0..1200 {
        let description = match client.describe_table().table_name(table).send().await {
            Ok(out) => out.table().cloned(),
            Err(e) => {
                if e.as_service_error()
                    .map(|se| se.is_resource_not_found_exception())
                    .unwrap_or(false)
                {
                    return Ok(None);
                }
                return Err(MeshqlError::Storage(format!(
                    "describe_table {table}: {}",
                    describe_sdk_error(&e)
                )));
            }
        };
        let Some(description) = description else {
            return Ok(None);
        };
        let table_active = description.table_status() == Some(&TableStatus::Active);
        let indexes_active = description
            .global_secondary_indexes()
            .iter()
            .all(|gsi| gsi.index_status() == Some(&IndexStatus::Active));
        if table_active && indexes_active {
            return Ok(Some(description));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Err(MeshqlError::Storage(format!(
        "table {table} and its indexes did not all become ACTIVE within 10 minutes"
    )))
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
///
/// `meter` is `None` for an unmetered call, which is the default everywhere:
/// without it `ReturnConsumedCapacity` is never set and the request on the wire
/// is unchanged. See [`crate::metering`].
pub async fn query_latest(
    client: &Client,
    table: &str,
    id: &str,
    cutoff_nanos: i64,
    meter: Option<&CapacityMeter>,
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
        .set_return_consumed_capacity(return_consumed_capacity(meter))
        .send()
        .await
        .map_err(|e| {
            MeshqlError::Storage(format!("query {table} pk={id}: {}", describe_sdk_error(&e)))
        })?;

    if let Some(m) = meter {
        m.record(Op::Query, out.consumed_capacity());
    }

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
///
/// **The pages are chained.** Each request needs the previous response's
/// `LastEvaluatedKey`, so the round trips are strictly serial and the wall clock
/// is `⌈V·S / 1 MiB⌉ × RTT`, not `RTT`. That is the single biggest fact about
/// this function's latency and it is measured in `docs/cost-model-dynamodb.md`.
/// A parallel `Scan` (`Segment` / `TotalSegments`) is the mitigation, and it is
/// free: capacity is charged on bytes examined and the segments partition the
/// same bytes, so **RRU is invariant in the segment count** — measured at
/// 122,254 RRU at one segment against 122,269 at sixty-four, a drift of 0.012%.
///
/// It does **not** divide the wall clock by the segment count. Measured from a
/// 2 GB Lambda in-region at V = 1,000,000: 45.4 s at one segment, 17.3 s at
/// four, and **no further improvement at sixteen or sixty-four**. The plateau is
/// ~58 MB/s and it is a *consumer-side* ceiling — raising the Lambda to 10 GB
/// moved it to 83 MB/s. Four segments is the whole win; more is waste.
///
/// See `docs/cost-model-dynamodb.md` §11.
pub async fn scan_latest(
    client: &Client,
    table: &str,
    cutoff_nanos: i64,
    meter: Option<&CapacityMeter>,
) -> Result<Vec<Envelope>> {
    scan_latest_segmented(client, table, cutoff_nanos, meter, 1).await
}

/// [`scan_latest`], with the scan split across `segments` concurrent workers.
///
/// `segments = 1` is the serial scan and is what [`scan_latest`] calls.
///
/// **The capacity is the same and the wall clock is not.** DynamoDB charges on
/// bytes examined and `Segment`/`TotalSegments` partitions the same bytes, so
/// RRU is invariant in the segment count — measured at 122,254.5 RRU on one
/// segment against 122,269.0 on sixty-four at V = 1,000,000, a drift of 0.012%.
///
/// The obvious objection is that a *small* table should behave differently,
/// because each segment's final page rounds up to its own 4 KB boundary and
/// there the rounding is the whole bill. **Measured, it does not, at four
/// segments**: a three-item table meters 2.0 RRU serially and 2.0 RRU at four,
/// because a serial `Scan` is already charged per partition and four segments
/// merely re-partition a rounding that was being paid anyway. Sixteen segments
/// on the same table costs 9.5 RRU, so the penalty is real above the table's
/// own partition count.
///
/// The speedup is **not** the segment count: 45.4 s → 17.3 s (2.63×) at four
/// segments and no further improvement at sixteen or sixty-four, against a
/// consumer-side ceiling of ~58 MB/s. Four segments is the whole win.
///
/// The default is one segment regardless, because on a table small enough to
/// fit in a page four round trips buy nothing, and above that a deployment
/// should be choosing this deliberately. **An export can afford four segments;
/// a request path should not be scanning at all.** See
/// `docs/cost-model-dynamodb.md` §10(b) and §11.
pub async fn scan_latest_segmented(
    client: &Client,
    table: &str,
    cutoff_nanos: i64,
    meter: Option<&CapacityMeter>,
    segments: i32,
) -> Result<Vec<Envelope>> {
    let segments = segments.max(1);
    let hi = upper_bound(cutoff_nanos);

    let walk = |segment: i32| {
        let hi = hi.clone();
        async move {
            let mut found: Vec<(String, Envelope)> = Vec::new();
            let mut start_key: Option<HashMap<String, AttributeValue>> = None;
            loop {
                let mut req = client
                    .scan()
                    .table_name(table)
                    .filter_expression("#sk < :hi")
                    .expression_attribute_names("#sk", SK)
                    .expression_attribute_values(":hi", AttributeValue::S(hi.clone()))
                    .set_return_consumed_capacity(return_consumed_capacity(meter));
                if segments > 1 {
                    req = req.segment(segment).total_segments(segments);
                }
                if let Some(key) = start_key.take() {
                    req = req.set_exclusive_start_key(Some(key));
                }

                let out = req.send().await.map_err(|e| {
                    MeshqlError::Storage(format!("scan {table}: {}", describe_sdk_error(&e)))
                })?;

                if let Some(m) = meter {
                    m.record(Op::Scan, out.consumed_capacity());
                }

                for item in out.items() {
                    found.push((item_sort_key(item)?, item_to_envelope(item)?));
                }

                match out.last_evaluated_key() {
                    Some(key) if !key.is_empty() => start_key = Some(key.clone()),
                    _ => break,
                }
            }
            Ok::<_, MeshqlError>(found)
        }
    };

    // Merged after every segment has finished, not within one: a segment holds
    // the versions whose *key* hashes into its slice, and an id's versions all
    // share a hash key, so they do land together — but relying on that would be
    // depending on an internal of the partitioning. The merge is global.
    let per_segment = futures::future::try_join_all((0..segments).map(walk)).await?;

    let mut latest: HashMap<String, (String, Envelope)> = HashMap::new();
    for (sk, env) in per_segment.into_iter().flatten() {
        match latest.get(&env.id) {
            Some((seen, _)) if *seen >= sk => {}
            _ => {
                latest.insert(env.id.clone(), (sk, env));
            }
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

/// Phase 1 of an indexed search: the ids of every **version** whose promoted
/// `ix_{field}` equals `value` at-or-before the cutoff.
///
/// This is a candidate set and nothing more. A version in the index is not a
/// record whose *resolved* version matches — an id that used to hold this value
/// stays in this partition of the index forever — so every id it returns must
/// be re-resolved and re-matched. See [`crate::searcher`] for why that is a
/// soundness requirement rather than a thoroughness habit.
///
/// The cutoff is a key condition on the index's own range key, not a filter, so
/// versions after `at:` are not read and not charged.
pub async fn query_index_candidates(
    client: &Client,
    table: &str,
    field: &str,
    value: &str,
    cutoff_nanos: i64,
    meter: Option<&CapacityMeter>,
) -> Result<HashSet<String>> {
    let hi = upper_bound(cutoff_nanos);
    let mut ids = HashSet::new();
    let mut start_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut req = client
            .query()
            .table_name(table)
            .index_name(index::index_name(field))
            .key_condition_expression("#ix = :v AND #sk < :hi")
            .expression_attribute_names("#ix", index::attribute_name(field))
            .expression_attribute_names("#sk", SK)
            .expression_attribute_values(":v", AttributeValue::S(value.to_string()))
            .expression_attribute_values(":hi", AttributeValue::S(hi.clone()))
            .set_return_consumed_capacity(return_consumed_capacity(meter));
        if let Some(key) = start_key.take() {
            req = req.set_exclusive_start_key(Some(key));
        }

        let out = req.send().await.map_err(|e| {
            MeshqlError::Storage(format!(
                "query {table} index on {field:?}: {}",
                describe_sdk_error(&e)
            ))
        })?;

        if let Some(m) = meter {
            m.record(Op::Query, out.consumed_capacity());
        }

        for item in out.items() {
            if let Some(AttributeValue::S(id)) = item.get(PK) {
                ids.insert(id.clone());
            }
        }

        match out.last_evaluated_key() {
            Some(key) if !key.is_empty() => start_key = Some(key.clone()),
            _ => break,
        }
    }

    Ok(ids)
}

/// How many phase-2 resolutions are in flight at once.
///
/// Not unbounded. `read_many` at k = 100 has a p50 of 22 ms and a p99 of
/// **281 ms** at V = 1M — a hundred concurrent `Query` calls saturate the
/// connection pool and the fan-out is bounded by the slowest of them, not by
/// one round trip (`docs/cost-model-dynamodb.md` §11). A candidate set is not
/// bounded by anything the caller chose, so it needs a ceiling that the caller
/// did not have to think about.
const RESOLVE_CONCURRENCY: usize = 32;

/// Phase 2 of an indexed search: resolve each candidate id to its latest
/// version at-or-before the cutoff, and drop the tombstones.
///
/// Exactly [`query_latest`] per distinct id — half a read unit each — run with
/// bounded concurrency. The result is unordered; the caller sorts, because the
/// caller is also the one applying the predicate and the token filter, and the
/// canonical order has to be established after both.
pub async fn resolve_candidates(
    client: &Client,
    table: &str,
    ids: impl IntoIterator<Item = String>,
    cutoff_nanos: i64,
    meter: Option<&CapacityMeter>,
) -> Result<Vec<Envelope>> {
    use futures::stream::{self, StreamExt, TryStreamExt};

    let reads = ids.into_iter().map(|id| async move {
        let resolved = query_latest(client, table, &id, cutoff_nanos, meter).await?;
        Ok::<Option<Envelope>, MeshqlError>(resolved.filter(|env| !env.deleted))
    });

    let resolved: Vec<Option<Envelope>> = stream::iter(reads)
        .buffer_unordered(RESOLVE_CONCURRENCY)
        .try_collect()
        .await?;
    Ok(resolved.into_iter().flatten().collect())
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
