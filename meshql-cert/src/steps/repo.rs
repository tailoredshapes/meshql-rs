use chrono::Utc;
use cucumber::{given, then, when};
use meshql_core::{Envelope, Stash};
use serde_json::json;

use crate::world::CertWorld;

#[given("a fresh repository instance")]
async fn fresh_repo(world: &mut CertWorld) {
    // Repository is injected via the test runner's before hook.
    // This step just records the start time for "recent" assertions.
    world.test_start = Utc::now();
}

#[when(regex = r#"^I create envelopes? named? "([^"]+)"$"#)]
async fn create_envelope_named(world: &mut CertWorld, name: String) {
    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!(name));
    let id = format!("env-{}", uuid::Uuid::new_v4().simple());
    let env = Envelope::new(id, payload, CertWorld::star());
    let result = world.repo().create(env, &CertWorld::star()).await.unwrap();
    world.envelopes_by_name.insert(name, result.clone());
    world.last_envelopes = vec![result];
}

#[when(regex = r#"^I create (\d+) envelopes? named? "([^"]+)"$"#)]
async fn create_n_envelopes(world: &mut CertWorld, count: usize, base_name: String) {
    world.last_envelopes.clear();
    for i in 0..count {
        let name = format!("{base_name}-{i}");
        let mut payload = Stash::new();
        payload.insert("name".to_string(), json!(name));
        let id = format!("env-{}", uuid::Uuid::new_v4().simple());
        let env = Envelope::new(id, payload, CertWorld::star());
        let result = world.repo().create(env, &CertWorld::star()).await.unwrap();
        world.envelopes_by_name.insert(name, result.clone());
        world.last_envelopes.push(result);
    }
}

#[when(regex = r#"^I read the envelope named "([^"]+)"$"#)]
async fn read_by_name(world: &mut CertWorld, name: String) {
    let env = world
        .envelopes_by_name
        .get(&name)
        .expect("envelope not found");
    let id = env.id.clone();
    let result = world
        .repo()
        .read(&id, &CertWorld::star(), None)
        .await
        .unwrap();
    world.last_search_result = Some(result.map(|e| {
        let mut s = e.payload.clone();
        s.insert("id".to_string(), json!(e.id));
        s
    }));
}

#[when("I list all envelopes")]
async fn list_all(world: &mut CertWorld) {
    let results = world.repo().list(&CertWorld::star()).await.unwrap();
    world.last_envelopes = results;
}

#[when(regex = r#"^I remove the envelope named "([^"]+)"$"#)]
async fn remove_by_name(world: &mut CertWorld, name: String) {
    let env = world
        .envelopes_by_name
        .get(&name)
        .expect("envelope not found");
    let id = env.id.clone();
    let result = world.repo().remove(&id, &CertWorld::star()).await.unwrap();
    world.last_remove = result;
}

#[when(regex = r#"^I create many envelopes with base name "([^"]+)" and count (\d+)$"#)]
async fn create_many(world: &mut CertWorld, base_name: String, count: usize) {
    let envelopes: Vec<Envelope> = (0..count)
        .map(|i| {
            let name = format!("{base_name}-{i}");
            let mut payload = Stash::new();
            payload.insert("name".to_string(), json!(name));
            let id = format!("bulk-{}", uuid::Uuid::new_v4().simple());
            Envelope::new(id, payload, CertWorld::star())
        })
        .collect();
    let results = world
        .repo()
        .create_many(envelopes, &CertWorld::star())
        .await
        .unwrap();
    for r in &results {
        let name = r.payload.get("name").unwrap().as_str().unwrap().to_string();
        world.envelopes_by_name.insert(name, r.clone());
    }
    world.last_envelopes = results;
}

#[when(regex = r#"^I read many envelopes named "([^"]+)"$"#)]
async fn read_many_by_prefix(world: &mut CertWorld, prefix: String) {
    let ids: Vec<String> = world
        .envelopes_by_name
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(_, v)| v.id.clone())
        .collect();
    let results = world
        .repo()
        .read_many(&ids, &CertWorld::star())
        .await
        .unwrap();
    world.last_envelopes = results;
}

