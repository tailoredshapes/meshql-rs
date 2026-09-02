//! Steps for the shared `Repository` certification.
//!
//! `tests/features/repository.feature` is byte-identical across meshql-rs,
//! meshobj (TypeScript), and meshql (Java), so these bindings track the
//! TypeScript ones in `meshobj/core/cert/src/steps/repository.steps.ts`. When
//! the wording of a step leaves room to interpret, the TypeScript reading wins.
//!
//! Data tables are hash tables: the first row names the payload fields and each
//! later row is one envelope. A column called `count` is a payload field, not a
//! repetition count, so three rows create three envelopes.

use chrono::Utc;
use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use meshql_core::{Envelope, Stash};
use serde_json::json;

use crate::world::CertWorld;

/// Read a Gherkin data table as a list of payloads, one per body row.
///
/// Every cell stays a JSON string, which is what the TypeScript bindings do
/// with `DataTable.hashes()`. A `count` column therefore reaches the store as
/// `"3"`, not `3`.
fn payloads(step: &Step) -> Vec<Stash> {
    let table = step.table.as_ref().expect("this step needs a data table");
    let headers: Vec<String> = table
        .rows
        .first()
        .expect("the data table needs a header row")
        .iter()
        .map(|h| h.trim().to_string())
        .collect();

    table
        .rows
        .iter()
        .skip(1)
        .map(|row| {
            let mut payload = Stash::new();
            for (column, header) in headers.iter().enumerate() {
                let cell = row.get(column).map(|c| c.trim()).unwrap_or("");
                payload.insert(header.clone(), json!(cell));
            }
            payload
        })
        .collect()
}

/// The `name` field of a payload, which is the key the world files envelopes
/// under so later steps can name them.
fn name_of(payload: &Stash) -> String {
    payload
        .get("name")
        .and_then(|n| n.as_str())
        .expect("every created envelope needs a 'name' column")
        .to_string()
}

/// Parse the `["a", "b"]` list that the read and remove steps carry, then
/// resolve each name to the id the store handed back.
fn ids_for(world: &CertWorld, names_json: &str) -> Vec<String> {
    let names: Vec<String> =
        serde_json::from_str(names_json).expect("expected a JSON array of envelope names");
    names
        .iter()
        .map(|name| {
            world
                .envelopes_by_name
                .get(name)
                .unwrap_or_else(|| panic!("envelope \"{name}\" was never created"))
                .id
                .clone()
        })
        .collect()
}

fn new_id() -> String {
    format!("env-{}", uuid::Uuid::new_v4().simple())
}

#[given("a fresh repository instance")]
async fn fresh_repo(world: &mut CertWorld) {
    // The test runner's before-hook injects the repository. This step records
    // the instant the scenario started, which the created_at assertion uses as
    // its floor.
    world.test_start = Utc::now();
}

// ---- Given / When: writing ----

#[given(regex = r"^I have created envelopes:$")]
#[given(regex = r"^I create envelopes:$")]
#[when(regex = r"^I have created envelopes:$")]
#[when(regex = r"^I create envelopes:$")]
async fn create_envelopes(world: &mut CertWorld, step: &Step) {
    for payload in payloads(step) {
        let name = name_of(&payload);
        let envelope = Envelope::new(new_id(), payload, CertWorld::star());
        let stored = world
            .repo()
            .create(envelope, &CertWorld::star_session())
            .await
            .expect("create failed");
        world.envelopes_by_name.insert(name, stored);
    }
}

#[given(regex = r"^I create many envelopes:$")]
#[when(regex = r"^I create many envelopes:$")]
async fn create_many_envelopes(world: &mut CertWorld, step: &Step) {
    let envelopes: Vec<Envelope> = payloads(step)
        .into_iter()
        .map(|payload| Envelope::new(new_id(), payload, CertWorld::star()))
        .collect();

    let stored = world
        .repo()
        .create_many(envelopes, &CertWorld::star_session())
        .await
        .expect("create_many failed");

    for envelope in stored {
        world
            .envelopes_by_name
            .insert(name_of(&envelope.payload), envelope);
    }
}

