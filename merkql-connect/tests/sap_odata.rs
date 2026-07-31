//! The SAP OData source, end to end against a faked service.
//!
//! **Nothing here touches a network.** `wiremock` binds a loopback HTTP server
//! and plays the part of an SAP gateway, which is the same thing
//! `sap-cdc-mcp`'s BDD suite does. That is not merely convenient: the failures
//! that matter here — an expired delta token, a service that does not
//! change-track, a tombstone that must land on the same envelope id as its
//! upsert — are all failures you cannot reliably *cause* on a real S/4HANA
//! system, so a test that needed one would be a test nobody ran.
//!
//! The v2 cases serve **Atom**, not JSON, and that is the point rather than an
//! aesthetic choice: an OData v2 service can only express a deletion as an Atom
//! `deleted-entry`, so a v2 suite written against JSON bodies would agree with a
//! connector that never sees a delete. See
//! [`a_v2_delta_tombstone_is_actually_observed`].

#![cfg(feature = "sap")]

use futures::StreamExt;
use merkql_connect::config::{SapAuthConfig, SapODataVersion};
use merkql_connect::record::{Op, Snapshot};
use merkql_connect::sap::SapSource;
use merkql_connect::{ChangeRecord, ChangeStream, CommitSource, Resume, SnapshotMode};
use serde_json::{json, Value};
use std::time::Duration;
use wiremock::matchers::{header, headers, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ENTITY_SET: &str = "A_BusinessPartnerAddress";
const SERVICE_PATH: &str = "/sap/opu/odata4/sap/API_BP";

fn keys() -> Vec<String> {
    vec!["BusinessPartner".to_string(), "AddressID".to_string()]
}

/// A source pointed at `server`, with no auth ceremony.
async fn source(server: &MockServer, auth: SapAuthConfig) -> SapSource {
    SapSource::open(
        &format!("{}{SERVICE_PATH}", server.uri()),
        ENTITY_SET,
        "business_partner_address",
        SapODataVersion::V4,
        &keys(),
        Some("LastChangeDateTime"),
        &["sap".to_string()],
        &auth,
        // Short, because a couple of tests wait for the next cycle. The idle
        // path is a timer by necessity — SAP OData has no notification edge.
        Duration::from_millis(50),
    )
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

fn address(bp: &str, address_id: &str, city: &str) -> Value {
    json!({
        "BusinessPartner": bp,
        "AddressID": address_id,
        "CityName": city,
        "LastChangeDateTime": "2026-07-31T09:00:00Z",
    })
}

fn meta(record: &ChangeRecord) -> Value {
    record.after.as_ref().unwrap().payload["_sap"].clone()
}

// ─────────────────────────────────────────────────────────────────────────────

/// The cold start: the tracked read *is* the snapshot and the position capture,
/// so history arrives as `op: r` and the delta link lands on the final record.
/// Anything else would leave a gap between "what the snapshot saw" and "where
/// the stream starts".
#[tokio::test]
async fn a_cold_start_snapshots_then_streams_from_the_captured_delta_link() {
    let server = MockServer::start().await;
    let delta = format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D1", server.uri());

    // The initial tracked read: two rows, page 1 of 2.
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param_is_missing("$deltatoken"))
        .and(query_param_is_missing("page"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("1", "1", "Leeds"), address("1", "2", "Hull")],
            "@odata.nextLink": format!("{}{SERVICE_PATH}/{ENTITY_SET}?page=2", server.uri()),
        })))
        .mount(&server)
        .await;

    // Page 2 closes the cycle and hands over the delta link.
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("2", "1", "York")],
            "@odata.deltaLink": delta,
        })))
        .mount(&server)
        .await;

    // The first live cycle, reached by following the delta link.
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param("$deltatoken", "D1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("3", "1", "Ripon")],
            "@odata.deltaLink": format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D2", server.uri()),
        })))
        .mount(&server)
        .await;

    let source = source(&server, SapAuthConfig::None).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();

    let records = take(&mut stream, 4).await;

    // Three snapshot reads, then one live create.
    assert_eq!(
        records.iter().map(|r| r.op).collect::<Vec<_>>(),
        vec![Op::Read, Op::Read, Op::Read, Op::Create]
    );
    assert_eq!(
        records
            .iter()
            .map(|r| r.source.snapshot)
            .collect::<Vec<_>>(),
        vec![
            Snapshot::True,
            Snapshot::True,
            Snapshot::Last,
            Snapshot::False
        ]
    );

    // Paging must not end the snapshot: `Last` belongs to the last row of the
    // whole cycle, not the last row of page 1.
    assert_eq!(
        records[0..2].iter().filter_map(|r| r.position()).count(),
        0,
        "no record inside a delta cycle may name a resumable position"
    );
    assert_eq!(
        records[2].position(),
        Some(delta.as_str()),
        "the snapshot's final record carries the captured delta link"
    );
    assert!(records[3].position().unwrap().contains("D2"));

    // And the ids are the composite-key encoding, sorted by property name.
    assert_eq!(
        records[0].key().unwrap(),
        "A_BusinessPartnerAddress(AddressID='1',BusinessPartner='1')"
    );
}

