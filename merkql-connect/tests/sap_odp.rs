//! The SAP ODP source, end to end against a faked Operational Delta Queue.
//!
//! **Nothing here touches a network.** `wiremock` binds a loopback server and
//! plays the part of an ODP OData service. That is not merely convenient: the
//! failures that matter — an expired delta token, a subscription that has moved,
//! a change mode nobody has mapped, a service that hands back no delta link —
//! are all things you cannot reliably *cause* on a real S/4HANA system, so a
//! test that needed one would be a test nobody ran.
//!
//! Most of the file drives one fake, [`Odq`], which behaves like a delta queue
//! rather than like a canned response: rows accumulate, a delta token names a
//! position in them, and a request from a token returns what followed it. That
//! is what makes the certification suite meaningful here — the resume test is
//! actually resuming.

#![cfg(feature = "sap-odp")]

use futures::StreamExt;
use merkql_connect::cert::{self, CertStore};
use merkql_connect::config::{SapAuthConfig, SapODataVersion};
use merkql_connect::record::{Op, Snapshot};
use merkql_connect::sap_odp::{Cursor, SapOdpOptions, SapOdpSource};
use merkql_connect::{CdcError, ChangeRecord, ChangeStream, CommitSource, Resume, SnapshotMode};
use meshql_core::Envelope;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wiremock::matchers::{header, header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const SERVICE_PATH: &str = "/sap/opu/odata/SAP/ZODP_SRV";
const ODP: &str = "SEPM_SO";
const ENTITY_SET: &str = "FactsOfSEPM_SO";
const CLIENT: &str = "100";
const IDENTITY: &str = "ZODP_SO_SRV/MERKQL_CDC";

fn entity_path() -> String {
    format!("{SERVICE_PATH}/{ENTITY_SET}")
}

fn options(server_uri: &str, auth: SapAuthConfig) -> SapOdpOptions {
    SapOdpOptions {
        service_root: format!("{server_uri}{SERVICE_PATH}"),
        odp_name: ODP.to_string(),
        entity_set: None,
        client: CLIENT.to_string(),
        send_sap_client: true,
        subscriber_identity: IDENTITY.to_string(),
        odata_version: SapODataVersion::V2,
        entity: "sales_order".to_string(),
        key_properties: vec!["SalesOrder".to_string()],
        changed_at_property: Some("LastChangeDateTime".to_string()),
        auth: vec!["sap".to_string()].into(),
        page_size: 100,
        auth,
        // Short, because several tests wait for the next cycle. The idle path is
        // a timer by necessity — ODP OData has no notification edge.
        poll_interval: Duration::from_millis(50),
    }
}

async fn source(server: &MockServer) -> SapOdpSource {
    SapOdpSource::open(options(&server.uri(), SapAuthConfig::None))
        .await
        .expect("the source opens")
}

/// Pull `n` records, or say what did arrive.
async fn take(stream: &mut ChangeStream, n: usize) -> Vec<ChangeRecord> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while out.len() < n {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(Ok(record))) => out.push(record),
            Ok(Some(Err(e))) => panic!("stream error after {} records: {e}", out.len()),
            Ok(None) => panic!("stream ended after {} of {n} records", out.len()),
            Err(_) => panic!(
                "timed out waiting for record {} of {n}; got {:?}",
                out.len() + 1,
                out.iter().map(|r| r.key()).collect::<Vec<_>>()
            ),
        }
    }
    out
}

/// The first error the stream yields.
async fn first_error(stream: &mut ChangeStream) -> CdcError {
    match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
        Ok(Some(Err(e))) => e,
        Ok(Some(Ok(r))) => panic!("expected an error, got the record {:?}", r.key()),
        Ok(None) => panic!("expected an error, the stream ended"),
        Err(_) => panic!("expected an error, the stream stalled"),
    }
}

fn meta(record: &ChangeRecord) -> Value {
    record.after.as_ref().unwrap().payload["_sap_odp"].clone()
}

/// A v2 delta package: `d.results`, plus whichever link closes it.
fn package(rows: Vec<Value>, next: Option<String>, delta: Option<String>) -> Value {
    let mut d = json!({ "results": rows });
    if let Some(next) = next {
        d["__next"] = json!(next);
    }
    if let Some(delta) = delta {
        d["__delta"] = json!(delta);
    }
    json!({ "d": d })
}

