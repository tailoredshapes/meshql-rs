//! Steps for the shared `Searcher` certification.
//!
//! `tests/features/searcher.feature` is byte-identical across meshql-rs,
//! meshobj (TypeScript), and meshql (Java), so these bindings track the
//! TypeScript ones in `meshobj/core/cert/src/steps/searcher.steps.ts`. When the
//! wording of a step leaves room to interpret, the TypeScript reading wins.
//!
//! Two table shapes appear here and they behave differently. The dataset table
//! seeds envelopes, and its cells become payload values, so a numeric-looking
//! cell becomes a JSON number — `count` is 1, not "1", which is what the
//! `count 1` assertions compare against. The parameter tables feed a Handlebars
//! template, and their cells stay strings.

use chrono::Utc;
use cucumber::gherkin::Step;
use cucumber::{given, then, when};
use meshql_core::{Envelope, Stash};
use serde_json::{json, Value};

use crate::world::CertWorld;

/// Read a Gherkin data table as a list of rows keyed by column name.
///
/// This is what `DataTable.hashes()` gives the TypeScript bindings: the first
/// row names the columns and every later row is one record.
fn rows(step: &Step) -> Vec<Vec<(String, String)>> {
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
            headers
                .iter()
                .enumerate()
                .map(|(column, header)| {
                    let cell = row.get(column).map(|c| c.trim()).unwrap_or("");
                    (header.clone(), cell.to_string())
                })
                .collect()
        })
        .collect()
}

/// Coerce a dataset cell the way the TypeScript bindings do: a cell that reads
/// as a number becomes a JSON number, everything else stays a string.
fn coerce(cell: &str) -> Value {
    if cell.is_empty() {
        return json!(cell);
    }
    if let Ok(n) = cell.parse::<i64>() {
        return json!(n);
    }
    if let Ok(n) = cell.parse::<f64>() {
        return json!(n);
    }
    json!(cell)
}

/// The single parameter row a search step carries, as template arguments.
fn params(step: &Step) -> Stash {
    let row = rows(step)
        .into_iter()
        .next()
        .expect("a parameter table needs one body row");
    let mut args = Stash::new();
    for (key, value) in row {
        args.insert(key, json!(value));
    }
    args
}

/// Resolve a `findById` parameter from an envelope name to the id the store
/// handed back. Only `findById` searches by id, so only it substitutes.
fn resolve_id(world: &CertWorld, template_name: &str, args: &mut Stash) {
    if template_name != "findById" {
        return;
    }
    let named = args.get("id").and_then(|v| v.as_str()).map(str::to_string);
    if let Some(name) = named {
        if let Some(envelope) = world.envelopes_by_name.get(&name) {
            args.insert("id".to_string(), json!(envelope.id.clone()));
        }
    }
}

fn template_for(world: &CertWorld, name: &str) -> String {
    world
        .templates
        .get(name)
        .cloned()
        .unwrap_or_else(|| panic!("template \"{name}\" not found"))
}

fn new_id() -> String {
    format!("env-{}", uuid::Uuid::new_v4().simple())
}

/// The record the singleton assertions read, which the search must have found.
fn found(world: &CertWorld) -> &Stash {
    world
        .last_search_result
        .as_ref()
        .expect("no search has run yet")
        .as_ref()
        .expect("the search found nothing")
}

// ---- Given: the dataset ----

#[given("a fresh repository and searcher instance")]
async fn fresh_repo_and_searcher(world: &mut CertWorld) {
    // The runner's before-hook injects both. This step records the instant the
    // scenario started, for the benefit of any timing assertion.
    world.test_start = Utc::now();
}

#[given(regex = r"^I have created and saved the following test dataset:$")]
async fn create_dataset(world: &mut CertWorld, step: &Step) {
    let envelopes: Vec<Envelope> = rows(step)
        .into_iter()
        .map(|row| {
            let mut payload = Stash::new();
            for (key, value) in row {
                payload.insert(key, coerce(&value));
            }
            Envelope::new(new_id(), payload, CertWorld::star())
        })
        .collect();

    let saved = world
        .repo()
        .create_many(envelopes, &CertWorld::star_session())
        .await
        .expect("create_many failed");

    for envelope in saved {
        let name = envelope
            .payload
            .get("name")
            .and_then(|n| n.as_str())
            .expect("every dataset row needs a 'name' column")
            .to_string();
        world.envelopes_by_name.insert(name, envelope);
    }
}

#[given(regex = r#"^I have removed envelope "([^"]+)"$"#)]
async fn remove_named_envelope(world: &mut CertWorld, name: String) {
    let id = world
        .envelopes_by_name
        .get(&name)
        .unwrap_or_else(|| panic!("envelope \"{name}\" was never created"))
        .id
        .clone();
    world
        .repo()
        .remove(&id, &CertWorld::star_session())
        .await
        .expect("remove failed");
}

/// Append a version that renames the document and resets its count. The new
/// version keeps the id and the authorized tokens of the version before it, and
/// carries the rest of the old payload forward, so `type` survives the rename.
#[given(regex = r#"^I have updated envelope "([^"]+)" to "([^"]+)" with count (-?\d+)$"#)]
async fn update_named_envelope(
    world: &mut CertWorld,
    old_name: String,
    new_name: String,
    count: i64,
) {
    let previous = world
        .envelopes_by_name
        .get(&old_name)
        .unwrap_or_else(|| panic!("envelope \"{old_name}\" was never created"))
        .clone();

    let mut payload = previous.payload.clone();
    payload.insert("name".to_string(), json!(new_name));
    payload.insert("count".to_string(), json!(count));

    let envelope = Envelope::new(previous.id.clone(), payload, previous.auth.clone());
    let updated = world
        .repo()
        .create(envelope, &CertWorld::star_session())
        .await
        .expect("create failed");

    world.envelopes_by_name.remove(&old_name);
    world.envelopes_by_name.insert(new_name, updated);
}