/// Append a version to an existing document. The new version reuses the id and
/// the authorized tokens of the version before it, and takes its whole payload
/// from the table row, so the `name` field drops away unless the row names it.
#[given(regex = r#"^I create a new version of envelope "([^"]+)":$"#)]
#[when(regex = r#"^I create a new version of envelope "([^"]+)":$"#)]
async fn create_new_version(world: &mut CertWorld, name: String, step: &Step) {
    let previous = world
        .envelopes_by_name
        .get(&name)
        .unwrap_or_else(|| panic!("envelope \"{name}\" was never created"));
    let id = previous.id.clone();
    let tokens = previous.auth.clone();

    let payload = payloads(step)
        .into_iter()
        .next()
        .expect("the data table needs one body row");

    let envelope = Envelope::new(id, payload, tokens);
    let stored = world
        .repo()
        .create(envelope, &CertWorld::star_session())
        .await
        .expect("create failed");
    world.envelopes_by_name.insert(name, stored);
}

// `I capture the current timestamp as "..."` and `I wait N milliseconds` live
// in `super::common`, which this feature shares with `farm.feature`.

// ---- Given / When: reading and removing ----

/// One name reads through `read`; more than one reads through `read_many`.
/// The single-id path also records the envelope on its own, because the payload
/// assertion prefers it.
#[given(regex = r"^I read envelopes (\[.*\]) by their IDs$")]
#[when(regex = r"^I read envelopes (\[.*\]) by their IDs$")]
async fn read_envelopes_by_id(world: &mut CertWorld, names_json: String) {
    let ids = ids_for(world, &names_json);

    if ids.len() == 1 {
        let found = world
            .repo()
            .read(&ids[0], &CertWorld::star_session(), None)
            .await
            .expect("read failed");
        world.last_envelopes = found.iter().cloned().collect();
        world.single_result = found;
    } else {
        let found = world
            .repo()
            .read_many(&ids, &CertWorld::star_session())
            .await
            .expect("read_many failed");
        world.last_envelopes = found;
        world.single_result = None;
    }
}

#[given(regex = r#"^I remove envelope "([^"]+)"$"#)]
#[when(regex = r#"^I remove envelope "([^"]+)"$"#)]
async fn remove_envelope(world: &mut CertWorld, name: String) {
    let id = world
        .envelopes_by_name
        .get(&name)
        .unwrap_or_else(|| panic!("envelope \"{name}\" was never created"))
        .id
        .clone();
    world.last_remove = world
        .repo()
        .remove(&id, &CertWorld::star_session())
        .await
        .expect("remove failed");
}

#[given(regex = r"^I remove envelopes (\[.*\]) by their IDs$")]
#[when(regex = r"^I remove envelopes (\[.*\]) by their IDs$")]
async fn remove_envelopes_by_id(world: &mut CertWorld, names_json: String) {
    let ids = ids_for(world, &names_json);
    world.remove_results = world
        .repo()
        .remove_many(&ids, &CertWorld::star_session())
        .await
        .expect("remove_many failed");
}

#[given("I list all envelopes")]
#[when("I list all envelopes")]
async fn list_all_envelopes(world: &mut CertWorld) {
    world.last_envelopes = world
        .repo()
        .list(&CertWorld::star_session())
        .await
        .expect("list failed");
    world.single_result = None;
}

// ---- Then: assertions ----

#[then("the envelopes should have generated IDs")]
async fn assert_generated_ids(world: &mut CertWorld) {
    for envelope in world.envelopes_by_name.values() {
        assert!(
            !envelope.id.is_empty(),
            "the store returned an envelope without an id"
        );
    }
}

#[then("the envelopes created_at should be greater than or equal to the test start time")]
async fn assert_created_at_after_start(world: &mut CertWorld) {
    // Compare in milliseconds: several stores round created_at to millisecond
    // precision, so a sub-millisecond floor would fail for reasons that have
    // nothing to do with the contract.
    let floor = world.test_start.timestamp_millis();
    for envelope in world.envelopes_by_name.values() {
        assert!(
            envelope.created_at.timestamp_millis() >= floor,
            "created_at {} predates the test start {floor}",
            envelope.created_at.timestamp_millis()
        );
    }
}

