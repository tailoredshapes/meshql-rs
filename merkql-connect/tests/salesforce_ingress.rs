//! Salesforce ingress, against a fake org.
//!
//! Everything here runs offline. `wiremock` stands in for Salesforce, which
//! means the failures that matter — an expired cursor, an expired session, a
//! truncated result set, a cursor from the wrong org — are *reproducible*
//! rather than described. A connector whose recovery paths have only ever been
//! reasoned about is a connector whose recovery paths do not work.
//!
//! The org's clock is served explicitly in the `Date` header rather than left
//! to the fake server, because the connector reads it to decide what is safe to
//! query, and a test that could not control it could not pin the window
//! arithmetic.

#![cfg(feature = "salesforce")]

use chrono::{DateTime, TimeZone, Utc};
use futures::StreamExt;
use merkql_connect::config::SalesforceAuth;
use merkql_connect::record::{Op, Snapshot};
use merkql_connect::salesforce::{Credentials, SalesforceOptions, SalesforceSource};
use merkql_connect::source::{CdcError, ChangeStream, CommitSource, Resume, SnapshotMode};
use serde_json::json;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// The org's "now" for most tests.
fn noon() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 30, 12, 0, 0).unwrap()
}

fn http_date(at: DateTime<Utc>) -> String {
    at.to_rfc2822()
}

/// A `Date` header the connector will read as the org's clock. Set explicitly
/// so the window arithmetic under test is deterministic.
fn clock(at: DateTime<Utc>) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("date", http_date(at).as_str())
        .set_body_json(json!([{"version": "62.0", "url": "/services/data/v62.0"}]))
}

/// Matches a SOQL query whose text contains `needle`.
///
/// Asserting on the query text is the only way to pin the bound shapes from
/// outside; the difference between `>=` and `>` on the low bound is invisible
/// in the records a healthy fake returns and catastrophic in a real org.
fn soql_contains(needle: &'static str) -> impl Fn(&Request) -> bool {
    move |req: &Request| {
        req.url
            .query_pairs()
            .find(|(k, _)| k == "q")
            .is_some_and(|(_, v)| v.contains(needle))
    }
}

async fn mock_token(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "00Dxx!fake",
            "instance_url": server.uri(),
            "token_type": "Bearer",
        })))
        .mount(server)
        .await;
}

async fn mock_clock(server: &MockServer, at: DateTime<Utc>) {
    Mock::given(method("GET"))
        .and(path("/services/data/"))
        .respond_with(clock(at))
        .mount(server)
        .await;
}

fn options(server: &MockServer) -> SalesforceOptions {
    SalesforceOptions {
        instance_url: server.uri(),
        api_version: "v62.0".to_string(),
        sobject: "Account".to_string(),
        fields: vec!["Name".to_string()],
        entity: "accounts".to_string(),
        auth: vec!["farm".to_string()].into(),
        auth: SalesforceAuth::ClientCredentials,
        // Long enough that the loop parks rather than spinning once the window
        // has been consumed, so a test taking N records is not racing a poller.
        poll_interval: Duration::from_secs(60),
        lag: Duration::from_secs(30),
        max_window: Duration::from_secs(3600),
        capture_deletes: true,
    }
}

/// Credentials injected directly: these tests must never depend on, or
/// disturb, the process environment. The one test that does exercise the
/// environment path says so in its name.
async fn source(options: SalesforceOptions) -> SalesforceSource {
    SalesforceSource::with_credentials(options, Credentials::new("id", "secret", None))
        .await
        .expect("the fake org accepts the client-credentials grant")
}

/// `expect_err` needs `Debug` on the success type, and neither a source nor a
/// boxed stream has one. Rather than deriving `Debug` on production types to
/// satisfy a test — which for anything holding [`Credentials`] would mean a
/// derivation that prints a secret — the success case is named here instead.
fn must_fail<T, E>(result: Result<T, E>, why: &str) -> E {
    match result {
        Ok(_) => panic!("{why}"),
        Err(e) => e,
    }
}

/// Pull `n` records, failing rather than hanging if the feed stalls.
async fn take(stream: &mut ChangeStream, n: usize) -> Vec<merkql_connect::ChangeRecord> {
    let mut out = Vec::new();
    for i in 0..n {
        let next = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .unwrap_or_else(|_| panic!("the feed stalled waiting for record {i}"))
            .unwrap_or_else(|| panic!("the feed ended before record {i}"));
        out.push(next.expect("no error expected on this feed"));
    }
    out
}

