//! The HubSpot source against a faked portal.
//!
//! Every test here runs offline: `wiremock` stands in for `api.hubapi.com`, so
//! `cargo test` needs no token, no portal and no network. That is deliberate —
//! a connector whose correctness argument can only be checked against a live
//! SaaS is a connector whose correctness argument is never checked.
#![cfg(feature = "hubspot")]

use futures::StreamExt;
use merkql_connect::hubspot::{HubspotSource, Settings};
use merkql_connect::record::{ChangeRecord, Op, Snapshot};
use merkql_connect::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use serde_json::{json, Value};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A HubSpot search result. `property` is the type's last-modified property,
/// which is *not* the same name for contacts as for everything else.
fn object(id: &str, ts_ms: i64, property: &str) -> Value {
    json!({
        "id": id,
        "properties": {
            property: ts_ms.to_string(),
            "name": format!("object {id}"),
        },
        "archived": false,
    })
}

fn page(results: Vec<Value>, after: Option<&str>) -> Value {
    let mut body = json!({ "total": results.len(), "results": results });
    if let Some(after) = after {
        body["paging"] = json!({ "next": { "after": after } });
    }
    body
}

fn settings(base_url: &str, objects: &[&str]) -> Settings {
    Settings {
        base_url: base_url.to_string(),
        objects: objects.iter().map(|o| o.to_string()).collect(),
        entity: "crm".to_string(),
        authorized_tokens: vec!["farm".to_string()],
        properties: vec!["name".to_string()],
        page_size: 100,
        // Long enough that an idle round parks instead of hammering the fake
        // portal, short enough that nothing here waits on it.
        poll_interval: Duration::from_millis(200),
        // Zero, so these tests exercise the same-millisecond boundary itself
        // rather than the lag window built on top of it. The lag window is the
        // same mechanism with a wider skirt, and has its own unit tests.
        index_lag: Duration::ZERO,
        search_window: 10_000,
    }
}

fn source(base_url: &str, objects: &[&str]) -> HubspotSource {
    HubspotSource::with_token(settings(base_url, objects), "test-token".to_string()).unwrap()
}

async fn next(stream: &mut ChangeStream) -> ChangeRecord {
    tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("a record was expected within 10s")
        .expect("the stream must not end")
        .expect("the record must not be an error")
}

async fn next_error(stream: &mut ChangeStream) -> CdcError {
    tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("an error was expected within 10s")
        .expect("the stream must not end")
        .expect_err("an error was expected")
}

/// Assert nothing more arrives. A poller with nothing to say simply does not
/// yield, so "no more records" is a timeout rather than an end of stream — and
/// an end of stream would itself be a failure, because `run_connector` reads
/// one as "the source is finished".
async fn nothing_more(stream: &mut ChangeStream) {
    if tokio::time::timeout(Duration::from_millis(700), stream.next())
        .await
        .is_ok()
    {
        panic!("the source delivered something it should not have");
    }
}

fn cursor_of(record: &ChangeRecord) -> String {
    record
        .position()
        .expect("this record must carry a resumable position")
        .to_string()
}

// ── snapshot-then-stream ─────────────────────────────────────────────────

/// A cold start must emit what already exists as `op: r`, mark the final one
/// [`Snapshot::Last`] so a restart resumes the stream rather than re-reading
/// the portal, and only then switch to `op: c`.
#[tokio::test]
async fn a_cold_start_snapshots_the_portal_then_streams() {
    let server = MockServer::start().await;

    // The backlog.
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                object("1", 1_000, "hs_lastmodifieddate"),
                object("2", 2_000, "hs_lastmodifieddate"),
            ],
            None,
        )))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    // The first live poll. Deal 2 comes back — the cursor filters `>= 2000`,
    // which is the boundary record's own timestamp — and must be dropped by the
    // cursor's id set rather than re-delivered.
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                object("2", 2_000, "hs_lastmodifieddate"),
                object("3", 3_000, "hs_lastmodifieddate"),
            ],
            None,
        )))
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&server)
        .await;

    // Every poll after that. The watermark has moved to deal 3, so a portal
    // honouring `>= 3000` returns only deal 3, which is already in the cursor's
    // id set.
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![object("3", 3_000, "hs_lastmodifieddate")], None)),
        )
        .mount(&server)
        .await;

    let source = source(&server.uri(), &["deals"]);
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();

    let first = next(&mut stream).await;
    assert_eq!(first.op, Op::Read);
    assert_eq!(first.source.snapshot, Snapshot::True);
    assert_eq!(first.key().unwrap(), "deals:1");
    assert!(
        first.position().is_none(),
        "a non-final snapshot record names a point INSIDE the snapshot; resuming a \
         live stream there would start it somewhere the stream never reached"
    );

    let second = next(&mut stream).await;
    assert_eq!(second.op, Op::Read);
    assert_eq!(
        second.source.snapshot,
        Snapshot::Last,
        "without a `last` the connector re-snapshots the whole portal on every restart"
    );
    assert_eq!(second.key().unwrap(), "deals:2");
    assert!(second.position().is_some());

    let third = next(&mut stream).await;
    assert_eq!(third.op, Op::Create, "the snapshot is over");
    assert_eq!(third.source.snapshot, Snapshot::False);
    assert_eq!(
        third.key().unwrap(),
        "deals:3",
        "deal 2 was already emitted and must not come round again"
    );

    nothing_more(&mut stream).await;
}

