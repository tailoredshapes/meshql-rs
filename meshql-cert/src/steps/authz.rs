//! Steps for the end-to-end authorization certification.
//!
//! Every step here goes over the wire. Writes are `POST`/`DELETE` against the
//! restlette, reads are GraphQL against the graphlette, and credentials ride
//! in on the trusted identity header the edge would set. The one exception is
//! the storage-layer assertion in `stored_envelope_carries_tokens`, which
//! reaches past HTTP on purpose: it certifies that the tokens the `Auth`
//! resolved are the tokens that actually got persisted.

use cucumber::{given, then, when};
use serde_json::{json, Value};

use crate::authz::{self, GRAPHLETTE_PATH, IDENTITY_HEADER, RESTLETTE_PATH};
use crate::world::CertWorld;

/// The tokens a caller reads with when *nothing* is meant to be filtered —
/// used only to inspect storage directly, never to certify a read path.
fn inspector_tokens() -> Vec<String> {
    vec!["*".to_string()]
}

fn request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    caller: &str,
) -> reqwest::RequestBuilder {
    let req = client.request(method, url);
    match authz::identity_of(caller) {
        Some(id) => req.header(IDENTITY_HEADER, id),
        // An anonymous caller sends no identity header at all.
        None => req,
    }
}

async fn graphql(world: &CertWorld, caller: &str, query: &str) -> Value {
    let client = reqwest::Client::new();
    let url = format!("{}{}", world.server_addr(), GRAPHLETTE_PATH);
    request(&client, reqwest::Method::POST, &url, caller)
        .json(&json!({ "query": query }))
        .send()
        .await
        .expect("graphlette request")
        .json()
        .await
        .expect("graphlette response is JSON")
}

fn assert_no_gql_errors(resp: &Value) {
    if let Some(errors) = resp.get("errors") {
        if !errors.is_null() {
            panic!("GraphQL errors: {errors}");
        }
    }
}

fn names_of(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap_or_else(|| panic!("expected an array, got: {items}"))
        .iter()
        .filter_map(|i| i.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect()
}

fn widget_id(world: &CertWorld, name: &str) -> String {
    world
        .authz_ids
        .get(name)
        .unwrap_or_else(|| panic!("no widget named '{name}' was created"))
        .clone()
}

// ---- Given ----

#[given("an authorizing MeshQL server is running")]
async fn server_running(world: &mut CertWorld) {
    assert!(
        world.server_addr.is_some(),
        "server_addr must be set by the test runner's before-hook"
    );
    assert!(
        world.has_repo(),
        "the backing repository must be set by the test runner's before-hook, \
         so the write path can be inspected at the storage layer"
    );
}

// ---- When: writes through the restlette ----

#[when(regex = r#"^"([^"]+)" creates a widget "([^"]+)" of kind "([^"]+)"$"#)]
async fn create_widget(world: &mut CertWorld, caller: String, name: String, kind: String) {
    let client = reqwest::Client::new();
    let url = format!("{}{}", world.server_addr(), RESTLETTE_PATH);
    let resp = request(&client, reqwest::Method::POST, &url, &caller)
        .json(&json!({ "name": name, "kind": kind }))
        .send()
        .await
        .expect("restlette create");
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.expect("create response is JSON");
    assert_eq!(status, 201, "create of '{name}' failed: {body}");

    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("create response carried no id: {body}"))
        .to_string();
    world.authz_ids.insert(name, id);
}

#[when(regex = r#"^"([^"]+)" deletes widget "([^"]+)"$"#)]
async fn delete_widget(world: &mut CertWorld, caller: String, name: String) {
    let client = reqwest::Client::new();
    let id = widget_id(world, &name);
    let url = format!("{}{}/{}", world.server_addr(), RESTLETTE_PATH, id);
    let resp = request(&client, reqwest::Method::DELETE, &url, &caller)
        .send()
        .await
        .expect("restlette delete");
    world.authz_status = Some(resp.status().as_u16());
}