fn order(id: &str, mode: &str, counter: i64, net: &str) -> Value {
    json!({
        "__metadata": {"uri": format!("…/{ENTITY_SET}('{id}')")},
        "SalesOrder": id,
        "NetAmount": net,
        "LastChangeDateTime": "2026-07-31T09:00:00Z",
        "ODQ_CHANGEMODE": mode,
        "ODQ_ENTITYCNTR": counter.to_string(),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// The fake delta queue
// ─────────────────────────────────────────────────────────────────────────────

/// A stand-in for the Operational Delta Queue: an append-only list of rows, a
/// delta token that names a position in it, and server-driven paging over the
/// slice a token opens.
///
/// It is a *queue*, not a canned response, because the properties worth
/// certifying are all about position — that a resume delivers what followed the
/// token and nothing before it, that a cycle's token covers everything the cycle
/// returned, that paging does not end the snapshot early. A mock that replayed a
/// fixed body would pass all three without exercising any of them.
#[derive(Clone, Default)]
struct Odq {
    rows: Arc<Mutex<Vec<Value>>>,
}

impl Odq {
    fn push(&self, row: Value) {
        self.rows.lock().expect("queue poisoned").push(row);
    }

    /// Mount this queue on `server`, serving `page_size` rows per page.
    async fn mount(&self, server: &MockServer, page_size: usize) {
        Mock::given(method("GET"))
            .and(path(entity_path()))
            .respond_with(OdqResponder {
                odq: self.clone(),
                base: format!("{}{}", server.uri(), entity_path()),
                page_size,
            })
            .mount(server)
            .await;
    }
}

struct OdqResponder {
    odq: Odq,
    base: String,
    page_size: usize,
}

impl Respond for OdqResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let option = |name: &str| -> Option<usize> {
            request
                .url
                .query_pairs()
                .find(|(k, _)| k == name)
                .and_then(|(_, v)| {
                    v.trim_matches('\'')
                        .trim_start_matches('D')
                        .parse::<usize>()
                        .ok()
                })
        };

        let rows = self.odq.rows.lock().expect("queue poisoned").clone();
        // `!skiptoken` pages within a package set; `!deltatoken` opens one. Both
        // are absolute positions here, which is all the connector can observe.
        let start = option("!skiptoken")
            .or_else(|| option("!deltatoken"))
            .unwrap_or(0)
            .min(rows.len());
        let end = (start + self.page_size).min(rows.len());
        let slice = rows[start..end].to_vec();

        let body = if end < rows.len() {
            // Mid-package-set: a next link and **no delta token**, which is what
            // SAP does — the token only ever arrives on the last page.
            package(slice, Some(format!("{}?!skiptoken={end}", self.base)), None)
        } else {
            package(
                slice,
                None,
                Some(format!("{}?!deltatoken='D{}'", self.base, rows.len())),
            )
        };
        ResponseTemplate::new(200).set_body_json(body)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Envelope mapping
// ─────────────────────────────────────────────────────────────────────────────

/// The cold start: the tracked read *is* the delta initialisation, the snapshot
/// and the position capture, so history arrives as `op: r` and the delta link
/// lands on the final record. Anything else leaves a gap between what the
/// snapshot saw and where the stream starts.
#[tokio::test]
async fn a_cold_start_snapshots_then_streams_from_the_captured_delta_link() {
    let server = MockServer::start().await;
    let odq = Odq::default();
    odq.push(order("1", "C", 1, "10"));
    odq.push(order("2", "C", 1, "20"));
    odq.mount(&server, 100).await;

    let source = source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();

    let snapshot = take(&mut stream, 2).await;
    assert_eq!(
        snapshot.iter().map(|r| r.op).collect::<Vec<_>>(),
        vec![Op::Read, Op::Read]
    );
    assert_eq!(
        snapshot
            .iter()
            .map(|r| r.source.snapshot)
            .collect::<Vec<_>>(),
        vec![Snapshot::True, Snapshot::Last]
    );
    assert_eq!(
        snapshot[0].position(),
        None,
        "no record inside a delta cycle may name a resumable position"
    );
    let cursor: Cursor = serde_json::from_str(snapshot[1].position().unwrap()).unwrap();
    assert!(cursor.delta_link.contains("D2"), "{cursor:?}");

    // A row written after the initial read arrives live, from the captured
    // token — the handover the snapshot exists to make gapless.
    odq.push(order("3", "C", 1, "30"));
    let live = take(&mut stream, 1).await;
    assert_eq!(live[0].op, Op::Create);
    assert_eq!(live[0].source.snapshot, Snapshot::False);
    assert_eq!(
        live[0].key().unwrap(),
        "sap_odp:100:SEPM_SO(SalesOrder='3')"
    );
}

/// An ODP row becomes a meshql envelope carrying both the business fields and
/// everything the Debezium `source` block cannot get past a repository sink.
#[tokio::test]
async fn an_odp_row_becomes_an_envelope_carrying_its_own_provenance() {
    let server = MockServer::start().await;
    let odq = Odq::default();
    odq.push(order("4711", "U", 1, "99.50"));
    odq.mount(&server, 100).await;

    let source = source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let record = take(&mut stream, 1).await.pop().unwrap();
    let envelope = record.after.as_ref().unwrap();

    assert_eq!(envelope.id, "sap_odp:100:SEPM_SO(SalesOrder='4711')");
    assert!(!envelope.deleted);
    // Authorisation is configured, not invented per record: SAP carries no
    // meshql tokens.
    assert_eq!(
        envelope.auth,
        meshql_core::AuthMark::from(vec!["sap".to_string()])
    );
    assert_eq!(envelope.payload["NetAmount"], json!("99.50"));

    // The queue's control columns are bookkeeping, not business data, and a fold
    // written against the ODP's field list should not be handed them.
    assert!(!envelope.payload.contains_key("ODQ_CHANGEMODE"));
    assert!(!envelope.payload.contains_key("ODQ_ENTITYCNTR"));
    assert!(!envelope.payload.contains_key("__metadata"));

    let meta = meta(&record);
    assert_eq!(meta["odp"], json!(ODP));
    assert_eq!(meta["entity_set"], json!(ENTITY_SET));
    assert_eq!(meta["client"], json!(CLIENT));
    assert_eq!(meta["subscriber_identity"], json!(IDENTITY));
    assert_eq!(meta["op"], json!("upsert"));
    assert_eq!(meta["change_mode"], json!("changed"));
    assert_eq!(meta["entity_counter"], json!(1));
    assert_eq!(meta["before_image"], json!(false));
    assert_eq!(meta["key"]["SalesOrder"], json!("4711"));
    assert_eq!(meta["read_from_delta_link"], Value::Null);
    assert!(meta["next_delta_link"].as_str().unwrap().contains("D1"));

    // The row named its own change time, so `created_at` is SAP's and the
    // payload says so rather than passing our poll time off as business time.
    assert_eq!(meta["changed_at_source"], json!("entity"));
    assert_eq!(
        envelope.created_at.to_rfc3339(),
        "2026-07-31T09:00:00+00:00"
    );
    assert_eq!(record.source.ts_ms, envelope.created_at.timestamp_millis());
    assert_eq!(record.source.connector, "sap_odp");
}

/// **The delete signal.** `ODQ_CHANGEMODE = 'D'` is the only thing that
/// distinguishes a deletion from an update, and meshql spells a deletion as a
/// new envelope version carrying `deleted: true` on **the same id** — a
/// tombstone under a different id retires an aggregate that does not exist.
#[tokio::test]
async fn a_change_mode_of_d_becomes_a_deleted_envelope_on_the_same_id() {
    let server = MockServer::start().await;
    let odq = Odq::default();
    odq.push(order("4711", "C", 1, "99.50"));
    odq.mount(&server, 100).await;

    let source = source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let live = take(&mut stream, 1).await.pop().unwrap();

    odq.push(order("4711", "D", -1, "99.50"));
    let gone = take(&mut stream, 1).await.pop().unwrap();

    let live = live.after.unwrap();
    let gone = gone.after.unwrap();
    assert_eq!(gone.id, live.id, "a deletion must land on the same id");
    assert!(!live.deleted);
    assert!(gone.deleted, "ODQ_CHANGEMODE 'D' is the delete signal");

    // And it has to *win* the version comparison. ODP hands the deletion over as
    // the row as it was, carrying the same LastChangeDateTime the live version
    // published under — stamped with that, the tombstone would tie on
    // `created_at` and the tiebreak is the id, which is identical.
    assert!(
        gone.created_at > live.created_at,
        "a tombstone that does not sort after the version it retires is an \
         undetectably ineffective delete: {} vs {}",
        gone.created_at,
        live.created_at
    );
}

/// A change mode nobody has mapped is a change whose *meaning* is unknown.
/// `R` and `N` are the trap — they are BW `RECORDMODE` values, not
/// `ODQ_CHANGEMODE` ones — and a connector that quietly accepted them would be
/// mapping a field it is not reading.
#[tokio::test]
async fn an_unmapped_change_mode_stops_the_stream_rather_than_guessing() {
    let server = MockServer::start().await;
    let odq = Odq::default();
    odq.push(order("1", "R", -1, "10"));
    odq.mount(&server, 100).await;

    let source = source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let error = first_error(&mut stream).await;
    let message = format!("{error}");
    assert!(message.contains("ODQ_CHANGEMODE"), "{message}");
    assert!(message.contains("\"R\""), "{message}");
    assert!(
        !matches!(error, CdcError::UnusablePosition { .. }),
        "an unmapped value is not a position failure"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Paging
// ─────────────────────────────────────────────────────────────────────────────

/// Server-driven paging must read the **whole** package set. With
/// `Prefer: odata.maxpagesize` the delta token arrives only on the last page, so
/// a cycle that stopped early would have no cursor at all — and `Snapshot::Last`
/// belongs to the final row of the cycle, not the final row of page one.
#[tokio::test]
async fn a_multi_page_package_set_is_read_to_the_end_before_anything_resumes() {
    let server = MockServer::start().await;
    let odq = Odq::default();
    for i in 1..=7 {
        odq.push(order(&i.to_string(), "C", 1, "10"));
    }
    // Three per page: two full pages, then a partial one. The boundary cases the
    // checklist asks for — exactly one full page, and one page plus one — both
    // fall inside a 3/3/1 split.
    odq.mount(&server, 3).await;

    let source = source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let records = take(&mut stream, 7).await;

    assert_eq!(
        records.iter().map(|r| r.key().unwrap()).collect::<Vec<_>>(),
        (1..=7)
            .map(|i| format!("sap_odp:100:SEPM_SO(SalesOrder='{i}')"))
            .collect::<Vec<_>>(),
        "every page of the set must arrive, in order"
    );

    // The snapshot ends at the end of the *cycle*, not the end of a page.
    assert!(
        records[..6]
            .iter()
            .all(|r| r.source.snapshot == Snapshot::True),
        "a page boundary is not the end of a snapshot"
    );
    assert_eq!(records[6].source.snapshot, Snapshot::Last);

    // And only that last record names a position. A crash mid-cycle must replay
    // the cycle, not resume past changes that were never appended.
    assert_eq!(
        records.iter().filter(|r| r.position().is_some()).count(),
        1,
        "a delta cycle has exactly one resumable position"
    );
    assert!(records[6].position().is_some());
}

/// The request has to carry both preferences, or the service returns an
/// untracked read with no delta link and this becomes a full-table re-reader.
/// The client also goes on the wire, because an ODP is resolved inside the logon
/// client.
#[tokio::test]
async fn the_tracked_read_asks_for_change_tracking_the_page_size_and_the_client() {
    let server = MockServer::start().await;
    // `header_regex` rather than `header`, and that is not a stylistic choice:
    // wiremock reads a comma-containing header value as the *list* HTTP says it
    // is, so an exact match on the combined `Prefer` never matches even when the
    // bytes on the wire are right. The two preferences are asserted separately.
    Mock::given(method("GET"))
        .and(path(entity_path()))
        .and(header_regex("prefer", "odata\\.track-changes"))
        .and(header_regex("prefer", "odata\\.maxpagesize=100"))
        .and(query_param("sap-client", CLIENT))
        .respond_with(ResponseTemplate::new(200).set_body_json(package(
            vec![order("1", "C", 1, "10")],
            None,
            Some(format!(
                "{}{}?!deltatoken='D1'",
                server.uri(),
                entity_path()
            )),
        )))
        .mount(&server)
        .await;
    // Anything that does not carry them fails the test rather than passing on a
    // permissive mock.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_string("missing Prefer or sap-client"))
        .mount(&server)
        .await;

    let source = source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    assert_eq!(take(&mut stream, 1).await[0].op, Op::Read);
}

// ─────────────────────────────────────────────────────────────────────────────
// Positions
// ─────────────────────────────────────────────────────────────────────────────

/// An expired or unknown delta token must be `UnusablePosition`, so
/// `snapshot_mode` decides whether to re-baseline or stop. Quietly starting a
/// fresh tracked read would republish the entire ODP with nothing in the
/// configuration having asked for it — and would do it *without* the operator
/// learning the token had expired, so the next expiry would be just as
/// invisible.
#[tokio::test]
async fn an_expired_delta_token_is_an_unusable_position() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(entity_path()))
        .respond_with(ResponseTemplate::new(410).set_body_string("delta token no longer available"))
        .mount(&server)
        .await;

    let source = source(&server).await;
    let cursor = Cursor {
        v: 1,
        subscriber_identity: IDENTITY.to_string(),
        odp: ODP.to_string(),
        client: CLIENT.to_string(),
        delta_link: format!("{}{}?!deltatoken='D7'", server.uri(), entity_path()),
    };

    let mut stream = source
        .changes(Resume::At(cursor.encode()), SnapshotMode::Initial)
        .await
        .expect("the stored cursor decodes; the service rejects it on the wire");
    let error = first_error(&mut stream).await;
    assert!(
        matches!(error, CdcError::UnusablePosition { .. }),
        "got {error}"
    );
}

/// SAP Gateway is documented to be inconsistent about `410`. A `400` whose body
/// names the delta queue is the same event, and reading it as transient means
/// retrying a doomed request forever while looking healthy.
#[tokio::test]
async fn a_400_naming_the_delta_queue_is_also_an_unusable_position() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(entity_path()))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("Subscription for delta token not found in ODQ"),
        )
        .mount(&server)
        .await;

    let source = source(&server).await;
    let cursor = Cursor {
        v: 1,
        subscriber_identity: IDENTITY.to_string(),
        odp: ODP.to_string(),
        client: CLIENT.to_string(),
        delta_link: format!("{}{}?!deltatoken='D7'", server.uri(), entity_path()),
    };
    let mut stream = source
        .changes(Resume::At(cursor.encode()), SnapshotMode::Initial)
        .await
        .unwrap();
    assert!(matches!(
        first_error(&mut stream).await,
        CdcError::UnusablePosition { .. }
    ));
}