#[when(regex = r#"^I remove many envelopes named "([^"]+)"$"#)]
async fn remove_many_by_prefix(world: &mut CertWorld, prefix: String) {
    let ids: Vec<String> = world
        .envelopes_by_name
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(_, v)| v.id.clone())
        .collect();
    let results = world
        .repo()
        .remove_many(&ids, &CertWorld::star())
        .await
        .unwrap();
    world.remove_results = results;
}

#[when(
    regex = r#"^I create a version 1 envelope named "([^"]+)" with value "([^"]+)" dated (\d+) seconds ago$"#
)]
async fn create_v1(world: &mut CertWorld, name: String, value: String, seconds_ago: i64) {
    let mut payload = Stash::new();
    payload.insert("version".to_string(), json!(value));
    let id = format!("temporal-{}", uuid::Uuid::new_v4().simple());
    let env = Envelope {
        id: id.clone(),
        payload,
        created_at: Utc::now() - chrono::Duration::seconds(seconds_ago),
        deleted: false,
        authorized_tokens: CertWorld::star(),
    };
    let result = world.repo().create(env, &CertWorld::star()).await.unwrap();
    world.envelopes_by_name.insert(name.clone(), result);
    // Record the midpoint timestamp
    world.timestamps.insert(
        format!("before_{name}"),
        Utc::now() - chrono::Duration::seconds(seconds_ago / 2),
    );
}

#[when(regex = r#"^I create a version 2 envelope for "([^"]+)" with value "([^"]+)"$"#)]
async fn create_v2(world: &mut CertWorld, name: String, value: String) {
    let env_v1 = world.envelopes_by_name.get(&name).expect("v1 not found");
    let id = env_v1.id.clone();
    let mut payload = Stash::new();
    payload.insert("version".to_string(), json!(value));
    let env = Envelope::new(id, payload, CertWorld::star());
    let result = world.repo().create(env, &CertWorld::star()).await.unwrap();
    world.envelopes_by_name.insert(name, result);
}

#[when(regex = r#"^I read envelope "([^"]+)" at timestamp "([^"]+)"$"#)]
async fn read_at_timestamp(world: &mut CertWorld, name: String, ts_key: String) {
    let env = world
        .envelopes_by_name
        .get(&name)
        .expect("envelope not found");
    let id = env.id.clone();
    let at = *world.timestamps.get(&ts_key).expect("timestamp not found");
    let result = world
        .repo()
        .read(&id, &CertWorld::star(), Some(at))
        .await
        .unwrap();
    world.last_search_result = Some(result.map(|e| {
        let mut s = e.payload.clone();
        s.insert("id".to_string(), json!(e.id));
        s
    }));
}

#[when(regex = r#"^I read envelope "([^"]+)" now$"#)]
async fn read_now(world: &mut CertWorld, name: String) {
    let env = world
        .envelopes_by_name
        .get(&name)
        .expect("envelope not found");
    let id = env.id.clone();
    let result = world
        .repo()
        .read(&id, &CertWorld::star(), None)
        .await
        .unwrap();
    world.last_search_result = Some(result.map(|e| {
        let mut s = e.payload.clone();
        s.insert("id".to_string(), json!(e.id));
        s
    }));
}

#[when(
    regex = r#"^I create two versions of envelope "([^"]+)" with old value "([^"]+)" and new value "([^"]+)"$"#
)]
async fn create_two_versions(
    world: &mut CertWorld,
    name: String,
    old_value: String,
    new_value: String,
) {
    let id = format!("latest-{}", uuid::Uuid::new_v4().simple());

    let mut payload_v1 = Stash::new();
    payload_v1.insert("version".to_string(), json!(old_value));
    let env_v1 = Envelope {
        id: id.clone(),
        payload: payload_v1,
        created_at: Utc::now() - chrono::Duration::seconds(10),
        deleted: false,
        authorized_tokens: CertWorld::star(),
    };
    world
        .repo()
        .create(env_v1, &CertWorld::star())
        .await
        .unwrap();

    let mut payload_v2 = Stash::new();
    payload_v2.insert("version".to_string(), json!(new_value));
    let env_v2 = Envelope::new(id.clone(), payload_v2, CertWorld::star());
    let result_v2 = world
        .repo()
        .create(env_v2, &CertWorld::star())
        .await
        .unwrap();
    world.envelopes_by_name.insert(name, result_v2);
}

// ---- Then assertions ----