/// The envelope mapping: an OData entity becomes a meshql envelope whose
/// payload carries both the business properties and everything the `source`
/// block cannot get past a sink.
#[tokio::test]
async fn an_odata_entity_becomes_an_envelope_carrying_its_own_provenance() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("1000", "7", "Leeds")],
            "@odata.deltaLink": format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D1", server.uri()),
        })))
        .mount(&server)
        .await;

    let source = source(&server, SapAuthConfig::None).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let record = take(&mut stream, 1).await.pop().unwrap();
    let envelope = record.after.as_ref().unwrap();

    assert_eq!(
        envelope.id,
        "A_BusinessPartnerAddress(AddressID='7',BusinessPartner='1000')"
    );
    assert!(!envelope.deleted);
    // Authorisation is configured, not invented per record: SAP carries no
    // meshql tokens.
    assert_eq!(envelope.authorized_tokens, vec!["sap".to_string()]);
    assert_eq!(envelope.payload["CityName"], json!("Leeds"));

    // The `source` block does not survive a repository append, so the domain's
    // copy of it lives in the payload.
    let meta = meta(&record);
    assert_eq!(meta["entity_set"], json!(ENTITY_SET));
    assert_eq!(meta["odata_version"], json!("v4"));
    assert_eq!(meta["op"], json!("upsert"));
    assert_eq!(meta["key"]["BusinessPartner"], json!("1000"));
    assert_eq!(meta["key"]["AddressID"], json!("7"));
    assert!(meta["next_delta_token"].as_str().unwrap().contains("D1"));
    assert_eq!(meta["read_from_delta_token"], Value::Null);

    // The entity named its own change time, so `ts_ms` is SAP's and the payload
    // says so rather than passing our poll time off as business time.
    assert_eq!(meta["changed_at_source"], json!("entity"));
    assert_eq!(
        envelope.created_at.to_rfc3339(),
        "2026-07-31T09:00:00+00:00"
    );
    assert_eq!(record.source.ts_ms, envelope.created_at.timestamp_millis());
    assert_eq!(record.source.connector, "sap");
}

/// meshql has no delete: a tombstone is a new envelope version carrying the
/// flag. And it must land on **the same id** the upsert used, or the deletion
/// applies to an aggregate that does not exist.
#[tokio::test]
async fn a_delta_tombstone_becomes_a_deleted_envelope_on_the_same_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("1000", "7", "Leeds")],
            "@odata.deltaLink": format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D1", server.uri()),
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param("$deltatoken", "D1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [{
                "@removed": { "reason": "deleted" },
                "@id": format!("{ENTITY_SET}(BusinessPartner='1000',AddressID='7')"),
            }],
            "@odata.deltaLink": format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D2", server.uri()),
        })))
        .mount(&server)
        .await;

    let source = source(&server, SapAuthConfig::None).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let records = take(&mut stream, 2).await;

    let upsert = records[0].after.as_ref().unwrap();
    let tombstone = records[1].after.as_ref().unwrap();
    assert!(!upsert.deleted);
    assert!(tombstone.deleted);
    assert_eq!(
        tombstone.id, upsert.id,
        "a tombstone that lands on a different id deletes nothing"
    );
    // Still `op: c` — a deletion reaches the topic as a new version, per the
    // `Op` docs, not as Debezium's `d`.
    assert_eq!(records[1].op, Op::Create);
    assert_eq!(meta(&records[1])["op"], json!("delete"));
}

