//! The harness's stand-in for CDC and the reference hen worker. Not under test.
//!
//! The contract certifies neither how a committed event reaches a worker nor what
//! a worker does with it (`specs/egg-economy.md`, rule 6); it needs both only so
//! the surfaces have a projection to serve. The quickest thing that does it for
//! every store (SQLite, PostgreSQL, MySQL, MongoDB, merkql) is to carry events
//! over the restlettes' own HTTP surface: a poller lists `lay_report` as the
//! worker, treats each id it has not delivered as newly committed, and hands it to
//! the worker, which writes `hen_productivity` back through the restlette with the
//! worker's credentials. A deployment would use Debezium and Kafka, merkql-connect
//! or a trigger and a queue table instead; the library knows none of them.
//!
//! The worker follows the guidance of the spec's rules 1 and 4 and no more: the
//! projection is recomputed from the hen's lay reports (a function of the events,
//! so a redelivery changes nothing) and uses only what the events carry. It
//! writes no rejections and no `farm_output` or `container_inventory`: nothing
//! certified reads them.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;

use super::HEN_WORKER;
use crate::authz::IDENTITY_HEADER;

/// Grades the spec counts as good for `quality_rate`.
const GOOD: [&str; 2] = ["grade_a", "double_yolk"];

/// Start the poller and the hen worker against a deployment. Aborting the
/// returned handle stops them.
pub fn start(base: String) -> tokio::task::AbortHandle {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut delivered: HashSet<String> = HashSet::new();
        loop {
            if let Ok(reports) = list(&client, &base, "lay_report").await {
                let fresh: Vec<&Value> = reports
                    .iter()
                    .filter(|r| !delivered.contains(id_of(r)))
                    .collect();
                let hens: HashSet<&str> = fresh
                    .iter()
                    .filter_map(|r| r.get("hen_id").and_then(Value::as_str))
                    .collect();
                let mut all_ok = true;
                for hen in hens {
                    all_ok &= fold_hen(&client, &base, hen, &reports).await.is_ok();
                }
                if all_ok {
                    delivered.extend(fresh.iter().map(|r| id_of(r).to_string()));
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .abort_handle()
}

fn id_of(v: &Value) -> &str {
    v.get("id").and_then(Value::as_str).unwrap_or("")
}

async fn list(client: &reqwest::Client, base: &str, entity: &str) -> Result<Vec<Value>, String> {
    let resp = client
        .get(format!("{base}/{entity}/api"))
        .header(IDENTITY_HEADER, HEN_WORKER)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match resp.json::<Value>().await.map_err(|e| e.to_string())? {
        Value::Array(items) => Ok(items),
        other => Err(format!("{entity} listing was not an array: {other}")),
    }
}

/// Recompute one hen's projection from all of its lay reports and write it if
/// it differs from what is stored.
async fn fold_hen(
    client: &reqwest::Client,
    base: &str,
    hen: &str,
    reports: &[Value],
) -> Result<(), String> {
    let mine: Vec<&Value> = reports
        .iter()
        .filter(|r| r.get("hen_id").and_then(Value::as_str) == Some(hen))
        .collect();
    let eggs = |r: &Value| r.get("eggs").and_then(Value::as_i64).unwrap_or(0);
    let total: i64 = mine.iter().map(|r| eggs(r)).sum();
    let good: i64 = mine
        .iter()
        .filter(|r| {
            r.get("quality")
                .and_then(Value::as_str)
                .is_some_and(|q| GOOD.contains(&q))
        })
        .map(|r| eggs(r))
        .sum();
    let rate = if total > 0 {
        good as f64 / total as f64
    } else {
        0.0
    };
    let farm = mine
        .iter()
        .find_map(|r| r.get("farm_id").and_then(Value::as_str))
        .unwrap_or("");
    let doc = json!({"hen_id": hen, "farm_id": farm, "total_eggs": total, "quality_rate": rate});

    let existing = list(client, base, "hen_productivity")
        .await?
        .into_iter()
        .find(|p| p.get("hen_id").and_then(Value::as_str) == Some(hen));
    let request = match &existing {
        Some(p) if p.get("total_eggs") == doc.get("total_eggs")
            && p.get("quality_rate") == doc.get("quality_rate")
            && p.get("farm_id") == doc.get("farm_id") =>
        {
            return Ok(());
        }
        Some(p) => client.put(format!("{base}/hen_productivity/api/{}", id_of(p))),
        None => client.post(format!("{base}/hen_productivity/api")),
    };
    let resp = request
        .header(IDENTITY_HEADER, HEN_WORKER)
        .json(&doc)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("worker write answered {}", resp.status()))
    }
}