/// The synthesised envelope is the only thing that survives a repository
/// append, so it has to carry the provenance itself.
#[tokio::test]
async fn the_delivered_envelope_carries_the_hubspot_provenance() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![object("512", 1_751_892_345_123, "hs_lastmodifieddate")],
            None,
        )))
        .mount(&server)
        .await;

    let source = source(&server.uri(), &["deals"]);
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();

    let record = next(&mut stream).await;
    let envelope = record.after.as_ref().unwrap();
    assert_eq!(envelope.id, "deals:512");
    assert_eq!(envelope.authorized_tokens, vec!["farm".to_string()]);
    assert_eq!(envelope.payload["name"], json!("object 512"));
    assert_eq!(envelope.payload["_source"]["object_type"], json!("deals"));
    assert_eq!(envelope.payload["_source"]["object_id"], json!("512"));
    assert_eq!(
        envelope.payload["_source"]["lastmodified_ms"],
        json!(1_751_892_345_123i64)
    );
    assert_eq!(
        envelope.created_at.timestamp_millis(),
        1_751_892_345_123,
        "created_at is HubSpot's modification time; wall-clock would let a re-snapshot \
         mint a newer version than a live record already on the topic"
    );
    assert_eq!(record.source.entity, "crm");
    assert_eq!(record.source.connector, "hubspot");
}

// ── the watermark boundary, end to end ───────────────────────────────────

/// **The headline case.** A record that shares its millisecond with the
/// committed watermark must still be delivered after a restart.
///
/// Incremental sync on `hs_lastmodifieddate` has exactly one way to lose data
/// silently: resume with `> watermark` and every record that shared the
/// boundary millisecond with the last one committed is gone, with nothing
/// downstream able to notice. Resuming with `>=` instead re-delivers the whole
/// boundary millisecond forever. The cursor is therefore `>=` *plus* the set of
/// ids already emitted there, and this test is both halves: `d` arrives, `a`,
/// `b` and `c` do not come round again.
#[tokio::test]
async fn a_record_sharing_the_boundary_millisecond_survives_a_restart() {
    let server = MockServer::start().await;
    let tied = 1_751_892_345_123i64;

    // The snapshot sees three records, all stamped the same millisecond.
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                object("a", tied, "hs_lastmodifieddate"),
                object("b", tied, "hs_lastmodifieddate"),
                object("c", tied, "hs_lastmodifieddate"),
            ],
            None,
        )))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    // A fourth, stamped in the very same millisecond, becomes searchable
    // afterwards. This is ordinary: a bulk import, a workflow or a list
    // membership change writes hundreds of records inside one millisecond and
    // they do not all become searchable together.
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                object("a", tied, "hs_lastmodifieddate"),
                object("b", tied, "hs_lastmodifieddate"),
                object("c", tied, "hs_lastmodifieddate"),
                object("d", tied, "hs_lastmodifieddate"),
            ],
            None,
        )))
        .mount(&server)
        .await;

    let source = source(&server.uri(), &["deals"]);

    // Run one: take the snapshot and stop on its last record.
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    assert_eq!(next(&mut stream).await.key().unwrap(), "deals:a");
    assert_eq!(next(&mut stream).await.key().unwrap(), "deals:b");
    let last = next(&mut stream).await;
    assert_eq!(last.key().unwrap(), "deals:c");
    assert_eq!(last.source.snapshot, Snapshot::Last);
    let committed = cursor_of(&last);
    drop(stream);

    // Run two: resume from exactly the committed position.
    let mut stream = source
        .changes(Resume::At(committed), SnapshotMode::Initial)
        .await
        .unwrap();
    let resumed = next(&mut stream).await;
    assert_eq!(
        resumed.key().unwrap(),
        "deals:d",
        "`d` shares the boundary millisecond with the committed position; a `>` cursor \
         drops it and nothing downstream can tell"
    );

    nothing_more(&mut stream).await;
}

/// An object type added to a running connector has history the topic has never
/// seen, so it backfills from zero rather than adopting the shared watermark —
/// which would be a gap covering everything that type had ever done.
#[tokio::test]
async fn an_object_type_added_after_the_fact_backfills_instead_of_gapping() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![object("1", 9_000, "hs_lastmodifieddate")], None)),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/tickets/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![object("7", 500, "hs_lastmodifieddate")], None)),
        )
        .mount(&server)
        .await;

    // A cursor written when only deals were captured. Note the deal's
    // timestamp is far ahead of the ticket's.
    let deals_only = source(&server.uri(), &["deals"]);
    let mut stream = deals_only
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let deal = next(&mut stream).await;
    assert_eq!(deal.source.snapshot, Snapshot::Last);
    let committed = cursor_of(&deal);
    drop(stream);

    // Tickets are added to the config and the connector restarts.
    let both = source(&server.uri(), &["deals", "tickets"]);
    let mut stream = both
        .changes(Resume::At(committed), SnapshotMode::Initial)
        .await
        .unwrap();
    let ticket = next(&mut stream).await;
    assert_eq!(
        ticket.key().unwrap(),
        "tickets:7",
        "the new type's history predates the stored watermark and must still arrive"
    );
    nothing_more(&mut stream).await;
}

