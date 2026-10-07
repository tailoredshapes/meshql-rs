//! Steps for `schema.feature`: a restlette enforces its JSON Schema.
//!
//! The scenario hands over the schema, and it goes in as the widget
//! restlette's configuration, so the certified server validates exactly as a
//! deployed one does. The `tries to` writes record the status and assert
//! nothing: a refusal is the outcome these scenarios are looking for.

use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use serde_json::{json, Value};

use crate::authz::{self, IDENTITY_HEADER, RESTLETTE_PATH};
use crate::world::CertWorld;

fn request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    caller: &str,
) -> reqwest::RequestBuilder {
    let req = client.request(method, url);
    match authz::identity_of(caller) {
        Some(id) => req.header(IDENTITY_HEADER, id),
        None => req,
    }
}

fn widget_id(world: &CertWorld, name: &str) -> String {
    world
        .authz_ids
        .get(name)
        .unwrap_or_else(|| panic!("no widget named '{name}' was created"))
        .clone()
}

/// Send `body` to the restlette and record the status; nothing is asserted.
async fn attempt(
    world: &mut CertWorld,
    method: reqwest::Method,
    path: String,
    caller: &str,
    body: Value,
) {
    let client = reqwest::Client::new();
    let url = format!("{}{}", world.server_addr(), path);
    let resp = request(&client, method, &url, caller)
        .json(&body)
        .send()
        .await
        .expect("restlette write");
    world.authz_status = Some(resp.status().as_u16());
}

// ---- Given ----

/// Stand the certified server up with the scenario's schema on its widget
/// restlette, behind the same edge auth as `an authorizing MeshQL server`.
#[given("an authorizing MeshQL server whose widgets must match the JSON Schema:")]
async fn server_with_schema(world: &mut CertWorld, step: &Step) {
    assert!(
        world.has_repo(),
        "the backing repository must be set by the test runner's before-hook"
    );
    let text = step
        .docstring
        .as_ref()
        .expect("this step needs the schema as a docstring");
    let schema: Value = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("the scenario's schema is not JSON: {e}"));
    let addr = authz::start_server_with_schema(
        world.repo_arc(),
        world.searcher_arc(),
        authz::edge_auth(),
        schema,
    )
    .await;
    world.server_addr = Some(addr);
}

// ---- When ----

#[when(regex = r#"^"([^"]+)" tries to create a widget "([^"]+)" of kind "([^"]+)"$"#)]
async fn try_create(world: &mut CertWorld, caller: String, name: String, kind: String) {
    attempt(
        world,
        reqwest::Method::POST,
        RESTLETTE_PATH.to_string(),
        &caller,
        json!({ "name": name, "kind": kind }),
    )
    .await;
}

#[when(regex = r#"^"([^"]+)" tries to create a widget "([^"]+)" with no kind$"#)]
async fn try_create_without_kind(world: &mut CertWorld, caller: String, name: String) {
    attempt(
        world,
        reqwest::Method::POST,
        RESTLETTE_PATH.to_string(),
        &caller,
        json!({ "name": name }),
    )
    .await;
}

#[when(regex = r#"^"([^"]+)" tries to update widget "([^"]+)" to kind "([^"]+)"$"#)]
async fn try_update(world: &mut CertWorld, caller: String, name: String, kind: String) {
    let path = format!("{}/{}", RESTLETTE_PATH, widget_id(world, &name));
    attempt(
        world,
        reqwest::Method::PUT,
        path,
        &caller,
        json!({ "name": name, "kind": kind }),
    )
    .await;
}

// ---- Then ----

#[then("the write should be refused as invalid")]
async fn refused_as_invalid(world: &mut CertWorld) {
    assert_eq!(
        world.authz_status,
        Some(400),
        "the write was not refused with 400"
    );
}
