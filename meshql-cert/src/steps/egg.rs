//! Steps of `egg_economy.feature` (contract, `features/proposed/egg-economy/`).
//!
//! Everything here is the front end: it holds the `frontend` credentials, posts
//! actors and events, and reads. Only [`crate::egg_economy::worker`] writes a
//! projection, with the worker's credentials. "Eventually" steps poll for at most
//! `EVENTUALLY` because the worker runs behind the (harness's) CDC.

use cucumber::{given, then, when};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use super::farm::{collect_at_path, graphql_query, resolve_ids};
use crate::authz::IDENTITY_HEADER;
use crate::egg_economy::FRONT_END;
use crate::world::CertWorld;

/// How long an "eventually" step waits for a worker (the contract's default).
const EVENTUALLY: Duration = Duration::from_secs(10);

/// Send a request as the front end. Returns the status and the body (null when
/// the body is not JSON, as for a 405).
async fn front_end(
    world: &mut CertWorld,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let url = format!("{}{path}", world.server_addr());
    let mut req = reqwest::Client::new()
        .request(method, url)
        .header(IDENTITY_HEADER, FRONT_END);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.expect("request to the economy");
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    world.egg_status = Some(status);
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn id_named(world: &CertWorld, entity: &str, name: &str) -> String {
    world
        .ids
        .get(entity)
        .and_then(|m| m.get(name))
        .cloned()
        .unwrap_or_else(|| panic!("no {entity} named {name} was made"))
}

async fn make(world: &mut CertWorld, entity: &str, name: &str, doc: Value) {
    let (status, body) = front_end(
        world,
        reqwest::Method::POST,
        &format!("/{entity}/api"),
        Some(doc),
    )
    .await;
    assert_eq!(status, 201, "POST {entity} {name} answered {status}: {body}");
    let id = body["id"].as_str().expect("created id").to_string();
    world
        .ids
        .entry(entity.to_string())
        .or_default()
        .insert(name.to_string(), id);
}

async fn list(world: &mut CertWorld, entity: &str) -> Vec<Value> {
    let (status, body) = front_end(world, reqwest::Method::GET, &format!("/{entity}/api"), None).await;
    assert_eq!(status, 200, "GET {entity} answered {status}");
    body.as_array().cloned().unwrap_or_default()
}

#[given("an egg economy deployment is running")]
async fn deployment_running(world: &mut CertWorld) {
    assert!(
        world.server_addr.is_some(),
        "server_addr must be set before this step"
    );
    // Fourteen entities, each with a restlette and a graphlette.
    for entity in crate::egg_economy::entities() {
        let (status, _) = front_end(world, reqwest::Method::GET, &format!("/{entity}/api"), None).await;
        assert_eq!(status, 200, "/{entity}/api is not served");
        let url = format!("{}/{entity}/graph", world.server_addr());
        let resp = graphql_query(&reqwest::Client::new(), world.server_addr(), entity, "{ getAll { id } }").await;
        assert!(resp.get("errors").is_none_or(Value::is_null), "/{entity}/graph: {resp} ({url})");
    }
}

#[given(regex = r#"^a farm "([^"]+)" with a coop "([^"]+)" and a hen "([^"]+)"$"#)]
async fn farm_coop_hen(world: &mut CertWorld, farm: String, coop: String, hen: String) {
    make(
        world,
        "farm",
        &farm,
        json!({"name": farm, "farm_type": "family", "zone": "north"}),
    )
    .await;
    let farm_id = id_named(world, "farm", &farm);
    make(
        world,
        "coop",
        &coop,
        json!({"name": coop, "farm_id": farm_id, "capacity": 10, "coop_type": "barn"}),
    )
    .await;
    let coop_id = id_named(world, "coop", &coop);
    make(
        world,
        "hen",
        &hen,
        json!({"name": hen, "coop_id": coop_id, "breed": "leghorn"}),
    )
    .await;
}

/// Entities the harness creates with a named worker's credentials: the
/// projections a local feature needs to exist before it queries them. (The
/// contract's feature never does; it lets the worker make them.)
#[given(regex = r#"^the worker "([^"]+)" has created "([^"]+)" entities:$"#)]
async fn worker_creates(
    world: &mut CertWorld,
    worker: String,
    entity: String,
    step: &cucumber::gherkin::Step,
) {
    let table = step.table.as_ref().expect("expected a table");
    for row in table.rows.iter().skip(1) {
        let name = row[0].trim().to_string();
        let doc: Value =
            serde_json::from_str(&resolve_ids(row[1].trim(), &world.ids)).expect("invalid JSON");
        let resp = reqwest::Client::new()
            .post(format!("{}/{entity}/api", world.server_addr()))
            .header(IDENTITY_HEADER, &worker)
            .json(&doc)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 201, "{worker} could not write {entity}");
        let body: Value = resp.json().await.unwrap();
        let id = body["id"].as_str().unwrap().to_string();
        world.ids.entry(entity.clone()).or_default().insert(name, id);
    }
}