/// The failure `SnapshotMode::when_needed` exists for. SAP invalidates a delta
/// token after its change-tracking retention window; the connector must say so
/// and must never quietly start a fresh full read instead.
#[tokio::test]
async fn an_expired_delta_token_is_an_unusable_position() {
    let server = MockServer::start().await;
    let expired = format!(
        "{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=OLD",
        server.uri()
    );

    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param("$deltatoken", "OLD"))
        .respond_with(ResponseTemplate::new(410).set_body_string(
            r#"{"error":{"code":"/IWBEP/CM_MGW_RT/023","message":"Delta token expired"}}"#,
        ))
        .mount(&server)
        .await;

    let source = source(&server, SapAuthConfig::None).await;
    let mut stream = source
        .changes(Resume::At(expired.clone()), SnapshotMode::WhenNeeded)
        .await
        .expect("opening the feed succeeds; the rejection arrives on the stream");

    match stream.next().await {
        Some(Err(merkql_connect::CdcError::UnusablePosition {
            connector,
            position,
            reason,
        })) => {
            assert_eq!(connector, "sap");
            assert_eq!(position, expired);
            assert!(reason.contains("retention"), "got: {reason}");
        }
        other => panic!("expected UnusablePosition, got {other:?}"),
    }
}

/// SAP Gateway is not consistent about 410. A 400 whose error body names the
/// token is the same failure, and classifying it as a plain backend error would
/// leave `when_needed` unable to recover from the commonest real form of it.
#[tokio::test]
async fn a_400_naming_the_delta_token_is_also_an_unusable_position() {
    let server = MockServer::start().await;
    let expired = format!(
        "{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=OLD",
        server.uri()
    );

    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param("$deltatoken", "OLD"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"message":{"value":"Delta token '...' is no longer available"}}}"#,
        ))
        .mount(&server)
        .await;

    let source = source(&server, SapAuthConfig::None).await;
    let mut stream = source
        .changes(Resume::At(expired), SnapshotMode::WhenNeeded)
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await,
        Some(Err(merkql_connect::CdcError::UnusablePosition { .. }))
    ));
}

/// A 503 is a bad afternoon, not an expired token. Reporting it as an unusable
/// position would let `when_needed` re-replicate the whole entity set because
/// of a blip.
#[tokio::test]
async fn a_transient_service_failure_is_not_an_unusable_position() {
    let server = MockServer::start().await;
    let stored = format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D1", server.uri());

    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .respond_with(ResponseTemplate::new(503).set_body_string("gateway busy"))
        .mount(&server)
        .await;

    let source = source(&server, SapAuthConfig::None).await;
    let mut stream = source
        .changes(Resume::At(stored), SnapshotMode::WhenNeeded)
        .await
        .unwrap();
    match stream.next().await {
        Some(Err(e @ merkql_connect::CdcError::Backend(_))) => {
            assert!(e.to_string().contains("503"), "got: {e}")
        }
        other => panic!("expected a backend error, got {other:?}"),
    }
}

/// A stored position from a different SAP system — the shape a production
/// copy-back into QA takes. Following it would replicate the old system's
/// changes onto the new system's topic, quietly, for as long as the old host
/// answered.
#[tokio::test]
async fn a_delta_link_from_another_sap_system_is_an_unusable_position() {
    let server = MockServer::start().await;
    let source = source(&server, SapAuthConfig::None).await;

    let opened = source
        .changes(
            Resume::At("https://other-s4.example.com/api/A_X?$deltatoken=D1".into()),
            SnapshotMode::WhenNeeded,
        )
        .await;
    match opened {
        Ok(_) => panic!("a token from another system names no position in this one"),
        Err(merkql_connect::CdcError::UnusablePosition { reason, .. }) => {
            assert!(reason.contains("other-s4.example.com"), "got: {reason}")
        }
        Err(other) => panic!("expected UnusablePosition, got {other:?}"),
    }
}

/// A service that hands back no delta link cannot be streamed. The only thing
/// the connector could otherwise do is re-read the entity set forever, flooding
/// the topic with history on every poll while looking perfectly healthy — so
/// this is `NoFeed`, the error that exists to stop exactly that degradation.
#[tokio::test]
async fn a_service_that_does_not_change_track_is_refused_rather_than_polled_forever() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("1", "1", "Leeds")],
        })))
        .mount(&server)
        .await;

    let source = source(&server, SapAuthConfig::None).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    match stream.next().await {
        Some(Err(merkql_connect::CdcError::NoFeed { reason, .. })) => {
            assert!(reason.contains("delta link"), "got: {reason}")
        }
        other => panic!("expected NoFeed, got {other:?}"),
    }
}