#[then("the envelopes deleted flag should be disabled")]
async fn assert_not_deleted(world: &mut CertWorld) {
    for envelope in world.envelopes_by_name.values() {
        assert!(
            !envelope.deleted,
            "envelope \"{}\" came back marked deleted",
            envelope.id
        );
    }
}

#[then(regex = r"^I should receive (\d+) envelopes?$")]
#[then(regex = r"^I should receive exactly (\d+) envelopes?$")]
async fn assert_received_count(world: &mut CertWorld, expected: usize) {
    assert_eq!(
        world.last_envelopes.len(),
        expected,
        "expected {expected} envelopes, got {}",
        world.last_envelopes.len()
    );
}

#[then(regex = r#"^the payload "([^"]+)" should be "([^"]*)"$"#)]
async fn assert_payload_field(world: &mut CertWorld, key: String, expected: String) {
    let envelope = world
        .single_result
        .as_ref()
        .or_else(|| world.last_envelopes.first())
        .expect("no envelope to assert against");
    let actual = envelope
        .payload
        .get(&key)
        .unwrap_or_else(|| panic!("payload has no field \"{key}\""));
    assert_eq!(actual, &json!(expected));
}

#[then("the remove operation should return true")]
async fn assert_remove_true(world: &mut CertWorld) {
    assert!(world.last_remove, "remove returned false");
}

#[then("the remove operations should return true")]
async fn assert_removes_true(world: &mut CertWorld) {
    assert!(
        !world.remove_results.is_empty(),
        "no remove results were recorded"
    );
    for (id, removed) in &world.remove_results {
        assert!(removed, "remove of \"{id}\" returned false");
    }
}

#[then(regex = r"^reading envelopes (\[.*\]) by their IDs should return nothing$")]
async fn assert_read_returns_nothing(world: &mut CertWorld, names_json: String) {
    let ids = ids_for(world, &names_json);

    if ids.len() == 1 {
        let found = world
            .repo()
            .read(&ids[0], &CertWorld::star_session(), None)
            .await
            .expect("read failed");
        assert!(found.is_none(), "expected nothing, got {found:?}");
    } else {
        let found = world
            .repo()
            .read_many(&ids, &CertWorld::star_session())
            .await
            .expect("read_many failed");
        assert!(
            found.is_empty(),
            "expected nothing, got {} rows",
            found.len()
        );
    }
}

#[then(
    regex = r#"^reading envelope "([^"]+)" at timestamp "([^"]+)" should return version "([^"]+)"$"#
)]
async fn assert_version_at_timestamp(
    world: &mut CertWorld,
    name: String,
    label: String,
    expected: String,
) {
    let id = world
        .envelopes_by_name
        .get(&name)
        .unwrap_or_else(|| panic!("envelope \"{name}\" was never created"))
        .id
        .clone();
    let at = *world
        .timestamps
        .get(&label)
        .unwrap_or_else(|| panic!("no timestamp was captured as \"{label}\""));

    let found = world
        .repo()
        .read(&id, &CertWorld::star_session(), Some(at))
        .await
        .expect("read failed")
        .unwrap_or_else(|| panic!("reading \"{name}\" at \"{label}\" returned nothing"));

    assert_eq!(
        found.payload.get("version"),
        Some(&json!(expected)),
        "wrong version at \"{label}\""
    );
}

#[then(regex = r"^listing all envelopes should show exactly (\d+) envelopes?$")]
async fn assert_list_count(world: &mut CertWorld, expected: usize) {
    let listed = world
        .repo()
        .list(&CertWorld::star_session())
        .await
        .expect("list failed");
    assert_eq!(
        listed.len(),
        expected,
        "expected {expected} envelopes in the list, got {}",
        listed.len()
    );
}