#[when(regex = r#"^the hen "([^"]+)" lays (\d+) eggs of quality "([^"]+)" at "([^"]+)"$"#)]
async fn hen_lays(world: &mut CertWorld, hen: String, eggs: i64, quality: String, at: String) {
    let hen_id = id_named(world, "hen", &hen);
    // The hen's coop and farm, as the front end knows them.
    let (_, hen_doc) = front_end(world, reqwest::Method::GET, &format!("/hen/api/{hen_id}"), None).await;
    let coop_id = hen_doc["coop_id"].as_str().expect("hen has a coop").to_string();
    let (_, coop_doc) = front_end(world, reqwest::Method::GET, &format!("/coop/api/{coop_id}"), None).await;
    let farm_id = coop_doc["farm_id"].as_str().expect("coop has a farm").to_string();
    let (status, body) = front_end(
        world,
        reqwest::Method::POST,
        "/lay_report/api",
        Some(json!({
            "hen_id": hen_id, "coop_id": coop_id, "farm_id": farm_id,
            "eggs": eggs, "quality": quality, "timestamp": at,
        })),
    )
    .await;
    if status == 201 {
        world.egg_lay_report = body["id"].as_str().map(String::from);
    }
}

#[when(regex = r#"^the coop "([^"]+)" is renamed "([^"]+)"$"#)]
async fn coop_renamed(world: &mut CertWorld, coop: String, name: String) {
    let id = id_named(world, "coop", &coop);
    let path = format!("/coop/api/{id}");
    let (status, mut doc) = front_end(world, reqwest::Method::GET, &path, None).await;
    assert_eq!(status, 200, "GET {path} answered {status}");
    doc["name"] = json!(name);
    let (status, body) = front_end(world, reqwest::Method::PUT, &path, Some(doc)).await;
    assert_eq!(status, 200, "PUT {path} answered {status}: {body}");
}

#[when(regex = r#"^the front end writes a hen_productivity of (\d+) total eggs$"#)]
async fn front_end_writes_projection(world: &mut CertWorld, total: i64) {
    let hen_id = id_named(world, "hen", "Ada");
    let farm_id = id_named(world, "farm", "Emerdale");
    front_end(
        world,
        reqwest::Method::POST,
        "/hen_productivity/api",
        Some(json!({"hen_id": hen_id, "farm_id": farm_id, "total_eggs": total, "quality_rate": 1.0})),
    )
    .await;
}

#[then("the posting was accepted")]
async fn posting_accepted(world: &mut CertWorld) {
    assert_eq!(world.egg_status, Some(201), "the last POST did not answer 201");
}

fn lay_report_path(world: &CertWorld) -> String {
    format!(
        "/lay_report/api/{}",
        world.egg_lay_report.as_deref().expect("no lay report was accepted")
    )
}