/// **The subscription-identity guard.** ODP identifies a subscription by the
/// OData service and the logon user and sends nothing on the wire, so a
/// credential change hands the connector a *different queue* and SAP answers with
/// a fresh full load rather than an error. The declared identity is bound into
/// the cursor precisely so that changing it is a decision with a diff attached
/// instead of a silent re-baseline.
#[tokio::test]
async fn a_cursor_from_another_subscription_is_an_unusable_position() {
    let server = MockServer::start().await;
    let odq = Odq::default();
    odq.mount(&server, 100).await;
    let source = source(&server).await;

    let foreign = Cursor {
        v: 1,
        subscriber_identity: "ZODP_SO_SRV/SOMEONE_ELSE".to_string(),
        odp: ODP.to_string(),
        client: CLIENT.to_string(),
        delta_link: format!("{}{}?!deltatoken='D1'", server.uri(), entity_path()),
    };
    match source
        .changes(Resume::At(foreign.encode()), SnapshotMode::Initial)
        .await
    {
        Err(CdcError::UnusablePosition { reason, .. }) => {
            assert!(reason.contains("SOMEONE_ELSE"), "{reason}");
        }
        Err(other) => panic!("expected an unusable position, got {other}"),
        Ok(_) => panic!("a cursor from another subscription must not be followed"),
    }

    // Same for the client, which is part of every envelope id: continuing would
    // stamp another client's ids onto this client's topic.
    let other_client = Cursor {
        v: 1,
        subscriber_identity: IDENTITY.to_string(),
        odp: ODP.to_string(),
        client: "200".to_string(),
        delta_link: format!("{}{}?!deltatoken='D1'", server.uri(), entity_path()),
    };
    assert!(matches!(
        source
            .changes(Resume::At(other_client.encode()), SnapshotMode::Initial)
            .await,
        Err(CdcError::UnusablePosition { .. })
    ));
}