#[then("the envelopes should have generated IDs")]
async fn assert_have_ids(world: &mut CertWorld) {
    for env in &world.last_envelopes {
        assert!(!env.id.is_empty(), "envelope should have an id");
    }
}

#[then("the envelopes created_at should be recent")]
async fn assert_recent(world: &mut CertWorld) {
    let now = Utc::now();
    for env in &world.last_envelopes {
        let diff = (now - env.created_at).num_seconds().abs();
        assert!(diff < 60, "created_at should be recent, diff={diff}s");
    }
}

#[then("the envelopes deleted flag should be false")]
async fn assert_not_deleted(world: &mut CertWorld) {
    for env in &world.last_envelopes {
        assert!(!env.deleted, "envelope should not be deleted");
    }
}

#[then(regex = r#"^the envelope list should contain at least (\d+) items?$"#)]
async fn assert_list_count(world: &mut CertWorld, min: usize) {
    assert!(
        world.last_envelopes.len() >= min,
        "expected >= {min} envelopes, got {}",
        world.last_envelopes.len()
    );
}

#[then(regex = r#"^the envelope list should contain "([^"]+)"$"#)]
async fn assert_list_contains(world: &mut CertWorld, name: String) {
    let env = world.envelopes_by_name.get(&name).expect("not in map");
    let found = world.last_envelopes.iter().any(|e| e.id == env.id);
    assert!(found, "envelope '{name}' not found in list");
}

#[then("the read should succeed")]
async fn assert_read_success(world: &mut CertWorld) {
    assert!(
        world
            .last_search_result
            .as_ref()
            .map(|r| r.is_some())
            .unwrap_or(false),
        "expected a result but got None"
    );
}

#[then(regex = r#"^the remove should return true$"#)]
async fn assert_remove_true(world: &mut CertWorld) {
    assert!(world.last_remove, "expected remove to return true");
}

#[then(regex = r#"^reading "([^"]+)" should return None$"#)]
async fn assert_reading_returns_none(world: &mut CertWorld, name: String) {
    let env = world
        .envelopes_by_name
        .get(&name)
        .expect("envelope not found");
    let id = env.id.clone();
    let result = world
        .repo()
        .read(&id, &CertWorld::star(), None)
        .await
        .unwrap();
    assert!(result.is_none(), "expected None after delete");
}

#[then(regex = r#"^I should have (\d+) created envelopes$"#)]
async fn assert_create_count(world: &mut CertWorld, count: usize) {
    assert_eq!(world.last_envelopes.len(), count);
}

#[then(regex = r#"^I should have (\d+) read envelopes$"#)]
async fn assert_read_count(world: &mut CertWorld, count: usize) {
    assert_eq!(world.last_envelopes.len(), count);
}

#[then("all removes should succeed")]
async fn assert_all_removes(world: &mut CertWorld) {
    assert!(
        world.remove_results.values().all(|&v| v),
        "all removes should return true"
    );
}

#[then(regex = r#"^the result at "([^"]+)" should have version "([^"]+)"$"#)]
async fn assert_version_at(world: &mut CertWorld, _ts_key: String, expected: String) {
    let result = world
        .last_search_result
        .as_ref()
        .expect("no result")
        .as_ref()
        .expect("result was None");
    let version = result.get("version").expect("no version field");
    assert_eq!(version, &serde_json::json!(expected));
}

#[then(regex = r#"^listing should return exactly 1 result for "([^"]+)"$"#)]
async fn assert_list_one_for(world: &mut CertWorld, name: String) {
    let env = world.envelopes_by_name.get(&name).expect("not in map");
    let matching: Vec<_> = world
        .last_envelopes
        .iter()
        .filter(|e| e.id == env.id)
        .collect();
    assert_eq!(matching.len(), 1, "should have exactly 1 version in list");
}

#[then(regex = r#"^the listed version should have value "([^"]+)"$"#)]
async fn assert_listed_version(world: &mut CertWorld, expected: String) {
    // Find the first envelope that has a "version" field
    let found = world
        .last_envelopes
        .iter()
        .find(|e| e.payload.contains_key("version"));
    let env = found.expect("no envelope with version field");
    assert_eq!(
        env.payload.get("version").unwrap(),
        &serde_json::json!(expected)
    );
}