// ---- When: reads through the graphlette (and the REST collection) ----

#[when(regex = r#"^"([^"]+)" queries widgets of kind "([^"]+)"$"#)]
async fn query_by_kind(world: &mut CertWorld, caller: String, kind: String) {
    let query = format!("{{ getByKind(kind: \"{kind}\") {{ id name kind }} }}");
    let resp = graphql(world, &caller, &query).await;
    assert_no_gql_errors(&resp);
    world.authz_names = names_of(&resp["data"]["getByKind"]);
}

#[when(regex = r#"^"([^"]+)" queries widgets of kind "([^"]+)" as of "([^"]+)"$"#)]
async fn query_by_kind_at(world: &mut CertWorld, caller: String, kind: String, stamp: String) {
    let at = world.authz_stamp(&stamp);
    let query = format!("{{ getByKind(kind: \"{kind}\", at: {at}) {{ id name kind }} }}");
    let resp = graphql(world, &caller, &query).await;
    assert_no_gql_errors(&resp);
    world.authz_names = names_of(&resp["data"]["getByKind"]);
}

#[when(regex = r#"^"([^"]+)" lists all widgets$"#)]
async fn list_widgets(world: &mut CertWorld, caller: String) {
    let client = reqwest::Client::new();
    let url = format!("{}{}", world.server_addr(), RESTLETTE_PATH);
    let body: Value = request(&client, reqwest::Method::GET, &url, &caller)
        .send()
        .await
        .expect("restlette list")
        .json()
        .await
        .expect("list response is JSON");
    world.authz_names = names_of(&body);
}

#[when(regex = r#"^"([^"]+)" reads widget "([^"]+)" by id as of "([^"]+)"$"#)]
async fn read_at(world: &mut CertWorld, caller: String, name: String, stamp: String) {
    let id = widget_id(world, &name);
    let at = world.authz_stamp(&stamp);
    let query = format!("{{ getById(id: \"{id}\", at: {at}) {{ id name kind }} }}");
    let resp = graphql(world, &caller, &query).await;
    assert_no_gql_errors(&resp);
    world.authz_response = Some(resp);
}

#[when(regex = r#"^the current instant is captured as "([^"]+)"$"#)]
async fn capture_instant(world: &mut CertWorld, key: String) {
    // Timestamps are millisecond-precision, so separate the captured instant
    // from the writes on either side of it — otherwise "as of before beta"
    // could land in the same millisecond as beta's creation.
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    world
        .authz_stamps
        .insert(key, chrono::Utc::now().timestamp_millis());
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
}

// ---- Then ----

#[then(regex = r#"^"([^"]+)" can read widget "([^"]+)" by id$"#)]
async fn can_read(world: &mut CertWorld, caller: String, name: String) {
    let id = widget_id(world, &name);

    // Through the graphlette.
    let query = format!("{{ getById(id: \"{id}\") {{ id name kind }} }}");
    let resp = graphql(world, &caller, &query).await;
    assert_no_gql_errors(&resp);
    let found = &resp["data"]["getById"];
    assert!(
        !found.is_null(),
        "'{caller}' should see widget '{name}' through the graphlette but got null: {resp}"
    );
    assert_eq!(
        found.get("name").and_then(|v| v.as_str()),
        Some(name.as_str()),
        "graphlette returned the wrong widget: {resp}"
    );

    // And through the restlette, which reads by a different code path.
    let client = reqwest::Client::new();
    let url = format!("{}{}/{}", world.server_addr(), RESTLETTE_PATH, id);
    let rest = request(&client, reqwest::Method::GET, &url, &caller)
        .send()
        .await
        .expect("restlette read");
    assert_eq!(
        rest.status().as_u16(),
        200,
        "'{caller}' should see widget '{name}' through the restlette"
    );
}