// ── snapshot-then-stream ────────────────────────────────────────────────

/// A cold start captures the streaming position first, emits what already
/// exists as `op: r`, and then streams from exactly that position — so nothing
/// falls between the snapshot and the feed.
///
/// Also pins the two rules that make the position safe: only the *last*
/// snapshot record is resumable, and only the *last* record of a live window
/// is, because a position naming a point inside a batch the connector has only
/// partly appended would be committed past records that never reached the topic.
#[tokio::test]
async fn a_cold_start_snapshots_then_streams_from_the_captured_position() {
    let server = MockServer::start().await;
    mock_token(&server).await;

    // First clock read is the snapshot's; every later one is the feed's, an
    // hour on, so exactly one live window opens.
    Mock::given(method("GET"))
        .and(path("/services/data/"))
        .respond_with(clock(noon()))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/services/data/"))
        .respond_with(clock(noon() + chrono::Duration::hours(1)))
        .with_priority(2)
        .mount(&server)
        .await;

    // The snapshot: everything before the captured target, and no lower bound.
    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/query"))
        .and(soql_contains("SystemModstamp < 2026-07-30T11:59:30Z"))
        .and(|req: &Request| !soql_contains("SystemModstamp >=")(req))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalSize": 2, "done": true,
            "records": [
                {
                    "attributes": {"type": "Account", "url": "/x"},
                    "Id": "001D000000IqhSLIAY",
                    "SystemModstamp": "2026-07-30T09:00:00.000+0000",
                    "CreatedDate": "2026-07-30T09:00:00.000+0000",
                    "Name": "Acme Poultry"
                },
                {
                    "attributes": {"type": "Account", "url": "/x"},
                    "Id": "001D000000IqhSMIAY",
                    "SystemModstamp": "2026-07-30T10:00:00.000+0000",
                    "CreatedDate": "2026-07-30T08:00:00.000+0000",
                    "Name": "Beta Hatchery"
                }
            ]
        })))
        .mount(&server)
        .await;

    // The live window: half-open on exactly the position the snapshot ended at.
    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/query"))
        .and(soql_contains(
            "SystemModstamp >= 2026-07-30T11:59:30Z AND SystemModstamp < 2026-07-30T12:59:30Z",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalSize": 1, "done": true,
            "records": [{
                "Id": "001D000000IqhSNIAY",
                "SystemModstamp": "2026-07-30T12:30:00.000+0000",
                "CreatedDate": "2026-07-30T08:00:00.000+0000",
                "Name": "Gamma Coop"
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/sobjects/Account/deleted/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "deletedRecords": [
                {"id": "001D000000IqhSOIAY", "deletedDate": "2026-07-30T12:40:00.000+0000"}
            ],
            "earliestDateAvailable": "2026-07-01T00:00:00.000+0000",
            "latestDateCovered": "2026-07-30T12:59:30.000+0000"
        })))
        .mount(&server)
        .await;

    let source = source(options(&server)).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .expect("a cold start opens");
    let records = take(&mut stream, 4).await;

    // ── the snapshot ──
    assert_eq!(records[0].op, Op::Read);
    assert_eq!(records[0].source.snapshot, Snapshot::True);
    assert_eq!(
        records[0].source.position, None,
        "a mid-snapshot position is not a streaming position"
    );
    assert_eq!(records[1].op, Op::Read);
    assert_eq!(records[1].source.snapshot, Snapshot::Last);
    assert_eq!(
        records[1].source.position.as_deref(),
        Some("modstamp:2026-07-30T11:59:30Z"),
        "the snapshot resumes at the position captured BEFORE it ran, so nothing \
         written during the snapshot can fall between it and the feed"
    );

    // ── the live window ──
    assert_eq!(records[2].op, Op::Update);
    assert_eq!(records[2].source.snapshot, Snapshot::False);
    assert_eq!(
        records[2].source.position, None,
        "only the last record of a window is resumable"
    );
    assert_eq!(records[2].key().as_deref(), Some("001D000000IqhSNIAY"));

    assert_eq!(records[3].op, Op::Delete, "the tombstone closes the window");
    assert!(records[3].after.as_ref().unwrap().deleted);
    assert_eq!(
        records[3].source.position.as_deref(),
        Some("modstamp:2026-07-30T12:59:30Z")
    );

    // Every record carries the connector's identity and the mesh's tokens.
    for record in &records {
        assert_eq!(record.source.connector, "salesforce");
        assert_eq!(record.source.entity, "accounts");
        let envelope = record.after.as_ref().unwrap();
        assert_eq!(
            envelope.auth,
            meshql_core::AuthMark::from(vec!["farm".to_string()])
        );
        assert_eq!(envelope.payload["_sobject"], json!("Account"));
        assert!(
            envelope.payload.get("attributes").is_none(),
            "Salesforce's per-row framing must not reach the payload"
        );
    }
}

