use chrono::Utc;
use cucumber::{given, then, when};
use meshql_core::{Envelope, Stash};
use serde_json::json;

use crate::world::CertWorld;

/// Seed the standard searcher dataset into the repository.
/// Items: (id, name, count, type)
/// s-id-1: alpha, 10, typeA
/// s-id-2: beta, 20, typeB
/// s-id-3: gamma, 30, typeA
/// s-id-4: delta, 40, typeB
#[given("the searcher dataset is seeded")]
async fn seed_data(world: &mut CertWorld) {
    let items = vec![
        ("s-id-1", "alpha", 10i64, "typeA"),
        ("s-id-2", "beta", 20, "typeB"),
        ("s-id-3", "gamma", 30, "typeA"),
        ("s-id-4", "delta", 40, "typeB"),
    ];

    for (id, name, count, item_type) in items {
        let mut payload = Stash::new();
        payload.insert("name".to_string(), json!(name));
        payload.insert("count".to_string(), json!(count));
        payload.insert("type".to_string(), json!(item_type));
        let env = Envelope::new(id, payload, CertWorld::star());
        world.repo().create(env, &CertWorld::star()).await.unwrap();
        // Track in envelopes_by_name so findById substitution works
        let stored = world
            .repo()
            .read(id, &CertWorld::star(), None)
            .await
            .unwrap()
            .unwrap();
        world.envelopes_by_name.insert(name.to_string(), stored);
    }
}

#[when(regex = r#"^I search using template "([^"]+)" with arg "([^"]+)" = "([^"]+)"$"#)]
async fn search_find(
    world: &mut CertWorld,
    template_name: String,
    arg_key: String,
    arg_value: String,
) {
    let template = world
        .templates
        .get(&template_name)
        .cloned()
        .expect("template not found");

    let mut args = Stash::new();
    // For findById, substitute envelope name → actual UUID
    let actual_value = if template_name == "findById" {
        if let Some(env) = world.envelopes_by_name.get(&arg_value) {
            env.id.clone()
        } else {
            arg_value
        }
    } else {
        arg_value
    };
    args.insert(arg_key, json!(actual_value));

    let result = world
        .searcher()
        .find(
            &template,
            &args,
            &CertWorld::star(),
            Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    world.last_search_result = Some(result);
}

#[when(regex = r#"^I search using template "([^"]+)" with args: (.+)$"#)]
async fn search_find_multi_args(world: &mut CertWorld, template_name: String, args_str: String) {
    let template = world
        .templates
        .get(&template_name)
        .cloned()
        .expect("template not found");

    let mut args = Stash::new();
    // Parse "key=value, key=value" pairs
    for pair in args_str.split(',') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            args.insert(k.trim().to_string(), json!(v.trim()));
        }
    }

    let result = world
        .searcher()
        .find(
            &template,
            &args,
            &CertWorld::star(),
            Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    world.last_search_result = Some(result);
}

#[when(regex = r#"^I search all using template "([^"]+)" with arg "([^"]+)" = "([^"]+)"$"#)]
async fn search_find_all(
    world: &mut CertWorld,
    template_name: String,
    arg_key: String,
    arg_value: String,
) {
    let template = world
        .templates
        .get(&template_name)
        .cloned()
        .expect("template not found");

    let mut args = Stash::new();
    args.insert(arg_key, json!(arg_value));

    let results = world
        .searcher()
        .find_all(
            &template,
            &args,
            &CertWorld::star(),
            Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    world.search_results = results;
}

#[when(regex = r#"^I search all using template "([^"]+)" with args: (.+)$"#)]
async fn search_find_all_multi(world: &mut CertWorld, template_name: String, args_str: String) {
    let template = world
        .templates
        .get(&template_name)
        .cloned()
        .expect("template not found");

    let mut args = Stash::new();
    for pair in args_str.split(',') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            args.insert(k.trim().to_string(), json!(v.trim()));
        }
    }

    let results = world
        .searcher()
        .find_all(
            &template,
            &args,
            &CertWorld::star(),
            Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    world.search_results = results;
}

#[when(regex = r#"^I search using literal template '([^']+)'$"#)]
async fn search_literal(world: &mut CertWorld, template: String) {
    let args = Stash::new();
    let result = world
        .searcher()
        .find(
            &template,
            &args,
            &CertWorld::star(),
            Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    world.last_search_result = Some(result);
}

#[when(regex = r#"^I search all using literal template '([^']+)'$"#)]
async fn search_all_literal(world: &mut CertWorld, template: String) {
    let args = Stash::new();
    let results = world
        .searcher()
        .find_all(
            &template,
            &args,
            &CertWorld::star(),
            Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    world.search_results = results;
}

#[when(regex = r#"^I search all using literal template '([^']+)' with limit (\d+)$"#)]
async fn search_all_literal_limit(world: &mut CertWorld, template: String, limit: i64) {
    let mut args = Stash::new();
    args.insert("limit".to_string(), json!(limit));
    let results = world
        .searcher()
        .find_all(
            &template,
            &args,
            &CertWorld::star(),
            Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    world.search_results = results;
}

// ---- Then assertions ----

#[then("the search result should be empty")]
async fn assert_empty(world: &mut CertWorld) {
    assert!(
        world
            .last_search_result
            .as_ref()
            .map(|r| r.is_none())
            .unwrap_or(true),
        "expected empty result"
    );
}

#[then("the search result should not be empty")]
async fn assert_not_empty(world: &mut CertWorld) {
    assert!(
        world
            .last_search_result
            .as_ref()
            .map(|r| r.is_some())
            .unwrap_or(false),
        "expected a result"
    );
}

#[then(regex = r#"^the search result should have "([^"]+)" = "([^"]+)"$"#)]
async fn assert_result_field(world: &mut CertWorld, field: String, expected: String) {
    let result = world
        .last_search_result
        .as_ref()
        .expect("no search result set")
        .as_ref()
        .expect("search result was None");
    let actual = result.get(&field).expect("field not found");
    assert_eq!(actual, &json!(expected), "field '{field}' mismatch");
}

#[then(regex = r#"^the search results count should be (\d+)$"#)]
async fn assert_results_count(world: &mut CertWorld, count: usize) {
    assert_eq!(
        world.search_results.len(),
        count,
        "expected {count} results, got {}",
        world.search_results.len()
    );
}

#[then("the search results should be empty")]
async fn assert_results_empty(world: &mut CertWorld) {
    assert!(world.search_results.is_empty(), "expected empty results");
}

#[then("the search results should not be empty")]
async fn assert_results_not_empty(world: &mut CertWorld) {
    assert!(
        !world.search_results.is_empty(),
        "expected non-empty results"
    );
}

#[then(regex = r#"^all search results should have "([^"]+)" = "([^"]+)"$"#)]
async fn assert_all_field(world: &mut CertWorld, field: String, expected: String) {
    for r in &world.search_results {
        let actual = r.get(&field).expect("field not found");
        assert_eq!(actual, &json!(expected), "field '{field}' mismatch");
    }
}
