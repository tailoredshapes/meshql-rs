use serde_json::Value;

/// Match a record's JSON representation against a dot-notation query.
///
/// The query is a JSON object where:
/// - Keys may use dot notation for nested fields (e.g., "payload.name")
/// - For flat storage, "payload.X" is mapped to just "X" at the top level
/// - Values must equal the record's value at that path
/// - An empty query `{}` matches everything
/// - All query keys must match (AND semantics)
pub fn matches(record_json: &Value, query: &Value) -> bool {
    let query_obj = match query.as_object() {
        Some(o) => o,
        None => return false,
    };

    for (key, expected) in query_obj {
        // Strip "payload." prefix since we store flat
        let lookup_key = key.strip_prefix("payload.").unwrap_or(key);

        // Map "id" to "_id" for our flat storage format
        let actual_key = if lookup_key == "id" {
            "_id"
        } else {
            lookup_key
        };

        match record_json.get(actual_key) {
            Some(actual) => {
                if actual != expected {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_query_matches_all() {
        let record = json!({"_id": "x", "name": "foo"});
        assert!(matches(&record, &json!({})));
    }

    #[test]
    fn id_match() {
        let record = json!({"_id": "s-id-1", "name": "alpha"});
        assert!(matches(&record, &json!({"id": "s-id-1"})));
        assert!(!matches(&record, &json!({"id": "s-id-2"})));
    }

    #[test]
    fn payload_dot_notation_match() {
        let record = json!({"_id": "x", "name": "beta", "type": "typeB"});
        assert!(matches(&record, &json!({"payload.name": "beta"})));
        assert!(!matches(&record, &json!({"payload.name": "gamma"})));
    }

    #[test]
    fn multi_condition() {
        let record = json!({"_id": "x", "name": "delta", "type": "typeB"});
        assert!(matches(
            &record,
            &json!({"payload.name": "delta", "payload.type": "typeB"})
        ));
        assert!(!matches(
            &record,
            &json!({"payload.name": "delta", "payload.type": "typeA"})
        ));
    }
}