/// `nextRecordsUrl` must be followed to exhaustion. A window that stopped at
/// the first page would advance the cursor past every record on the pages it
/// never read — a gap manufactured by the pagination itself.
#[tokio::test]
async fn a_window_follows_next_records_url_to_exhaustion() {
    let server = MockServer::start().await;
    mock_token(&server).await;
    mock_clock(&server, noon()).await;

    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalSize": 2, "done": false,
            "nextRecordsUrl": "/services/data/v62.0/query/01g000000000000-2000",
            "records": [{
                "Id": "001D000000IqhSLIAY",
                "SystemModstamp": "2026-07-30T09:00:00.000+0000",
                "CreatedDate": "2026-07-30T09:00:00.000+0000",
                "Name": "page one"
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/query/01g000000000000-2000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalSize": 2, "done": true,
            "records": [{
                "Id": "001D000000IqhSMIAY",
                "SystemModstamp": "2026-07-30T10:00:00.000+0000",
                "CreatedDate": "2026-07-30T09:00:00.000+0000",
                "Name": "page two"
            }]
        })))
        .mount(&server)
        .await;

    let source = source(options(&server)).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let records = take(&mut stream, 2).await;

    assert_eq!(
        records
            .iter()
            .map(|r| r.after.as_ref().unwrap().payload["Name"].clone())
            .collect::<Vec<_>>(),
        vec![json!("page one"), json!("page two")]
    );
    assert_eq!(records[1].source.snapshot, Snapshot::Last);
}

// ── unusable positions ──────────────────────────────────────────────────

/// The headline failure. Salesforce keeps delete tracking for about 30 days;
/// past that it answers `INVALID_REPLICATION_DATE`, and that verdict — the
/// server's, not a clock comparison of ours — must become
/// `UnusablePosition` and nothing else.
///
/// Resuming anyway would be an unusually deceptive skip: the *updates* since
/// the stale cursor are still queryable, so the connector would produce records
/// and look healthy while every deletion in the gap stayed invisible.
#[tokio::test]
async fn a_cursor_older_than_delete_tracking_is_an_unusable_position() {
    let server = MockServer::start().await;
    mock_token(&server).await;
    mock_clock(&server, noon()).await;

    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/sobjects/Account/deleted/"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!([{
            "message": "startDate before the earliest available date",
            "errorCode": "INVALID_REPLICATION_DATE"
        }])))
        .mount(&server)
        .await;

    // Queries are mounted too, so a connector that ignored the delete window
    // and carried on would succeed rather than fail for an unrelated reason.
    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/query"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"totalSize": 0, "done": true, "records": []})),
        )
        .mount(&server)
        .await;

    let source = source(options(&server)).await;
    let stale = "modstamp:2026-06-01T00:00:00Z";
    let err = must_fail(
        source
            .changes(Resume::At(stale.to_string()), SnapshotMode::WhenNeeded)
            .await,
        "a cursor outside delete tracking must not be honoured",
    );

    match err {
        CdcError::UnusablePosition {
            connector, reason, ..
        } => {
            assert_eq!(connector, "salesforce");
            assert!(
                reason.contains("delete tracking"),
                "the reason must name what was lost, got: {reason}"
            );
            assert!(
                reason.contains("looks healthy"),
                "the reason must explain why resuming would be deceptive, got: {reason}"
            );
        }
        other => panic!("expected UnusablePosition, got {other:?}"),
    }
}