// ── the request HubSpot actually receives ────────────────────────────────

/// Contacts file their modification time under `lastmodifieddate`; every other
/// object uses `hs_lastmodifieddate`. HubSpot answers a filter on an unknown
/// property with an empty result set rather than an error, so getting this
/// wrong yields a connector that runs forever and delivers nothing.
#[tokio::test]
async fn a_contacts_query_filters_and_sorts_on_the_contacts_specific_property() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/contacts/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![object("1", 4_000, "lastmodifieddate")], None)),
        )
        .mount(&server)
        .await;

    let source = source(&server.uri(), &["contacts"]);
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let record = next(&mut stream).await;
    assert_eq!(record.key().unwrap(), "contacts:1");

    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["filterGroups"][0]["filters"][0]["propertyName"],
        json!("lastmodifieddate")
    );
    assert_eq!(
        body["filterGroups"][0]["filters"][0]["operator"],
        json!("GTE")
    );
    assert_eq!(body["sorts"][0]["propertyName"], json!("lastmodifieddate"));
    assert_eq!(
        body["sorts"][0]["direction"],
        json!("ASCENDING"),
        "the cursor's safety argument is that every undelivered record sorts at or after \
         the last delivered one; descending inverts that into a gap"
    );
    // The operator listed only `name`, but a response without the timestamp
    // property carries no watermark and the cursor could never advance.
    let properties = body["properties"].as_array().unwrap();
    assert!(properties.contains(&json!("lastmodifieddate")));
    assert!(properties.contains(&json!("name")));
}

// ── rate limits ──────────────────────────────────────────────────────────

/// A `429` must cost latency and nothing else. In particular it must not end
/// the stream: `run_connector` reads an ended stream as "the source is
/// finished", commits the offset and returns `Ok(())`, so a throttled portal
/// would exit the process zero while the CRM kept changing.
#[tokio::test]
async fn a_rate_limited_poll_backs_off_and_keeps_the_stream_alive() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(page(vec![object("1", 1_000, "hs_lastmodifieddate")], None)),
        )
        .mount(&server)
        .await;

    let source = source(&server.uri(), &["deals"]);
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();

    let started = std::time::Instant::now();
    let record = next(&mut stream).await;
    assert_eq!(record.key().unwrap(), "deals:1");
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the Retry-After header must be honoured rather than retried immediately"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "one throttled attempt and one that succeeded"
    );
}

/// A rejected token is a configuration error, not a transient one. Retrying it
/// eight times only delays the moment an operator learns the token is wrong,
/// and spends the app's request budget doing it.
#[tokio::test]
async fn a_rejected_token_fails_at_once_rather_than_burning_the_retry_budget() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "message": "expired" })))
        .mount(&server)
        .await;

    let source = source(&server.uri(), &["deals"]);
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();

    let err = next_error(&mut stream).await;
    assert!(
        err.to_string().contains("private-app token"),
        "the error must name what is wrong: {err}"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "a 401 must not be retried"
    );
}

// ── the search result window ─────────────────────────────────────────────

/// HubSpot refuses to page past ~10,000 results, so a large backfill pages by
/// advancing the watermark instead of by a deeper offset. When the window fills
/// without the watermark moving, neither route can reach the remainder — and
/// skipping it is the one thing this crate never does. The connector stops and
/// names both the wall and the knob.
#[tokio::test]
async fn a_result_window_that_fills_without_advancing_the_watermark_stops_the_connector() {
    let server = MockServer::start().await;
    let tied = 7_000i64;
    Mock::given(method("POST"))
        .and(path("/crm/v3/objects/deals/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(
            vec![
                object("a", tied, "hs_lastmodifieddate"),
                object("b", tied, "hs_lastmodifieddate"),
            ],
            // Always more, always at the same millisecond.
            Some("2"),
        )))
        .mount(&server)
        .await;

    let mut settings = settings(&server.uri(), &["deals"]);
    // A two-result window, so the guard is reachable without ten thousand
    // fixtures. The production value is `SEARCH_RESULT_WINDOW`.
    settings.search_window = 2;
    settings.page_size = 2;
    let source = HubspotSource::with_token(settings, "test-token".to_string()).unwrap();

    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    assert_eq!(next(&mut stream).await.key().unwrap(), "deals:a");

    let err = next_error(&mut stream).await;
    let message = err.to_string();
    assert!(
        message.contains("search result window"),
        "the error must name the wall it hit: {message}"
    );
    assert!(
        message.contains("index_lag_ms"),
        "and the knob that resolves it: {message}"
    );
}