/// `never` must not emit history — but it also must not *invent* a delta token,
/// because OData issues one only as the tail of a read. So the read happens and
/// its rows are dropped.
#[tokio::test]
async fn snapshot_mode_never_harvests_the_token_without_emitting_history() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("1", "1", "Leeds"), address("2", "1", "Hull")],
            "@odata.deltaLink": format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D1", server.uri()),
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param("$deltatoken", "D1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("9", "1", "Ripon")],
            "@odata.deltaLink": format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D2", server.uri()),
        })))
        .mount(&server)
        .await;

    let source = source(&server, SapAuthConfig::None).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Never)
        .await
        .unwrap();

    // The first record out is the live one, not either of the two the initial
    // read returned.
    let first = take(&mut stream, 1).await.pop().unwrap();
    assert_eq!(first.op, Op::Create);
    assert_eq!(
        first.key().unwrap(),
        "A_BusinessPartnerAddress(AddressID='1',BusinessPartner='9')"
    );
}

/// Auth, end to end: the source must actually present the credential, and the
/// credential must come from the environment rather than from the config.
#[tokio::test]
async fn basic_auth_credentials_come_from_the_environment_and_reach_the_service() {
    let server = MockServer::start().await;

    // base64("s4user:s4pass")
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(header("authorization", "Basic czR1c2VyOnM0cGFzcw=="))
        .and(header("prefer", "odata.track-changes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("1", "1", "Leeds")],
            "@odata.deltaLink": format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D1", server.uri()),
        })))
        .mount(&server)
        .await;

    // Anything without the header is a 401, so a source that failed to
    // authenticate fails the test rather than passing on a permissive mock.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string("no credentials"))
        .mount(&server)
        .await;

    std::env::set_var("MERKQL_TEST_SAP_USER", "s4user");
    std::env::set_var("MERKQL_TEST_SAP_PASS", "s4pass");
    let source = source(
        &server,
        SapAuthConfig::Basic {
            user_env: "MERKQL_TEST_SAP_USER".into(),
            pass_env: "MERKQL_TEST_SAP_PASS".into(),
        },
    )
    .await;

    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let record = take(&mut stream, 1).await.pop().unwrap();
    assert_eq!(record.op, Op::Read);
}

/// OAuth2 client credentials: fetch a bearer, then use it. The token exchange
/// must happen against the configured token URL with the grant SAP expects.
#[tokio::test]
async fn oauth2_client_credentials_exchanges_a_token_and_presents_the_bearer() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "bearer-abc",
            "expires_in": 3600,
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(header("authorization", "Bearer bearer-abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "value": [address("1", "1", "Leeds")],
            "@odata.deltaLink": format!("{}{SERVICE_PATH}/{ENTITY_SET}?$deltatoken=D1", server.uri()),
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    std::env::set_var("MERKQL_TEST_SAP_CID", "client-1");
    std::env::set_var("MERKQL_TEST_SAP_SECRET", "shh");
    let source = source(
        &server,
        SapAuthConfig::Oauth2Cc {
            token_url: format!("{}/oauth/token", server.uri()),
            client_id_env: "MERKQL_TEST_SAP_CID".into(),
            client_secret_env: "MERKQL_TEST_SAP_SECRET".into(),
            scope: None,
        },
    )
    .await;

    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    assert_eq!(take(&mut stream, 1).await.len(), 1);
}

/// A missing credential must stop the connector at `open`, before it claims a
/// merkql topic — not on the first poll an hour later.
#[tokio::test]
async fn a_missing_credential_environment_variable_fails_at_open() {
    let server = MockServer::start().await;
    std::env::remove_var("MERKQL_TEST_SAP_ABSENT");
    let opened = SapSource::open(
        &format!("{}{SERVICE_PATH}", server.uri()),
        ENTITY_SET,
        "bp",
        SapODataVersion::V4,
        &keys(),
        None,
        &[],
        &SapAuthConfig::Bearer {
            token_env: "MERKQL_TEST_SAP_ABSENT".into(),
        },
        Duration::from_millis(50),
    )
    .await;
    let Err(err) = opened else {
        panic!("a missing secret must fail at startup");
    };
    assert!(
        err.to_string().contains("MERKQL_TEST_SAP_ABSENT"),
        "the error must name the variable, got: {err}"
    );
}

// ── OData v2, which is Atom, because a v2 deletion exists nowhere else ───────

/// One Atom entry, as SAP Gateway renders an entity.
fn v2_entry(bp: &str, address_id: &str, city: &str) -> String {
    format!(
        r#"<entry>
             <id>https://s4.example.com/svc/{ENTITY_SET}(BusinessPartner='{bp}',AddressID='{address_id}')</id>
             <link rel="edit" href="{ENTITY_SET}(BusinessPartner='{bp}',AddressID='{address_id}')"/>
             <link rel="http://schemas.microsoft.com/ado/2007/08/dataservices/related/ToRegion"
                   href="{ENTITY_SET}(BusinessPartner='{bp}',AddressID='{address_id}')/ToRegion"/>
             <content type="application/xml">
               <m:properties>
                 <d:BusinessPartner>{bp}</d:BusinessPartner>
                 <d:AddressID>{address_id}</d:AddressID>
                 <d:CityName>{city}</d:CityName>
                 <d:LastChangeDateTime m:type="Edm.DateTime">2026-07-31T09:00:00</d:LastChangeDateTime>
               </m:properties>
             </content>
           </entry>"#
    )
}

/// An RFC 6721 tombstone — the only way an OData v2 service can say "gone".
fn v2_deleted_entry(bp: &str, address_id: &str) -> String {
    format!(
        r#"<at:deleted-entry
             ref="https://s4.example.com/svc/{ENTITY_SET}(BusinessPartner='{bp}',AddressID='{address_id}')"
             when="2026-07-31T10:30:00Z"/>"#
    )
}

fn v2_feed(entries: &str, delta_link: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:m="http://schemas.microsoft.com/ado/2007/08/dataservices/metadata"
      xmlns:d="http://schemas.microsoft.com/ado/2007/08/dataservices"
      xmlns:at="http://purl.org/atom/tombstones/1.0">
  <id>https://s4.example.com/svc/{ENTITY_SET}</id>
  <title type="text">{ENTITY_SET}</title>
  {entries}
  <link rel="delta" href="{delta_link}"/>
</feed>"#
    )
}

