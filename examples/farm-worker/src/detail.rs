//! Detail lookup: given a thin ChangeEvent's id, fetch the full lay_report
//! record via GraphQL — "the same 'thin notification -> query for detail ->
//! react' shape an SSE-consuming FE client is designed for" per the pipeline
//! spec.

use crate::config::QueryDialect;
use crate::graphql::graphql_query;
use anyhow::{anyhow, Context};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct LayReport {
    #[serde(rename = "henId")]
    pub hen_id: String,
    pub eggs: i64,
    #[serde(rename = "timeOfDay")]
    pub time_of_day: String,
}

/// Fetch the full lay_report detail for `id`, as of `at` (epoch millis — the
/// ChangeEvent's commit time), from the source farm's lay_report graphlette.
/// The field shape (`henId`/`eggs`/`timeOfDay`) is identical across all
/// three retrofits, but the QUERY NAME is not — `dialect` picks between
/// Rust's `getLayReport` and Java/TS's `getById`. See the reconciliation
/// note at the top of this plan.
pub async fn fetch_lay_report(
    client: &reqwest::Client,
    graphql_base: &str,
    id: &str,
    at: i64,
    dialect: QueryDialect,
) -> anyhow::Result<LayReport> {
    let url = format!("{}/lay_report/graph", graphql_base.trim_end_matches('/'));
    let query_name = dialect.lay_report_by_id();
    let query =
        format!(r#"{{ {query_name}(id: "{id}", at: {at}) {{ henId eggs timeOfDay }} }}"#);
    let data = graphql_query(client, &url, &query, None).await?;
    let report = data
        .get(query_name)
        .filter(|v| !v.is_null())
        .ok_or_else(|| {
            anyhow!(
                "{query_name}({id}) at {at} returned null — detail not yet visible or id unknown"
            )
        })?;
    serde_json::from_value(report.clone())
        .context("lay_report detail response did not match the assumed LayReport shape")
}

/// Fetch every eggs count currently on record for `hen_id`, as of `at` —
/// the input to `productivity::recompute`'s `report_eggs` parameter. Fresh
/// on every call, never cached: this is what makes the worker's fold
/// idempotent under redelivery (see the reconciliation note at the top of
/// this plan and `productivity::recompute`'s doc comment) instead of
/// relying on a stored dedup ledger. Only `eggs` is deserialized — the
/// hen's identity and the rest of each report's fields aren't needed here.
pub async fn fetch_lay_reports_for_hen(
    client: &reqwest::Client,
    graphql_base: &str,
    hen_id: &str,
    at: i64,
    dialect: QueryDialect,
) -> anyhow::Result<Vec<i64>> {
    #[derive(Deserialize)]
    struct EggsOnly {
        eggs: i64,
    }

    let url = format!("{}/lay_report/graph", graphql_base.trim_end_matches('/'));
    let query_name = dialect.lay_reports_by_hen();
    let query = format!(r#"{{ {query_name}(id: "{hen_id}", at: {at}) {{ eggs }} }}"#);
    let data = graphql_query(client, &url, &query, None).await?;
    let list = data
        .get(query_name)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    list.into_iter()
        .map(|v| {
            serde_json::from_value::<EggsOnly>(v)
                .map(|r| r.eggs)
                .context("lay_report list response did not match the assumed {eggs} shape")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{json, Value};

    async fn echo_getlayreport(Json(body): Json<Value>) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(query.contains("getLayReport"), "unexpected query: {query}");
        assert!(query.contains(r#""lr-1""#), "query missing expected id: {query}");
        assert!(query.contains("at: 1000"), "query missing expected at: {query}");
        Json(json!({
            "data": {
                "getLayReport": {
                    "henId": "hen-1",
                    "eggs": 3,
                    "timeOfDay": "morning"
                }
            }
        }))
    }

    async fn echo_getbyid(Json(body): Json<Value>) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(query.contains("getById"), "unexpected query: {query}");
        Json(json!({
            "data": {
                "getById": {
                    "henId": "hen-1",
                    "eggs": 3,
                    "timeOfDay": "morning"
                }
            }
        }))
    }

    async fn echo_null() -> Json<Value> {
        Json(json!({ "data": { "getLayReport": null } }))
    }

    async fn start(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn fetch_lay_report_parses_a_successful_response_entity_named() {
        let base = start(Router::new().route("/lay_report/graph", post(echo_getlayreport))).await;
        let client = reqwest::Client::new();

        let report = fetch_lay_report(&client, &base, "lr-1", 1000, QueryDialect::EntityNamed)
            .await
            .unwrap();
        assert_eq!(report.hen_id, "hen-1");
        assert_eq!(report.eggs, 3);
        assert_eq!(report.time_of_day, "morning");
    }

    #[tokio::test]
    async fn fetch_lay_report_parses_a_successful_response_generic() {
        // Proves the SAME function, given QueryDialect::Generic, talks the
        // dialect Java's and TS's farm retrofits actually landed
        // (getById(id, at)) instead of Rust's getLayReport — see the
        // reconciliation note at the top of this plan.
        let base = start(Router::new().route("/lay_report/graph", post(echo_getbyid))).await;
        let client = reqwest::Client::new();

        let report = fetch_lay_report(&client, &base, "lr-1", 1000, QueryDialect::Generic)
            .await
            .unwrap();
        assert_eq!(report.hen_id, "hen-1");
        assert_eq!(report.eggs, 3);
    }

    #[tokio::test]
    async fn fetch_lay_report_errors_on_null_result() {
        let base = start(Router::new().route("/lay_report/graph", post(echo_null))).await;
        let client = reqwest::Client::new();

        let err = fetch_lay_report(&client, &base, "missing-id", 1000, QueryDialect::EntityNamed)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing-id"));
    }

    async fn echo_getlayreportsbyhen(Json(body): Json<Value>) -> Json<Value> {
        let query = body["query"].as_str().unwrap_or_default();
        assert!(query.contains("getLayReportsByHen"), "unexpected query: {query}");
        assert!(query.contains(r#""hen-1""#), "query missing expected hen id: {query}");
        Json(json!({
            "data": {
                "getLayReportsByHen": [
                    { "eggs": 3 },
                    { "eggs": 2 }
                ]
            }
        }))
    }

    async fn echo_getbyhen_empty(Json(_body): Json<Value>) -> Json<Value> {
        Json(json!({ "data": { "getByHen": [] } }))
    }

    #[tokio::test]
    async fn fetch_lay_reports_for_hen_sums_every_report_currently_on_record() {
        let base =
            start(Router::new().route("/lay_report/graph", post(echo_getlayreportsbyhen))).await;
        let client = reqwest::Client::new();

        let eggs = fetch_lay_reports_for_hen(&client, &base, "hen-1", 1000, QueryDialect::EntityNamed)
            .await
            .unwrap();
        assert_eq!(eggs, vec![3, 2]);
    }

    #[tokio::test]
    async fn fetch_lay_reports_for_hen_returns_empty_for_a_hen_with_no_reports() {
        let base = start(Router::new().route("/lay_report/graph", post(echo_getbyhen_empty))).await;
        let client = reqwest::Client::new();

        let eggs = fetch_lay_reports_for_hen(&client, &base, "hen-1", 1000, QueryDialect::Generic)
            .await
            .unwrap();
        assert_eq!(eggs, Vec::<i64>::new());
    }
}
