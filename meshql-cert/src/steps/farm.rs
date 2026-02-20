use chrono::Utc;
use cucumber::{given, then, when};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::world::CertWorld;

/// Resolve <ids.entity_type.name> references in a string
fn resolve_ids(s: &str, ids: &HashMap<String, HashMap<String, String>>) -> String {
    let mut result = s.to_string();
    // Iterate until no more substitutions needed
    for _ in 0..10 {
        if !result.contains("<ids.") {
            break;
        }
        let mut new_result = result.clone();
        // Simple scan for <ids.X.Y> patterns
        let mut start = 0;
        while let Some(open) = new_result[start..].find("<ids.") {
            let open_pos = start + open;
            if let Some(close) = new_result[open_pos..].find('>') {
                let placeholder = &new_result[open_pos..open_pos + close + 1];
                let inner = &placeholder[1..placeholder.len() - 1]; // strip < >
                                                                    // inner = "ids.entity_type.name"
                let parts: Vec<&str> = inner.splitn(3, '.').collect();
                if parts.len() == 3 {
                    let entity_type = parts[1];
                    let name = parts[2];
                    if let Some(type_map) = ids.get(entity_type) {
                        if let Some(id) = type_map.get(name) {
                            new_result = new_result.replacen(placeholder, id, 1);
                            start = 0;
                            continue;
                        }
                    }
                }
                start = open_pos + close + 1;
            } else {
                break;
            }
        }
        result = new_result;
    }
    result
}

async fn post_entity(
    client: &reqwest::Client,
    server_addr: &str,
    entity_type: &str,
    data: Value,
) -> String {
    let url = format!("{server_addr}/{entity_type}/api");
    let resp = client.post(&url).json(&data).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 201, "POST {entity_type} failed");
    resp.text().await.unwrap(); // consume body

    // Use GET list to find the entity
    let list_resp: Value = client.get(&url).send().await.unwrap().json().await.unwrap();

    if let Some(items) = list_resp.as_array() {
        // Try matching by name first (for named entities)
        let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if !name.is_empty() {
            for item in items {
                if item.get("name").and_then(|v| v.as_str()) == Some(name) {
                    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                        return id.to_string();
                    }
                }
            }
        }

        // Fallback: match by all posted data fields (for events/projections)
        if let Some(obj) = data.as_object() {
            for item in items.iter().rev() {
                let all_match = obj.iter().all(|(k, v)| item.get(k) == Some(v));
                if all_match {
                    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                        return id.to_string();
                    }
                }
            }
        }
    }
    panic!("Could not find {entity_type} after creation");
}

async fn graphql_query(
    client: &reqwest::Client,
    server_addr: &str,
    entity_type: &str,
    query: &str,
) -> Value {
    let url = format!("{server_addr}/{entity_type}/graph");
    let body = json!({ "query": query });
    let resp: Value = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    resp
}

/// Navigate a JSON value by dot-separated path (e.g., "data.getById.name")
fn json_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    Some(current)
}

#[given("a MeshQL farm server is running")]
async fn server_running(world: &mut CertWorld) {
    // The server addr is injected by the test runner before hook
    assert!(
        world.server_addr.is_some(),
        "server_addr must be set before this step"
    );
}

#[given("a MeshQL egg economy server is running")]
async fn egg_economy_server_running(world: &mut CertWorld) {
    assert!(
        world.server_addr.is_some(),
        "server_addr must be set before this step"
    );
}

#[given(regex = r#"^I have created "([^"]+)" entities:$"#)]
async fn create_entities(
    world: &mut CertWorld,
    entity_type: String,
    step: &cucumber::gherkin::Step,
) {
    let client = reqwest::Client::new();
    let server_addr = world.server_addr.clone().unwrap();

    let table = step.table.as_ref().expect("expected a table");
    // Header row: name | data
    for row in table.rows.iter().skip(1) {
        let name = row[0].trim().to_string();
        let raw_data = row[1].trim().to_string();
        // Resolve any <ids.X.Y> references
        let resolved_data = resolve_ids(&raw_data, &world.ids);
        let data: Value = serde_json::from_str(&resolved_data).expect("invalid JSON in table");

        let id = post_entity(&client, &server_addr, &entity_type, data).await;
        world
            .ids
            .entry(entity_type.clone())
            .or_default()
            .insert(name, id);
    }
}

#[given(regex = r#"^I capture the current timestamp as "([^"]+)"$"#)]
async fn capture_timestamp(world: &mut CertWorld, key: String) {
    let ms = Utc::now().timestamp_millis();
    if key == "first_stamp" || key == "before_update" {
        world.first_stamp_ms = Some(ms);
    }
    world.timestamps.insert(key, Utc::now());
    // Small sleep to ensure temporal separation
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
}