fn atom(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/atom+xml;type=feed;charset=utf-8")
}

async fn v2_source(server: &MockServer) -> SapSource {
    SapSource::open(
        &format!("{}{SERVICE_PATH}", server.uri()),
        ENTITY_SET,
        "business_partner_address",
        SapODataVersion::V2,
        &keys(),
        Some("LastChangeDateTime"),
        &["sap".to_string()],
        &SapAuthConfig::None,
        Duration::from_millis(50),
    )
    .await
    .expect("the v2 source opens")
}

/// **The defect this file exists to close.** An OData v2 service can only
/// express a deletion as an Atom `deleted-entry`; an earlier build asked for
/// JSON and looked for v4's `@sap.deleted_entity`, so on a real v2 service no
/// deletion was ever observed and nothing said so.
///
/// This test fails on that build and passes on this one, which is the whole
/// point: the tombstone must arrive, and it must arrive on the same envelope id
/// as the upsert it deletes.
#[tokio::test]
async fn a_v2_delta_tombstone_is_actually_observed() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param_is_missing("!deltatoken"))
        .respond_with(atom(v2_feed(
            &v2_entry("1000", "7", "Leeds"),
            &format!("{}{SERVICE_PATH}/{ENTITY_SET}?!deltatoken=D1", server.uri()),
        )))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param("!deltatoken", "D1"))
        .respond_with(atom(v2_feed(
            &v2_deleted_entry("1000", "7"),
            &format!("{}{SERVICE_PATH}/{ENTITY_SET}?!deltatoken=D2", server.uri()),
        )))
        .mount(&server)
        .await;

    let source = v2_source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let records = take(&mut stream, 2).await;

    let upsert = records[0].after.as_ref().unwrap();
    let tombstone = records[1].after.as_ref().unwrap();
    assert!(!upsert.deleted);
    assert!(
        tombstone.deleted,
        "a v2 deleted-entry must reach the topic as a deletion"
    );
    assert_eq!(
        tombstone.id, upsert.id,
        "a tombstone that lands on a different id deletes nothing"
    );
    assert_eq!(
        tombstone.id,
        "A_BusinessPartnerAddress(AddressID='7',BusinessPartner='1000')"
    );
    assert_eq!(records[1].op, Op::Create);
    assert_eq!(meta(&records[1])["op"], json!("delete"));

    // A tombstone has no properties for `changed_at_property` to name, so the
    // `when` attribute is the only honest time available — and the payload says
    // where it came from rather than passing our poll clock off as SAP's.
    assert_eq!(meta(&records[1])["changed_at_source"], json!("tombstone"));
    assert_eq!(
        tombstone.created_at.to_rfc3339(),
        "2026-07-31T10:30:00+00:00"
    );
}