/// A cursor ahead of the org's own clock is not skew — it is the wrong org.
/// The routine way to produce one is a sandbox refresh: the sandbox is rebuilt
/// from production, every record gets a new Id, and the stored watermark now
/// describes data that no longer exists. Resuming would emit nothing until the
/// sandbox's clock caught up, looking healthy the whole time.
#[tokio::test]
async fn a_cursor_ahead_of_the_orgs_clock_is_an_unusable_position() {
    let server = MockServer::start().await;
    mock_token(&server).await;
    mock_clock(&server, noon()).await;

    let source = source(options(&server)).await;
    let err = must_fail(
        source
            .changes(
                Resume::At("modstamp:2026-08-30T12:00:00Z".to_string()),
                SnapshotMode::WhenNeeded,
            )
            .await,
        "a cursor from another org must not be honoured",
    );

    match err {
        CdcError::UnusablePosition { reason, .. } => assert!(
            reason.contains("sandbox refresh"),
            "the reason must name how this happens, got: {reason}"
        ),
        other => panic!("expected UnusablePosition, got {other:?}"),
    }
}

/// A replay ID belongs to the Pub/Sub API, not to this build. Both report the
/// connector name `salesforce`, so `OffsetStore`'s connector/entity check
/// cannot catch the mismatch — the cursor tag is the only thing that can, and
/// without it a replay ID would be parsed on a guess.
#[tokio::test]
async fn a_replay_id_cursor_is_refused_rather_than_guessed_at() {
    let server = MockServer::start().await;
    mock_token(&server).await;
    mock_clock(&server, noon()).await;

    let source = source(options(&server)).await;
    let err = must_fail(
        source
            .changes(
                Resume::At("replay:AAAAAgAAAAAAAAA=".to_string()),
                SnapshotMode::WhenNeeded,
            )
            .await,
        "a replay ID is not a modstamp",
    );
    assert!(
        matches!(err, CdcError::UnusablePosition { .. }),
        "got {err:?}"
    );
}

/// The whole point of returning `UnusablePosition` rather than starting
/// somewhere else: the crate's policy then decides. `initial` must refuse to
/// start, because silently choosing a new start point is the gap the type
/// exists to prevent.
#[tokio::test]
async fn an_expired_cursor_stops_a_connector_that_may_not_re_snapshot() {
    use merkql::broker::{Broker, BrokerConfig};
    use merkql_connect::{run_connector, OffsetStore, TopicWriter};

    let server = MockServer::start().await;
    mock_token(&server).await;
    mock_clock(&server, noon()).await;
    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/sobjects/Account/deleted/"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!([{
            "message": "startDate before the earliest available date",
            "errorCode": "INVALID_REPLICATION_DATE"
        }])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/query"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"totalSize": 0, "done": true, "records": []})),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let broker = Broker::open(BrokerConfig::new(dir.path().join("merkql"))).unwrap();
    let writer = TopicWriter::claim(broker, "accounts", dir.path()).unwrap();
    let mut offsets = OffsetStore::open(
        dir.path().join("offsets.json"),
        "salesforce",
        "accounts",
        Duration::from_millis(0),
    )
    .unwrap();
    offsets.stage("modstamp:2026-06-01T00:00:00Z", false);
    offsets.commit_now().unwrap();

    let source = source(options(&server)).await;
    let err = run_connector(&source, &writer, &mut offsets, SnapshotMode::Initial)
        .await
        .expect_err("initial must refuse to start rather than skip");
    assert!(err.to_string().contains("Refusing to start"), "got: {err}");
}

// ── sessions ────────────────────────────────────────────────────────────

/// Salesforce access tokens expire on the org's session-timeout policy — two
/// hours by default — and can be revoked from Setup at any time. Without a
/// re-authentication the connector runs perfectly for two hours and then dies
/// with what looks, from the outside, exactly like a Salesforce outage.
#[tokio::test]
async fn an_expired_session_is_re_authenticated_once_and_the_request_retried() {
    let server = MockServer::start().await;
    mock_token(&server).await;
    mock_clock(&server, noon()).await;

    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/query"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!([{
            "message": "Session expired or invalid",
            "errorCode": "INVALID_SESSION_ID"
        }])))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalSize": 1, "done": true,
            "records": [{
                "Id": "001D000000IqhSLIAY",
                "SystemModstamp": "2026-07-30T09:00:00.000+0000",
                "CreatedDate": "2026-07-30T09:00:00.000+0000",
                "Name": "survived the session expiry"
            }]
        })))
        .with_priority(2)
        .mount(&server)
        .await;

    let source = source(options(&server)).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let records = take(&mut stream, 1).await;
    assert_eq!(
        records[0].after.as_ref().unwrap().payload["Name"],
        json!("survived the session expiry")
    );

    // Twice: once at open, once after the 401. Not more — a connector that
    // re-authenticates on every request trips an org's login rate limit.
    let logins = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/services/oauth2/token")
        .count();
    assert_eq!(logins, 2, "exactly one re-authentication");
}