/// A cursor of the wrong shape — an older encoding, or another connector's
/// offset file — is an unusable position, never a backend error and never a
/// silent reinterpretation.
#[tokio::test]
async fn a_cursor_this_build_cannot_read_is_an_unusable_position() {
    let server = MockServer::start().await;
    Odq::default().mount(&server, 100).await;
    let source = source(&server).await;

    for raw in [
        "not json at all".to_string(),
        json!({"v": 99, "subscriber_identity": IDENTITY, "odp": ODP, "client": CLIENT,
               "delta_link": format!("{}{}", server.uri(), entity_path())})
        .to_string(),
    ] {
        assert!(
            matches!(
                source
                    .changes(Resume::At(raw.clone()), SnapshotMode::Initial)
                    .await,
                Err(CdcError::UnusablePosition { .. })
            ),
            "{raw}"
        );
    }
}

/// A copy-back from production into QA leaves the old system's delta link in the
/// offset file. Following it would replicate the old system's changes onto the
/// new system's topic, quietly, for as long as the old host stayed reachable.
#[tokio::test]
async fn a_delta_link_from_another_system_is_an_unusable_position() {
    let server = MockServer::start().await;
    Odq::default().mount(&server, 100).await;
    let source = source(&server).await;

    let elsewhere = Cursor {
        v: 1,
        subscriber_identity: IDENTITY.to_string(),
        odp: ODP.to_string(),
        client: CLIENT.to_string(),
        delta_link: format!(
            "https://s4-prod.example.com{}?!deltatoken='D1'",
            entity_path()
        ),
    };
    assert!(matches!(
        source
            .changes(Resume::At(elsewhere.encode()), SnapshotMode::Initial)
            .await,
        Err(CdcError::UnusablePosition { .. })
    ));
}