/// An OData v2 service speaks Atom, not the v4 JSON envelope — and the request
/// must ask for it. `$format=json` is one of the options SAP documents as
/// mutually exclusive with a v2 delta query, and this is the read that issues
/// the delta token.
#[tokio::test]
async fn an_odata_v2_service_is_read_as_atom_and_produces_the_same_envelopes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .and(query_param_is_missing("$format"))
        .and(query_param_is_missing("$top"))
        // `headers` rather than `header` because wiremock splits a comma-joined
        // header into its values.
        .and(headers(
            "accept",
            vec!["application/atom+xml", "application/xml;q=0.9"],
        ))
        .respond_with(atom(v2_feed(
            &v2_entry("1000", "7", "Leeds"),
            &format!("{}{SERVICE_PATH}/{ENTITY_SET}?!deltatoken=D1", server.uri()),
        )))
        .mount(&server)
        .await;

    // Anything that asked for JSON, or pinned `$format`, falls through to here
    // and fails the test rather than quietly getting a JSON body.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(400).set_body_string("not a v2 delta-compatible read"))
        .mount(&server)
        .await;

    let source = v2_source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    let record = take(&mut stream, 1).await.pop().unwrap();

    assert_eq!(
        record.key().unwrap(),
        "A_BusinessPartnerAddress(AddressID='7',BusinessPartner='1000')",
        "the envelope id must not depend on which OData dialect the service speaks"
    );
    assert_eq!(meta(&record)["odata_version"], json!("v2"));
    assert_eq!(meta(&record)["changed_at_source"], json!("entity"));
    assert_eq!(
        record.after.as_ref().unwrap().payload["CityName"],
        json!("Leeds")
    );
    // `Edm.DateTime` has no offset and SAP means UTC. Normalising on the way in
    // is what stops the entity's own time being silently replaced by our clock.
    assert_eq!(
        record.after.as_ref().unwrap().created_at.to_rfc3339(),
        "2026-07-31T09:00:00+00:00"
    );
}

/// A v2 service that answers JSON anyway has no way to have told us about a
/// deletion. Reading it would look perfectly healthy while every delete was
/// dropped, so the cycle stops and names the limitation instead.
#[tokio::test]
async fn a_v2_service_answering_json_is_refused_rather_than_read_without_deletions() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{SERVICE_PATH}/{ENTITY_SET}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "d": {
                "results": [{ "BusinessPartner": "1000", "AddressID": "7" }],
                "__delta": format!("{}{SERVICE_PATH}/{ENTITY_SET}?!deltatoken=D1", server.uri()),
            }
        })))
        .mount(&server)
        .await;

    let source = v2_source(&server).await;
    let mut stream = source
        .changes(Resume::Cold, SnapshotMode::Initial)
        .await
        .unwrap();
    match stream.next().await {
        Some(Err(e @ merkql_connect::CdcError::Backend(_))) => {
            assert!(e.to_string().contains("deletion"), "got: {e}")
        }
        other => panic!("expected a refusal naming the v2 JSON limitation, got {other:?}"),
    }
}

/// The same refusal from the other direction: a stored v2 delta link that pins
/// JSON is a cursor written by a build that could not see deletions, so
/// following it would silently continue the gap. It is an unusable position, and
/// `when_needed` re-baselines onto a correct one.
#[tokio::test]
async fn a_stored_v2_delta_link_pinning_json_is_an_unusable_position() {
    let server = MockServer::start().await;
    let stored = format!(
        "{}{SERVICE_PATH}/{ENTITY_SET}?$format=json&!deltatoken=D1",
        server.uri()
    );

    let source = v2_source(&server).await;
    match source
        .changes(Resume::At(stored), SnapshotMode::WhenNeeded)
        .await
    {
        Err(merkql_connect::CdcError::UnusablePosition { reason, .. }) => {
            assert!(reason.contains("$format=json"), "got: {reason}")
        }
        Err(other) => panic!("expected UnusablePosition, got {other:?}"),
        Ok(_) => panic!("a delta link pinning JSON must not be followed"),
    }
}