// ---- When: searching ----

#[when(regex = r#"^I search using template "([^"]+)" with parameters:$"#)]
async fn search_one(world: &mut CertWorld, template_name: String, step: &Step) {
    let template = template_for(world, &template_name);
    let mut args = params(step);
    resolve_id(world, &template_name, &mut args);

    let result = world
        .searcher()
        .find(
            &template,
            &args,
            &CertWorld::star_session(),
            Utc::now().timestamp_millis(),
        )
        .await
        .expect("find failed");
    world.last_search_result = Some(result);
}

#[when(regex = r#"^I search all using template "([^"]+)" with parameters:$"#)]
async fn search_all(world: &mut CertWorld, template_name: String, step: &Step) {
    let template = template_for(world, &template_name);
    let mut args = params(step);
    resolve_id(world, &template_name, &mut args);

    world.search_results = world
        .searcher()
        .find_all(
            &template,
            &args,
            &CertWorld::star_session(),
            Utc::now().timestamp_millis(),
        )
        .await
        .expect("find_all failed");
}

/// A limit is not a parameter of `find_all`; it rides in the same `args` stash
/// as the template parameters, under the reserved key `limit`.
#[when(regex = r#"^I search all using template "([^"]+)" with a limit of (\d+) and parameters:$"#)]
async fn search_all_with_limit(
    world: &mut CertWorld,
    template_name: String,
    limit: i64,
    step: &Step,
) {
    let template = template_for(world, &template_name);
    let mut args = params(step);
    resolve_id(world, &template_name, &mut args);
    args.insert("limit".to_string(), json!(limit));

    world.search_results = world
        .searcher()
        .find_all(
            &template,
            &args,
            &CertWorld::star_session(),
            Utc::now().timestamp_millis(),
        )
        .await
        .expect("find_all failed");
}

// ---- Then: the singleton result ----

/// TypeScript compares the result against `{}`, which is what its searchers
/// return when nothing matches. Rust's `find` returns `None` instead, so both
/// readings of "empty" count.
#[then("the search result should be empty")]
async fn assert_result_empty(world: &mut CertWorld) {
    let result = world
        .last_search_result
        .as_ref()
        .expect("no search has run yet");
    match result {
        None => {}
        Some(stash) => assert!(
            stash.is_empty(),
            "expected an empty search result, got {stash:?}"
        ),
    }
}

#[then(regex = r#"^the search result should have name "([^"]+)"$"#)]
async fn assert_result_name(world: &mut CertWorld, expected: String) {
    assert_eq!(
        found(world).get("name"),
        Some(&json!(expected)),
        "wrong name on the search result"
    );
}

#[then(regex = r"^the search result should have count (-?\d+)$")]
async fn assert_result_count(world: &mut CertWorld, expected: i64) {
    assert_eq!(
        found(world).get("count"),
        Some(&json!(expected)),
        "wrong count on the search result"
    );
}

// ---- Then: the result set ----

#[then(regex = r"^I should receive exactly (\d+) results?$")]
async fn assert_result_count_exact(world: &mut CertWorld, expected: usize) {
    assert_eq!(
        world.search_results.len(),
        expected,
        "expected {expected} results, got {:?}",
        world.search_results
    );
}

#[then(regex = r#"^the results should include an envelope with name "([^"]+)"$"#)]
async fn assert_results_include_name(world: &mut CertWorld, name: String) {
    let hit = world
        .search_results
        .iter()
        .any(|r| r.get("name") == Some(&json!(name)));
    assert!(
        hit,
        "no result named \"{name}\" in {:?}",
        world.search_results
    );
}

#[then(regex = r#"^the results should include an envelope with name "([^"]+)" and count (-?\d+)$"#)]
async fn assert_results_include_name_and_count(world: &mut CertWorld, name: String, count: i64) {
    let hit = world
        .search_results
        .iter()
        .any(|r| r.get("name") == Some(&json!(name)) && r.get("count") == Some(&json!(count)));
    assert!(
        hit,
        "no result named \"{name}\" with count {count} in {:?}",
        world.search_results
    );
}

// ---- Then: the opt-in `createdAt` as-of field ----
//
// A result is the resolved envelope's payload with two envelope-level fields
// merged in: `id` and `createdAt`, the RFC3339 rendering of `created_at`. A
// GraphQL schema declaring `createdAt: String` resolves it straight off that
// key, so an adapter that omits the field does not error — the field silently
// comes back null. That is what these two steps guard.

fn assert_valid_created_at(stash: &Stash, whence: &str) {
    let created_at = stash
        .get("createdAt")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!("{whence} must carry `createdAt`, an RFC3339 string; got {stash:?}")
        });
    chrono::DateTime::parse_from_rfc3339(created_at).unwrap_or_else(|e| {
        panic!("{whence}: `createdAt` must be RFC3339, got {created_at:?}: {e}")
    });
}

#[then("the search result should have a valid createdAt")]
async fn assert_result_created_at(world: &mut CertWorld) {
    assert_valid_created_at(found(world), "the search result");
}

#[then("every result should have a valid createdAt")]
async fn assert_every_result_created_at(world: &mut CertWorld) {
    assert!(
        !world.search_results.is_empty(),
        "there are no results to check for `createdAt`"
    );
    for result in &world.search_results {
        assert_valid_created_at(result, "every result");
    }
}