/// A service that answers without a delta link is not change-tracking this read.
/// The only thing the connector *could* do is re-read the ODP forever, emitting
/// every row on every cycle while looking healthy — so it refuses instead. This
/// is also the shape SAP KBA 2825795 produces (JSON plus server-side paging),
/// which is exactly this connector's combination.
#[tokio::test]
async fn a_read_that_yields_no_delta_link_refuses_to_become_a_full_table_poller() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(entity_path()))
        .respond_with(ResponseTemplate::new(200).set_body_json(package(
            vec![order("1", "C", 1, "10")],
            None,
            None,
        )))
        .mount(&server)
        .await;

    let source = source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    assert!(matches!(
        first_error(&mut stream).await,
        CdcError::NoFeed { .. }
    ));
}

/// `snapshot_mode = never` still makes the tracked read — SAP documents that a
/// delta initialisation *is* a full load and cannot be skipped — but the rows are
/// dropped rather than published, and no token is ever manufactured.
#[tokio::test]
async fn never_mode_pays_for_the_full_load_and_publishes_none_of_it() {
    let server = MockServer::start().await;
    let odq = Odq::default();
    odq.push(order("1", "C", 1, "10"));
    odq.push(order("2", "C", 1, "20"));
    odq.mount(&server, 100).await;

    let source = source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();

    // Nothing from before the start.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), stream.next())
            .await
            .is_err(),
        "history must not be republished under `never`"
    );

    // But the feed is live, not merely silent.
    odq.push(order("3", "C", 1, "30"));
    let live = take(&mut stream, 1).await;
    assert_eq!(
        live[0].key().unwrap(),
        "sap_odp:100:SEPM_SO(SalesOrder='3')"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Auth
// ─────────────────────────────────────────────────────────────────────────────

/// Credentials come from the environment and reach the service. The connector
/// config names the variable; the deployment supplies the value, because a
/// connector TOML is a file that gets copied into tickets.
#[tokio::test]
async fn basic_auth_credentials_come_from_the_environment_and_reach_the_service() {
    let server = MockServer::start().await;

    // base64("s4user:s4pass")
    Mock::given(method("GET"))
        .and(path(entity_path()))
        .and(header("authorization", "Basic czR1c2VyOnM0cGFzcw=="))
        .respond_with(ResponseTemplate::new(200).set_body_json(package(
            vec![order("1", "C", 1, "10")],
            None,
            Some(format!(
                "{}{}?!deltatoken='D1'",
                server.uri(),
                entity_path()
            )),
        )))
        .mount(&server)
        .await;
    // Anything without the header is a 401, so a source that failed to
    // authenticate fails the test rather than passing on a permissive mock.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string("no credentials"))
        .mount(&server)
        .await;

    std::env::set_var("MERKQL_TEST_ODP_USER", "s4user");
    std::env::set_var("MERKQL_TEST_ODP_PASS", "s4pass");
    let source = SapOdpSource::open(options(
        &server.uri(),
        SapAuthConfig::Basic {
            user_env: "MERKQL_TEST_ODP_USER".into(),
            pass_env: "MERKQL_TEST_ODP_PASS".into(),
        },
    ))
    .await
    .expect("the source opens");

    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    assert_eq!(take(&mut stream, 1).await[0].op, Op::Read);
}

/// An unset variable is a startup error naming it, not a connector that opens
/// and then fails on the first poll — by which time it has already claimed a
/// merkql topic.
#[tokio::test]
async fn a_missing_credential_variable_fails_at_open() {
    let server = MockServer::start().await;
    match SapOdpSource::open(options(
        &server.uri(),
        SapAuthConfig::Bearer {
            token_env: "MERKQL_TEST_ODP_DEFINITELY_UNSET".into(),
        },
    ))
    .await
    {
        Err(error) => assert!(
            format!("{error}").contains("MERKQL_TEST_ODP_DEFINITELY_UNSET"),
            "{error}"
        ),
        Ok(_) => panic!("an unset credential variable must not open"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Configuration guards
// ─────────────────────────────────────────────────────────────────────────────

/// The identity components of the envelope id and of the queue are checked at
/// open, because every one of them is a silent merge or a silent re-baseline
/// discovered later by counting rows.
#[tokio::test]
async fn the_identity_components_are_checked_before_the_topic_is_claimed() {
    let server = MockServer::start().await;
    let uri = server.uri();

    let empty_keys = SapOdpOptions {
        key_properties: vec![],
        ..options(&uri, SapAuthConfig::None)
    };
    assert!(SapOdpSource::open(empty_keys).await.is_err());

    let control_column_key = SapOdpOptions {
        key_properties: vec!["ODQ_CHANGEMODE".to_string()],
        ..options(&uri, SapAuthConfig::None)
    };
    assert!(SapOdpSource::open(control_column_key).await.is_err());

    let no_identity = SapOdpOptions {
        subscriber_identity: "  ".to_string(),
        ..options(&uri, SapAuthConfig::None)
    };
    assert!(SapOdpSource::open(no_identity).await.is_err());

    let forgeable_client = SapOdpOptions {
        client: "100:SEPM_SO(SalesOrder='9'".to_string(),
        ..options(&uri, SapAuthConfig::None)
    };
    assert!(SapOdpSource::open(forgeable_client).await.is_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// Certification
// ─────────────────────────────────────────────────────────────────────────────
//
// Three of the suite's four sub-tests run **as written**, against the fake delta
// queue, using the `CertStore::envelope_id` hook — this connector is the first
// source that derives its ids, and the hook exists so that "the ids you derive
// are the ids that come back" is asserted by the shared contract rather than by
// the connector about itself.
//
// `cert::certify_positions_are_present_and_distinct` is **excluded**, and the
// reason is a property of the mechanism rather than a shortcut:
//
//   That sub-test requires *every* record to carry a position. A delta cycle has
//   exactly one — the delta link the service issues at the end of it — so every
//   record but the last legitimately carries `position: None`. That is not an
//   oversight; it is the rule the crate states in `sink.rs` and the skill states
//   as the fan-out rule, and inventing interior positions would let a restart
//   resume past changes it never appended. The sub-test's assumption holds for a
//   row-cursor source and is false for a cycle-cursor one.
//
// The property it exists to catch — a watermark cursor that ties across records
// changed in one bulk edit — is still worth asserting, so it is transliterated
// below over three rows sharing one modification timestamp. See
// `positions_are_distinct_across_cycles_even_when_timestamps_tie`.

struct OdpCert {
    server: MockServer,
    odq: Odq,
}

#[async_trait::async_trait]
impl CertStore for OdpCert {
    /// Make the fake serve a row that derives to this envelope.
    async fn write(&self, envelope: Envelope) -> anyhow::Result<()> {
        self.odq.push(json!({
            "SalesOrder": envelope.id,
            "marker": envelope.payload.get("marker"),
            "ODQ_CHANGEMODE": "C",
            "ODQ_ENTITYCNTR": "1",
        }));
        Ok(())
    }

    async fn source(&self) -> anyhow::Result<Box<dyn CommitSource>> {
        let options = SapOdpOptions {
            auth: vec!["cert".to_string()].into(),
            // The suite's envelopes carry no timestamp, so `created_at` comes
            // from the monotonic observation clock — which is exactly the
            // production shape for an ODP with no last-changed field.
            changed_at_property: None,
            ..options(&self.server.uri(), SapAuthConfig::None)
        };
        Ok(Box::new(SapOdpSource::open(options).await?))
    }

    /// This is the hook the whole suite hinges on for an ingress connector: the
    /// id the source will *deliver* for the logical record the suite wrote.
    fn envelope_id(&self, logical: &str) -> String {
        format!("sap_odp:{CLIENT}:{ODP}(SalesOrder='{logical}')")
    }
}

async fn odp_cert() -> OdpCert {
    let server = MockServer::start().await;
    let odq = Odq::default();
    odq.mount(&server, 100).await;
    OdpCert { server, odq }
}

#[tokio::test]
async fn odp_certifies_snapshot_then_stream() {
    cert::certify_snapshot_then_stream(&odp_cert().await)
        .await
        .unwrap();
}

#[tokio::test]
async fn odp_certifies_resume() {
    cert::certify_resume_delivers_only_what_follows(&odp_cert().await)
        .await
        .unwrap();
}

#[tokio::test]
async fn odp_certifies_never_mode() {
    cert::certify_never_mode_skips_history(&odp_cert().await)
        .await
        .unwrap();
}

/// A transliteration of `cert::certify_positions_are_present_and_distinct`,
/// asserting the property that applies to a cycle-cursor source.
///
/// The suite's version requires a position on every record, which a delta cycle
/// cannot honestly give. What it exists to catch is a **watermark** cursor: three
/// records stamped by one bulk edit share a modification timestamp, and a source
/// cursoring on that timestamp either skips them forever (`>`) or replays them
/// forever (`>=`). So the rows here deliberately share one
/// `LastChangeDateTime` — the production case — and the assertions are that all
/// three arrive, that the positions which *are* present are distinct, and that a
/// resume from the last one delivers nothing already seen.
#[tokio::test]
async fn positions_are_distinct_across_cycles_even_when_timestamps_tie() {
    let server = MockServer::start().await;
    let odq = Odq::default();
    odq.mount(&server, 100).await;

    let source = source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();

    // One bulk edit: three rows, one timestamp. `order` stamps them all with
    // 2026-07-31T09:00:00Z.
    let mut positions = Vec::new();
    for i in 1..=3 {
        odq.push(order(&i.to_string(), "C", 1, "10"));
        let record = take(&mut stream, 1).await.pop().unwrap();
        assert_eq!(
            record.key().unwrap(),
            format!("sap_odp:100:SEPM_SO(SalesOrder='{i}')"),
            "a bulk edit must not lose the rows that tie on its timestamp"
        );
        positions.push(
            record
                .position()
                .expect("the final record of a cycle names a position")
                .to_string(),
        );
    }

    let mut sorted = positions.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        positions.len(),
        "positions repeat, so a restart cannot tell the cycles apart: {positions:?}"
    );

    // And the last of them resumes cleanly: nothing already delivered comes back
    // before the next real change does.
    drop(stream);
    odq.push(order("4", "C", 1, "10"));
    let resumed = source
        .changes(
            Resume::At(positions.last().unwrap().clone()),
            SnapshotMode::Initial,
        )
        .await
        .unwrap();
    let mut resumed = resumed;
    let next = take(&mut resumed, 1).await.pop().unwrap();
    assert_eq!(
        next.key().unwrap(),
        "sap_odp:100:SEPM_SO(SalesOrder='4')",
        "resuming must deliver what followed the position and nothing before it"
    );
}