#[given(regex = r#"^I update "([^"]+)" "([^"]+)" with data (.+)$"#)]
async fn update_entity(world: &mut CertWorld, entity_type: String, name: String, raw_data: String) {
    let client = reqwest::Client::new();
    let server_addr = world.server_addr.clone().unwrap();

    let id = world
        .ids
        .get(&entity_type)
        .and_then(|m| m.get(&name))
        .cloned()
        .expect("entity not found");

    let resolved_data = resolve_ids(&raw_data, &world.ids);
    let data: Value = serde_json::from_str(&resolved_data).expect("invalid JSON");

    let url = format!("{server_addr}/{entity_type}/api/{id}");
    let resp = client.put(&url).json(&data).send().await.unwrap();
    assert!(resp.status().is_success(), "PUT {entity_type}/{id} failed");
}

#[when(regex = r#"^I query the "([^"]+)" graph with: (.+)$"#)]
async fn query_graph(world: &mut CertWorld, entity_type: String, raw_query: String) {
    let client = reqwest::Client::new();
    let server_addr = world.server_addr.clone().unwrap();

    let resolved_query = resolve_ids(&raw_query, &world.ids);
    let response = graphql_query(&client, &server_addr, &entity_type, &resolved_query).await;
    world.farm_response = Some(response);
}

#[when(regex = r#"^I query the "([^"]+)" graph with at=first_stamp: (.+)$"#)]
async fn query_graph_at_stamp(world: &mut CertWorld, entity_type: String, raw_query: String) {
    let client = reqwest::Client::new();
    let server_addr = world.server_addr.clone().unwrap();

    let first_stamp = world.first_stamp_ms.expect("first_stamp not captured");
    let resolved = resolve_ids(&raw_query, &world.ids);
    // Inject at parameter — the query should contain "at: STAMP_PLACEHOLDER"
    let resolved_query = resolved.replace("at: first_stamp", &format!("at: {first_stamp}"));
    let response = graphql_query(&client, &server_addr, &entity_type, &resolved_query).await;
    world.farm_response = Some(response);
}

// ---- Then assertions ----

// Legacy assertion kept for backward compatibility with farm.feature
#[then(regex = r#"^the response data\.([a-zA-Z]+)\.name should be "([^"]+)"$"#)]
async fn assert_name(world: &mut CertWorld, field: String, expected: String) {
    let resp = world.farm_response.as_ref().expect("no response");
    let value = resp["data"][&field]["name"]
        .as_str()
        .expect("no name field");
    assert_eq!(value, expected, "name mismatch");
}

#[then(regex = r#"^the response data\.([a-zA-Z]+)\.([a-zA-Z]+) should have (\d+) items?$"#)]
async fn assert_array_count(world: &mut CertWorld, root: String, field: String, count: usize) {
    let resp = world.farm_response.as_ref().expect("no response");
    let arr = resp["data"][&root][&field]
        .as_array()
        .expect("not an array");
    assert_eq!(arr.len(), count, "array count mismatch for {root}.{field}");
}

// Generic path-based assertions for egg economy and beyond

#[then(regex = r#"^the response at "([^"]+)" should be "([^"]+)"$"#)]
async fn assert_path_string(world: &mut CertWorld, path: String, expected: String) {
    let resp = world.farm_response.as_ref().expect("no response");
    let value = json_at_path(resp, &path)
        .unwrap_or_else(|| panic!("path '{path}' not found in response: {resp}"));
    let actual = value
        .as_str()
        .unwrap_or_else(|| panic!("value at '{path}' is not a string: {value}"));
    assert_eq!(actual, expected, "mismatch at path '{path}'");
}

#[then(regex = r#"^the response at "([^"]+)" should have (\d+) items?$"#)]
async fn assert_path_array_count(world: &mut CertWorld, path: String, count: usize) {
    let resp = world.farm_response.as_ref().expect("no response");
    let value = json_at_path(resp, &path)
        .unwrap_or_else(|| panic!("path '{path}' not found in response: {resp}"));
    let arr = value
        .as_array()
        .unwrap_or_else(|| panic!("value at '{path}' is not an array: {value}"));
    assert_eq!(arr.len(), count, "array count mismatch at path '{path}'");
}

#[then("there should be no GraphQL errors")]
async fn assert_no_errors(world: &mut CertWorld) {
    let resp = world.farm_response.as_ref().expect("no response");
    if let Some(errors) = resp.get("errors") {
        if !errors.is_null() {
            panic!("GraphQL errors: {errors}");
        }
    }
}