/// A refused grant must fail at *startup*, in front of the operator, rather
/// than hours later inside a stream — the same reason the PostgreSQL source
/// creates its replication slot at open.
#[tokio::test]
async fn a_refused_grant_fails_at_startup_with_salesforces_own_reason() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/oauth2/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_client",
            "error_description": "client identifier invalid"
        })))
        .mount(&server)
        .await;

    let err = must_fail(
        SalesforceSource::with_credentials(options(&server), Credentials::new("id", "bad", None))
            .await,
        "a refused grant must not produce a source",
    );
    let text = err.to_string();
    assert!(text.contains("invalid_client"), "got: {text}");
    assert!(text.contains("SALESFORCE_CLIENT_ID"), "got: {text}");
}

/// The environment path itself. `Credentials::resolve` is unit-tested against a
/// synthetic lookup; this pins that `open` actually uses it, and that a
/// deployment which forgot to set the secrets is told which one is missing
/// rather than being handed an anonymous session.
#[tokio::test]
async fn credentials_absent_from_the_environment_are_a_clear_startup_error() {
    let server = MockServer::start().await;
    mock_token(&server).await;

    // Only this test touches the process environment, so nothing races it.
    std::env::remove_var("SALESFORCE_CLIENT_ID");
    std::env::remove_var("SALESFORCE_CLIENT_SECRET");
    std::env::remove_var("SALESFORCE_REFRESH_TOKEN");

    let err = must_fail(
        SalesforceSource::open(options(&server)).await,
        "an empty environment must not produce a source",
    );
    let text = err.to_string();
    assert!(
        text.contains("SALESFORCE_CLIENT_ID"),
        "the error must name the variable, got: {text}"
    );
    assert!(
        text.contains("never from the connector TOML"),
        "and must say where credentials do belong, got: {text}"
    );
}

// ── misconfiguration ────────────────────────────────────────────────────

/// An empty token list means PUBLIC in meshql. Refusing it at open is the only
/// place this can be caught before a topic full of CRM data is readable by
/// every consumer of the mesh.
#[tokio::test]
async fn a_source_with_no_authorized_tokens_refuses_to_open() {
    let server = MockServer::start().await;
    mock_token(&server).await;

    let mut options = options(&server);
    options.auth.clear();
    let err = must_fail(
        SalesforceSource::with_credentials(options, Credentials::new("id", "secret", None)).await,
        "CRM data must not become public by omission",
    );
    assert!(err.to_string().contains("PUBLIC"), "got: {err}");
}

/// SOQL has no `SELECT *`, and describing the object to select everything would
/// change the payload shape the day an admin adds a custom field — a breaking
/// change to every downstream fold with nothing in version control to show for
/// it.
#[tokio::test]
async fn a_source_with_no_fields_refuses_to_open() {
    let server = MockServer::start().await;
    mock_token(&server).await;

    let mut options = options(&server);
    options.fields.clear();
    let err = must_fail(
        SalesforceSource::with_credentials(options, Credentials::new("id", "secret", None)).await,
        "an empty field list is a misconfiguration, not a default",
    );
    assert!(err.to_string().contains("SELECT *"), "got: {err}");
}

/// Salesforce truncating a result set without giving a continuation URL must be
/// an error, not an early return. Treating it as the end of the window would
/// advance the cursor past every record Salesforce declined to send.
#[tokio::test]
async fn a_truncated_result_set_with_no_continuation_is_an_error() {
    let server = MockServer::start().await;
    mock_token(&server).await;
    mock_clock(&server, noon()).await;

    Mock::given(method("GET"))
        .and(path("/services/data/v62.0/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalSize": 5000, "done": false,
            "records": [{
                "Id": "001D000000IqhSLIAY",
                "SystemModstamp": "2026-07-30T09:00:00.000+0000",
                "CreatedDate": "2026-07-30T09:00:00.000+0000",
                "Name": "the only one we were given"
            }]
        })))
        .mount(&server)
        .await;

    let source = source(options(&server)).await;
    let err = must_fail(
        source.changes(Resume::Cold, SnapshotMode::Initial).await,
        "an incomplete window must not be mistaken for a complete one",
    );
    assert!(err.to_string().contains("nextRecordsUrl"), "got: {err}");
}