#[then(regex = r#"^a lay report of the hen "([^"]+)" can be read by its id$"#)]
async fn lay_report_readable(world: &mut CertWorld, hen: String) {
    let hen_id = id_named(world, "hen", &hen);
    let path = lay_report_path(world);
    let (status, body) = front_end(world, reqwest::Method::GET, &path, None).await;
    assert_eq!(status, 200, "GET {path} answered {status}");
    assert_eq!(body["hen_id"], json!(hen_id), "that lay report is not the hen's: {body}");
}

#[then("replacing that lay report is refused")]
async fn replacing_refused(world: &mut CertWorld) {
    let path = lay_report_path(world);
    let (status, _) = front_end(world, reqwest::Method::PUT, &path, Some(json!({"eggs": 99}))).await;
    assert_eq!(status, 405, "PUT {path} answered {status}, not 405");
    let (_, body) = front_end(world, reqwest::Method::GET, &path, None).await;
    assert_ne!(body["eggs"], json!(99), "the lay report was changed anyway: {body}");
}

#[then("deleting that lay report is refused")]
async fn deleting_refused(world: &mut CertWorld) {
    let path = lay_report_path(world);
    let (status, _) = front_end(world, reqwest::Method::DELETE, &path, None).await;
    assert_eq!(status, 405, "DELETE {path} answered {status}, not 405");
    let (status, _) = front_end(world, reqwest::Method::GET, &path, None).await;
    assert_eq!(status, 200, "the lay report is gone after a refused DELETE");
}

async fn productivity_of(world: &mut CertWorld, hen: &str) -> Option<Value> {
    let hen_id = id_named(world, "hen", hen);
    list(world, "hen_productivity")
        .await
        .into_iter()
        .find(|p| p["hen_id"] == json!(hen_id))
}

#[then(regex = r#"^the hen "([^"]+)" has produced (\d+) eggs eventually$"#)]
async fn hen_produced(world: &mut CertWorld, hen: String, eggs: i64) {
    let what = format!("hen {hen} produced {eggs} eggs");
    let start = Instant::now();
    loop {
        if let Some(p) = productivity_of(world, &hen).await {
            if p["total_eggs"] == json!(eggs) {
                return;
            }
        }
        assert!(start.elapsed() <= EVENTUALLY, "never became true within {EVENTUALLY:?}: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[then(regex = r#"^no hen_productivity of (\d+) total eggs exists$"#)]
async fn no_such_projection(world: &mut CertWorld, total: i64) {
    // Give a worker, and any write that was wrongly admitted, time to land.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let found: Vec<Value> = list(world, "hen_productivity")
        .await
        .into_iter()
        .filter(|p| p["total_eggs"] == json!(total))
        .collect();
    assert!(found.is_empty(), "the front end wrote a projection: {found:?}");
}

#[when(regex = r#"^I query the "([^"]+)" graph as of "([^"]+)" with: (.+)$"#)]
async fn query_graph_as_of(world: &mut CertWorld, entity: String, label: String, raw_query: String) {
    let at = world.authz_stamp(&label);
    let query = resolve_ids(&raw_query, &world.ids);
    // The first query field gets the instant as `at`.
    let open = query.find('(').expect("a query field with arguments to give `at`");
    let query = format!("{}at: {at}, {}", &query[..=open], &query[open + 1..]);
    world.farm_response = Some(
        graphql_query(&reqwest::Client::new(), world.server_addr(), &entity, &query).await,
    );
}

#[then(regex = r#"^the response at "([^"]+)" should contain the number (\d+)$"#)]
async fn response_contains_number(world: &mut CertWorld, path: String, n: i64) {
    let resp = world.farm_response.as_ref().expect("no response");
    let found = collect_at_path(resp, &path)
        .iter()
        .any(|v| v.as_f64() == Some(n as f64));
    assert!(found, "no {n} at '{path}' in {resp}");
}
