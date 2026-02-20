use chrono::{DateTime, TimeZone, Utc};
use meshql_core::{Envelope, Stash};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Convert an Envelope to the Kafka JSON value format (double-encoded payload).
///
/// Matches the Java Converters.envelopeToJson format:
/// ```json
/// {
///   "payload": "{\"name\":\"Alice\",\"age\":30}",
///   "created_at": 1640000000000,
///   "deleted": false,
///   "authorized_tokens": "[\"*\"]"
/// }
/// ```
pub fn envelope_to_kafka_value(envelope: &Envelope) -> Value {
    let payload_str = serde_json::to_string(&envelope.payload).unwrap_or_else(|_| "{}".to_string());
    let tokens_str =
        serde_json::to_string(&envelope.authorized_tokens).unwrap_or_else(|_| "[]".to_string());

    json!({
        "payload": payload_str,
        "created_at": envelope.created_at.timestamp_millis(),
        "deleted": envelope.deleted,
        "authorized_tokens": tokens_str,
    })
}

/// Convert a ksqlDB row (HashMap from pull query) back to an Envelope.
///
/// Column names may be uppercase (ksqlDB convention) or lowercase.
pub fn row_to_envelope(row: &HashMap<String, Value>) -> anyhow::Result<Envelope> {
    let id = get_string_field(row, "ID", "id")
        .ok_or_else(|| anyhow::anyhow!("missing id field in row"))?;

    let payload_str = get_string_field(row, "PAYLOAD", "payload").unwrap_or_default();
    let payload: Stash = if payload_str.is_empty() {
        Map::new()
    } else {
        serde_json::from_str(&payload_str)?
    };

    let created_at_millis = get_i64_field(row, "CREATED_AT", "created_at").unwrap_or(0);
    let created_at: DateTime<Utc> = Utc
        .timestamp_millis_opt(created_at_millis)
        .single()
        .unwrap_or_else(Utc::now);

    let deleted = get_bool_field(row, "DELETED", "deleted").unwrap_or(false);

    let tokens_str = get_string_field(row, "AUTHORIZED_TOKENS", "authorized_tokens")
        .unwrap_or_else(|| "[]".to_string());
    let authorized_tokens: Vec<String> =
        serde_json::from_str(&tokens_str).unwrap_or_else(|_| Vec::new());

    Ok(Envelope {
        id,
        payload,
        created_at,
        deleted,
        authorized_tokens,
    })
}

/// Convert an Envelope to a result Stash (payload fields + id merged in).
pub fn envelope_to_stash(envelope: &Envelope) -> Stash {
    let mut stash = envelope.payload.clone();
    stash.insert("id".to_string(), json!(envelope.id));
    stash
}

fn get_field<'a>(row: &'a HashMap<String, Value>, upper: &str, lower: &str) -> Option<&'a Value> {
    row.get(upper).or_else(|| row.get(lower))
}

fn get_string_field(row: &HashMap<String, Value>, upper: &str, lower: &str) -> Option<String> {
    get_field(row, upper, lower).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    })
}

fn get_i64_field(row: &HashMap<String, Value>, upper: &str, lower: &str) -> Option<i64> {
    get_field(row, upper, lower).and_then(|v| v.as_i64())
}

fn get_bool_field(row: &HashMap<String, Value>, upper: &str, lower: &str) -> Option<bool> {
    get_field(row, upper, lower).and_then(|v| v.as_bool())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_envelope_to_kafka_value_roundtrip() {
        let mut payload = Map::new();
        payload.insert("name".to_string(), json!("Alice"));
        payload.insert("age".to_string(), json!(30));

        let created_at = Utc.timestamp_millis_opt(1640000000000).unwrap();
        let envelope = Envelope {
            id: "test-id".to_string(),
            payload,
            created_at,
            deleted: false,
            authorized_tokens: vec!["*".to_string()],
        };

        let kafka_val = envelope_to_kafka_value(&envelope);

        // Payload should be double-encoded (a JSON string, not an object)
        assert!(kafka_val["payload"].is_string());
        let payload_str = kafka_val["payload"].as_str().unwrap();
        let parsed_payload: Map<String, Value> = serde_json::from_str(payload_str).unwrap();
        assert_eq!(parsed_payload["name"], json!("Alice"));
        assert_eq!(parsed_payload["age"], json!(30));

        assert_eq!(kafka_val["created_at"], json!(1640000000000i64));
        assert_eq!(kafka_val["deleted"], json!(false));

        // authorized_tokens should be double-encoded
        assert!(kafka_val["authorized_tokens"].is_string());
        let tokens_str = kafka_val["authorized_tokens"].as_str().unwrap();
        let parsed_tokens: Vec<String> = serde_json::from_str(tokens_str).unwrap();
        assert_eq!(parsed_tokens, vec!["*"]);
    }

    #[test]
    fn test_row_to_envelope_uppercase() {
        let mut row = HashMap::new();
        row.insert("ID".to_string(), json!("abc-123"));
        row.insert("PAYLOAD".to_string(), json!("{\"name\":\"Bob\"}"));
        row.insert("CREATED_AT".to_string(), json!(1640000000000i64));
        row.insert("DELETED".to_string(), json!(false));
        row.insert("AUTHORIZED_TOKENS".to_string(), json!("[\"*\"]"));

        let env = row_to_envelope(&row).unwrap();
        assert_eq!(env.id, "abc-123");
        assert_eq!(env.payload["name"], json!("Bob"));
        assert_eq!(env.created_at.timestamp_millis(), 1640000000000);
        assert!(!env.deleted);
        assert_eq!(env.authorized_tokens, vec!["*"]);
    }

    #[test]
    fn test_row_to_envelope_lowercase() {
        let mut row = HashMap::new();
        row.insert("id".to_string(), json!("def-456"));
        row.insert("payload".to_string(), json!("{\"type\":\"A\"}"));
        row.insert("created_at".to_string(), json!(1640000001000i64));
        row.insert("deleted".to_string(), json!(true));
        row.insert("authorized_tokens".to_string(), json!("[\"admin\"]"));

        let env = row_to_envelope(&row).unwrap();
        assert_eq!(env.id, "def-456");
        assert_eq!(env.payload["type"], json!("A"));
        assert!(env.deleted);
        assert_eq!(env.authorized_tokens, vec!["admin"]);
    }

    #[test]
    fn test_envelope_to_stash() {
        let mut payload = Map::new();
        payload.insert("name".to_string(), json!("Charlie"));
        let env = Envelope::new("stash-id", payload, vec!["*".to_string()]);

        let stash = envelope_to_stash(&env);
        assert_eq!(stash["id"], json!("stash-id"));
        assert_eq!(stash["name"], json!("Charlie"));
    }

    #[test]
    fn test_empty_payload() {
        let mut row = HashMap::new();
        row.insert("ID".to_string(), json!("empty-id"));
        row.insert("PAYLOAD".to_string(), json!("{}"));
        row.insert("CREATED_AT".to_string(), json!(0));
        row.insert("DELETED".to_string(), json!(false));
        row.insert("AUTHORIZED_TOKENS".to_string(), json!("[]"));

        let env = row_to_envelope(&row).unwrap();
        assert_eq!(env.id, "empty-id");
        assert!(env.payload.is_empty());
        assert!(env.authorized_tokens.is_empty());
    }
}
