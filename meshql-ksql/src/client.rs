use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::{debug, error, warn};

use crate::config::KsqlConfig;

/// HTTP client for Confluent Cloud Kafka REST API v3 and ksqlDB REST API.
#[derive(Clone)]
pub struct ConfluentClient {
    http: Client,
    kafka_rest_url: String,
    kafka_cluster_id: String,
    kafka_auth: String,
    ksqldb_url: String,
    ksqldb_auth: String,
}

impl ConfluentClient {
    pub fn new(config: &KsqlConfig) -> Self {
        let kafka_auth = BASE64.encode(format!(
            "{}:{}",
            config.kafka_api_key, config.kafka_api_secret
        ));
        let ksqldb_auth = BASE64.encode(format!(
            "{}:{}",
            config.ksqldb_api_key, config.ksqldb_api_secret
        ));

        let kafka_rest_url = config.kafka_rest_url.trim_end_matches('/').to_string();
        let ksqldb_url = config.ksqldb_url.trim_end_matches('/').to_string();

        Self {
            http: Client::new(),
            kafka_rest_url,
            kafka_cluster_id: config.kafka_cluster_id.clone(),
            kafka_auth,
            ksqldb_url,
            ksqldb_auth,
        }
    }

    /// Produce a record to a Kafka topic via REST API v3.
    pub async fn produce_record(
        &self,
        topic: &str,
        key: &str,
        value: &Value,
    ) -> anyhow::Result<()> {
        let url = format!(
            "{}/kafka/v3/clusters/{}/topics/{}/records",
            self.kafka_rest_url, self.kafka_cluster_id, topic
        );

        let body = json!({
            "key": { "type": "STRING", "data": key },
            "value": { "type": "JSON", "data": value },
        });

        debug!("Producing to {}: key={}", topic, key);

        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Basic {}", self.kafka_auth))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            error!("Kafka REST produce failed ({}): {}", status, body_text);
            anyhow::bail!("Kafka REST produce failed ({}): {}", status, body_text);
        }

        Ok(())
    }

    /// Execute a ksqlDB DDL statement (CREATE STREAM, CREATE TABLE, etc.).
    pub async fn execute_statement(&self, ksql: &str) -> anyhow::Result<()> {
        let url = format!("{}/ksql", self.ksqldb_url);

        let body = json!({
            "ksql": ksql,
            "streamsProperties": {}
        });

        debug!("Executing ksqlDB statement: {}", ksql);

        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/vnd.ksql.v1+json")
            .header("Accept", "application/vnd.ksql.v1+json")
            .header("Authorization", format!("Basic {}", self.ksqldb_auth))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if status.as_u16() >= 400 {
            let body_text = resp.text().await.unwrap_or_default();
            error!("ksqlDB statement failed ({}): {}", status, body_text);
            anyhow::bail!("ksqlDB statement failed ({}): {}", status, body_text);
        }

        debug!("ksqlDB statement succeeded: {}", status);
        Ok(())
    }

    /// Execute a ksqlDB pull query, returning parsed rows.
    pub async fn pull_query(&self, ksql: &str) -> anyhow::Result<Vec<HashMap<String, Value>>> {
        let url = format!("{}/query", self.ksqldb_url);

        let body = json!({
            "ksql": ksql,
            "streamsProperties": {}
        });

        debug!("Executing ksqlDB query: {}", ksql);

        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/vnd.ksql.v1+json")
            .header("Accept", "application/vnd.ksql.v1+json")
            .header("Authorization", format!("Basic {}", self.ksqldb_auth))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if status.as_u16() >= 400 {
            let body_text = resp.text().await.unwrap_or_default();
            // Return empty for "not ready yet" errors — caller can retry
            if body_text.contains("not available yet")
                || body_text.contains("Cannot determine which host")
            {
                debug!("ksqlDB query not ready yet: {}", body_text);
                return Ok(Vec::new());
            }
            error!("ksqlDB query failed ({}): {}", status, body_text);
            anyhow::bail!("ksqlDB query failed ({}): {}", status, body_text);
        }

        let body_text = resp.text().await?;
        parse_query_response(&body_text)
    }

    /// Check if a ksqlDB table is ready for pull queries.
    pub async fn is_table_ready(&self, table_name: &str) -> bool {
        let ksql = format!("SELECT * FROM {} LIMIT 1;", table_name);
        match self.pull_query(&ksql).await {
            Ok(_) => true,
            Err(e) => {
                warn!("Table {} not ready: {}", table_name, e);
                false
            }
        }
    }
}

