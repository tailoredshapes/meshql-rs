use chrono::{DateTime, TimeZone, Utc};
use meshql_core::{Envelope, Stash};
use serde_json::{json, Value};

/// Flatten an Envelope into a flat JSON object for storage in merkql topic.
/// Uses `_` prefix for metadata fields to avoid collision with payload fields.
pub fn envelope_to_flat_json(envelope: &Envelope) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("_id".to_string(), json!(envelope.id));
    map.insert(
        "_created_at".to_string(),
        json!(envelope.created_at.timestamp_millis()),
    );
    map.insert("_deleted".to_string(), json!(envelope.deleted));
    map.insert(
        "_tokens".to_string(),
        json!(serde_json::to_string(&envelope.authorized_tokens).unwrap_or_default()),
    );

    // Flatten payload fields into top-level
    for (k, v) in &envelope.payload {
        map.insert(k.clone(), v.clone());
    }

    Value::Object(map)
}

/// Reconstruct an Envelope from a flat JSON record (as stored in the topic).
pub fn flat_json_to_envelope(json: &Value) -> Option<Envelope> {
    let obj = json.as_object()?;

    let id = obj.get("_id")?.as_str()?.to_string();
    let created_at_ms = obj.get("_created_at")?.as_i64()?;
    let deleted = obj.get("_deleted")?.as_bool().unwrap_or(false);
    let tokens_str = obj.get("_tokens").and_then(|v| v.as_str()).unwrap_or("[]");
    let authorized_tokens: Vec<String> = serde_json::from_str(tokens_str).unwrap_or_default();
    let created_at: DateTime<Utc> = Utc.timestamp_millis_opt(created_at_ms).single()?;

    // Extract payload: all fields except _prefixed metadata
    let mut payload = Stash::new();
    for (k, v) in obj {
        if !k.starts_with('_') {
            payload.insert(k.clone(), v.clone());
        }
    }

    Some(Envelope {
        id,
        payload,
        created_at,
        deleted,
        authorized_tokens,
    })
}

/// Convert an Envelope to a result Stash (payload fields + id merged in).
pub fn envelope_to_stash(env: &Envelope) -> Stash {
    let mut stash = env.payload.clone();
    stash.insert("id".to_string(), json!(env.id));
    stash
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn roundtrip_envelope() {
        let mut payload = Stash::new();
        payload.insert("name".to_string(), json!("test"));
        payload.insert("count".to_string(), json!(42));

        let env = Envelope {
            id: "test-id".to_string(),
            payload,
            created_at: Utc::now(),
            deleted: false,
            authorized_tokens: vec!["*".to_string()],
        };

        let flat = envelope_to_flat_json(&env);
        let restored = flat_json_to_envelope(&flat).unwrap();

        assert_eq!(restored.id, env.id);
        assert_eq!(restored.payload, env.payload);
        assert_eq!(restored.deleted, env.deleted);
        assert_eq!(restored.authorized_tokens, env.authorized_tokens);
        // Millisecond precision comparison
        assert_eq!(
            restored.created_at.timestamp_millis(),
            env.created_at.timestamp_millis()
        );
    }
}
