//! Steps for the document-versions certification.
//!
//! Every step goes over the wire, against the same widget restlette the
//! authorization certification uses, so what is certified is the REST surface a
//! caller actually gets rather than the trait underneath it.

use cucumber::{then, when};
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

/// `GET /<entity>/api/<id>/versions` as `caller`.
async fn fetch_versions(world: &CertWorld, name: &str, caller: &str) -> Vec<Value> {
    let client = reqwest::Client::new();
    let id = widget_id(world, name);
    let url = format!("{}{}/{}/versions", world.server_addr(), RESTLETTE_PATH, id);
    let resp = request(&client, reqwest::Method::GET, &url, caller)
        .send()
        .await
        .expect("version listing request");
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.expect("version listing is JSON");
    assert_eq!(status, 200, "version listing for '{name}' failed: {body}");
    assert_eq!(
        body.get("id").and_then(|v| v.as_str()),
        Some(id.as_str()),
        "a version listing names the document it belongs to: {body}"
    );
    body.get("versions")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("version listing carried no `versions` array: {body}"))
        .clone()
}

/// Fetch a URL taken from a version listing. Returns the status and body, so a
/// step can assert a refusal as readily as a payload.
async fn fetch_url(world: &CertWorld, url: &str, caller: &str) -> (u16, Value) {
    let client = reqwest::Client::new();
    let full = format!("{}{}", world.server_addr(), url);
    let resp = request(&client, reqwest::Method::GET, &full, caller)
        .send()
        .await
        .expect("version request");
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

fn nth_entry(world: &CertWorld, n: usize) -> &Value {
    world.version_list.get(n - 1).unwrap_or_else(|| {
        panic!(
            "no version {n}; the list holds {}",
            world.version_list.len()
        )
    })
}

// ---- When ----

#[when(regex = r#"^"([^"]+)" updates widget "([^"]+)" to kind "([^"]+)"$"#)]
async fn update_widget(world: &mut CertWorld, caller: String, name: String, kind: String) {
    let client = reqwest::Client::new();
    let id = widget_id(world, &name);
    let url = format!("{}{}/{}", world.server_addr(), RESTLETTE_PATH, id);
    let resp = request(&client, reqwest::Method::PUT, &url, &caller)
        .json(&json!({ "name": name, "kind": kind }))
        .send()
        .await
        .expect("restlette update");
    let status = resp.status().as_u16();
    assert!(
        (200..300).contains(&status),
        "update of '{name}' failed with {status}"
    );
    world.authz_status = Some(status);
}

// ---- Then ----

#[then(
    regex = r#"^the version list for widget "([^"]+)" seen by "([^"]+)" should have (\d+) entr(?:y|ies)$"#
)]
async fn version_list_has(world: &mut CertWorld, name: String, caller: String, expected: usize) {
    let list = fetch_versions(world, &name, &caller).await;
    assert_eq!(
        list.len(),
        expected,
        "version list for '{name}' seen by '{caller}': {list:?}"
    );
    world.version_list = list;
}

#[then("that version list should be ordered oldest first")]
async fn ordered_oldest_first(world: &mut CertWorld) {
    let stamps: Vec<&str> = world
        .version_list
        .iter()
        .map(|e| {
            e.get("created_at")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("version entry carried no created_at: {e}"))
        })
        .collect();
    let mut sorted = stamps.clone();
    sorted.sort();
    assert_eq!(
        stamps, sorted,
        "version list is not oldest first: {stamps:?}"
    );
}

#[then("every entry in that version list should carry a URL")]
async fn every_entry_has_url(world: &mut CertWorld) {
    for entry in &world.version_list {
        assert!(
            entry.get("url").and_then(|v| v.as_str()).is_some(),
            "entry carried no url: {entry}"
        );
    }
}

#[then("no entry in that version list should carry a URL")]
async fn no_entry_has_url(world: &mut CertWorld) {
    for entry in &world.version_list {
        assert!(
            entry.get("url").is_none(),
            "entry carried a url it should not have: {entry}"
        );
        assert_eq!(
            entry.get("unauthorized"),
            Some(&json!(true)),
            "an unreadable version must say so: {entry}"
        );
        assert!(
            entry.get("created_at").is_some(),
            "an unreadable version still carries when it happened: {entry}"
        );
    }
}

#[then("the last entry in that version list should be marked deleted")]
async fn last_entry_deleted(world: &mut CertWorld) {
    let last = world
        .version_list
        .last()
        .expect("version list is empty")
        .clone();
    assert_eq!(
        last.get("deleted"),
        Some(&json!(true)),
        "the last version should be the deletion: {last}"
    );
}

#[then(
    regex = r#"^reading version (\d+) of widget "([^"]+)" as "([^"]+)" should return kind "([^"]+)"$"#
)]
async fn read_version_returns_kind(
    world: &mut CertWorld,
    n: usize,
    name: String,
    caller: String,
    kind: String,
) {
    if world.version_list.is_empty() {
        world.version_list = fetch_versions(world, &name, &caller).await;
    }
    let url = nth_entry(world, n)
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("version {n} carried no url"))
        .to_string();

    let (status, body) = fetch_url(world, &url, &caller).await;
    assert_eq!(status, 200, "reading version {n} of '{name}': {body}");
    assert_eq!(
        body.get("kind").and_then(|v| v.as_str()),
        Some(kind.as_str()),
        "version {n} of '{name}' should be the version as it was then: {body}"
    );
}

#[then(
    regex = r#"^listing the versions of widget "([^"]+)" as "([^"]+)" twice should give the same URLs$"#
)]
async fn listing_twice_is_stable(world: &mut CertWorld, name: String, caller: String) {
    let first = fetch_versions(world, &name, &caller).await;
    let second = fetch_versions(world, &name, &caller).await;
    let urls = |list: &[Value]| -> Vec<String> {
        list.iter()
            .map(|e| {
                e.get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    };
    assert_eq!(
        urls(&first),
        urls(&second),
        "a version's address changed between two listings"
    );
    world.version_list = first;
}

#[then(
    regex = r#"^reading version token "([^"]+)" of widget "([^"]+)" as "([^"]+)" should not be found$"#
)]
async fn unknown_token_is_not_found(
    world: &mut CertWorld,
    token: String,
    name: String,
    caller: String,
) {
    let id = widget_id(world, &name);
    let url = format!("{RESTLETTE_PATH}/{id}/versions/{token}");
    let (status, body) = fetch_url(world, &url, &caller).await;
    assert_eq!(status, 404, "an unknown version token must 404: {body}");
}

#[then(regex = r#"^reading version (\d+) of widget "([^"]+)" as "([^"]+)" should be refused$"#)]
async fn read_version_refused(world: &mut CertWorld, n: usize, name: String, caller: String) {
    // The URL is discovered as a caller who *can* see it, then presented by one
    // who cannot — otherwise the listing would hand out no URL to try.
    let list = fetch_versions(world, &name, "alice").await;
    let url = list
        .get(n - 1)
        .and_then(|e| e.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("version {n} carried no url"))
        .to_string();

    let (status, body) = fetch_url(world, &url, &caller).await;
    assert!(
        status == 403 || status == 404,
        "'{caller}' must not read version {n} of '{name}', got {status}: {body}"
    );
}