/// Parse ksqlDB query response format:
/// `[{header: {schema: ...}}, {row: {columns: [...]}}, ..., {finalMessage: ...}]`
fn parse_query_response(body: &str) -> anyhow::Result<Vec<HashMap<String, Value>>> {
    let raw: Vec<Value> = serde_json::from_str(body)?;

    if raw.is_empty() {
        return Ok(Vec::new());
    }

    // First element has the header with schema
    let header = raw[0]
        .get("header")
        .ok_or_else(|| anyhow::anyhow!("missing header in ksqlDB response"))?;
    let schema = header.get("schema").and_then(|s| s.as_str()).unwrap_or("");
    let column_names = parse_schema_columns(schema);

    let mut results = Vec::new();
    for item in raw.iter().skip(1) {
        if item.get("finalMessage").is_some() {
            break;
        }
        if let Some(row) = item.get("row") {
            if let Some(columns) = row.get("columns").and_then(|c| c.as_array()) {
                let mut row_map = HashMap::new();
                for (i, name) in column_names.iter().enumerate() {
                    if let Some(val) = columns.get(i) {
                        row_map.insert(name.clone(), val.clone());
                    }
                }
                results.push(row_map);
            }
        }
    }

    Ok(results)
}

/// Parse column names from ksqlDB schema string.
/// Format: "`ID` STRING KEY, `PAYLOAD` STRING, `CREATED_AT` BIGINT, ..."
fn parse_schema_columns(schema: &str) -> Vec<String> {
    if schema.is_empty() {
        return Vec::new();
    }

    schema
        .split(',')
        .filter_map(|part| {
            let trimmed = part.trim();
            let start = trimmed.find('`')?;
            let end = trimmed[start + 1..].find('`')?;
            Some(trimmed[start + 1..start + 1 + end].to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_schema_columns() {
        let schema = "`ID` STRING KEY, `PAYLOAD` STRING, `CREATED_AT` BIGINT, `DELETED` BOOLEAN, `AUTHORIZED_TOKENS` STRING";
        let cols = parse_schema_columns(schema);
        assert_eq!(
            cols,
            vec![
                "ID",
                "PAYLOAD",
                "CREATED_AT",
                "DELETED",
                "AUTHORIZED_TOKENS"
            ]
        );
    }

    #[test]
    fn test_parse_schema_columns_empty() {
        assert!(parse_schema_columns("").is_empty());
    }

    #[test]
    fn test_parse_query_response_with_rows() {
        let body = r#"[
            {"header": {"queryId": "q1", "schema": "`ID` STRING KEY, `PAYLOAD` STRING, `CREATED_AT` BIGINT"}},
            {"row": {"columns": ["abc-123", "{\"name\":\"Alice\"}", 1640000000000]}},
            {"row": {"columns": ["def-456", "{\"name\":\"Bob\"}", 1640000001000]}},
            {"finalMessage": "Limit Reached"}
        ]"#;

        let rows = parse_query_response(body).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["ID"], json!("abc-123"));
        assert_eq!(rows[0]["PAYLOAD"], json!("{\"name\":\"Alice\"}"));
        assert_eq!(rows[0]["CREATED_AT"], json!(1640000000000i64));
        assert_eq!(rows[1]["ID"], json!("def-456"));
    }

    #[test]
    fn test_parse_query_response_empty() {
        let body = r#"[
            {"header": {"queryId": "q1", "schema": "`ID` STRING KEY"}},
            {"finalMessage": "Limit Reached"}
        ]"#;
        let rows = parse_query_response(body).unwrap();
        assert!(rows.is_empty());
    }
}