#[then(regex = r#"^"([^"]+)" cannot read widget "([^"]+)" by id$"#)]
async fn cannot_read(world: &mut CertWorld, caller: String, name: String) {
    let id = widget_id(world, &name);

    let query = format!("{{ getById(id: \"{id}\") {{ id name kind }} }}");
    let resp = graphql(world, &caller, &query).await;
    assert_no_gql_errors(&resp);
    assert!(
        resp["data"]["getById"].is_null(),
        "'{caller}' must not see widget '{name}' through the graphlette: {resp}"
    );

    let client = reqwest::Client::new();
    let url = format!("{}{}/{}", world.server_addr(), RESTLETTE_PATH, id);
    let rest = request(&client, reqwest::Method::GET, &url, &caller)
        .send()
        .await
        .expect("restlette read");
    assert_eq!(
        rest.status().as_u16(),
        404,
        "'{caller}' must not see widget '{name}' through the restlette"
    );
}

#[then(regex = r#"^the result should be exactly "([^"]*)"$"#)]
async fn result_should_be(world: &mut CertWorld, expected: String) {
    let mut want: Vec<String> = expected
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    want.sort();
    let mut got = world.authz_names.clone();
    got.sort();
    assert_eq!(got, want, "result set mismatch");
}

#[then(
    regex = r#"^the stored envelope for widget "([^"]+)" should carry the tokens "([^"]+)" resolves to$"#
)]
async fn stored_envelope_carries_tokens(world: &mut CertWorld, name: String, caller: String) {
    let id = widget_id(world, &name);
    let env = world
        .repo()
        .read(&id, &inspector_tokens(), None)
        .await
        .expect("storage read")
        .unwrap_or_else(|| panic!("widget '{name}' is not in storage at all"));

    let expected = authz::tokens_for(&caller);
    assert!(
        !expected.is_empty(),
        "'{caller}' resolves to no tokens — this step is meaningless for that caller"
    );
    assert!(
        !env.authorized_tokens.is_empty(),
        "the stored envelope for '{name}' carries NO authorized_tokens: the write \
         path dropped the tokens the Auth resolved, so the record is public to \
         everyone"
    );
    assert_eq!(
        env.authorized_tokens, expected,
        "the stored envelope for '{name}' does not carry the tokens '{caller}' resolves to"
    );
}

#[then(regex = r#"^the stored envelope for widget "([^"]+)" should carry no tokens$"#)]
async fn stored_envelope_carries_no_tokens(world: &mut CertWorld, name: String) {
    let id = widget_id(world, &name);
    let env = world
        .repo()
        .read(&id, &inspector_tokens(), None)
        .await
        .expect("storage read")
        .unwrap_or_else(|| panic!("widget '{name}' is not in storage at all"));
    assert!(
        env.authorized_tokens.is_empty(),
        "a record written without credentials must stay public, but it was \
         stamped with {:?}",
        env.authorized_tokens
    );
}

#[then("the delete should be refused")]
async fn delete_refused(world: &mut CertWorld) {
    let status = world.authz_status.expect("no delete was attempted");
    assert!(
        status == 404 || status == 403,
        "a caller deleting someone else's record must be refused, got HTTP {status}"
    );
}

#[then("the delete should succeed")]
async fn delete_succeeded(world: &mut CertWorld) {
    let status = world.authz_status.expect("no delete was attempted");
    assert_eq!(status, 200, "the owner's delete should succeed");
}

#[then("the temporal read should find it")]
async fn temporal_found(world: &mut CertWorld) {
    let resp = world.authz_response.as_ref().expect("no temporal read");
    assert!(
        !resp["data"]["getById"].is_null(),
        "expected the record to exist at that instant: {resp}"
    );
}

#[then("the temporal read should find nothing")]
async fn temporal_not_found(world: &mut CertWorld) {
    let resp = world.authz_response.as_ref().expect("no temporal read");
    assert!(
        resp["data"]["getById"].is_null(),
        "`at:` rewinds data, not authorization — this caller reached a record \
         it is not entitled to by asking for an earlier instant: {resp}"
    );
}
