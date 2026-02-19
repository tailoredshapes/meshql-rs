use serde_json::Value;

/// Match a record's JSON representation against a dot-notation query.
///
/// The query is a JSON object where:
/// - Keys may use dot notation for nested fields (e.g., "payload.name")
/// - Values must equal the record's value at that path
/// - An empty query `{}` matches everything
/// - All query keys must match (AND semantics)
pub fn matches(record_json: &Value, query: &Value) -> bool {
    let query_obj = match query.as_object() {
        Some(o) => o,
        None => return false,
    };

    for (key, expected) in query_obj {
        let path: Vec<&str> = key.split('.').collect();
        match get_path(record_json, &path) {
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

fn get_path<'a>(val: &'a Value, path: &[&str]) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(val);
    }
    match val {
        Value::Object(obj) => {
            let head = path[0];
            let rest = &path[1..];
            obj.get(head).and_then(|child| get_path(child, rest))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_query_matches_all() {
        let record = json!({"id": "x", "payload": {"name": "foo"}});
        assert!(matches(&record, &json!({})));
    }

    #[test]
    fn simple_id_match() {
        let record = json!({"id": "s-id-1", "payload": {"name": "alpha"}});
        assert!(matches(&record, &json!({"id": "s-id-1"})));
        assert!(!matches(&record, &json!({"id": "s-id-2"})));
    }

    #[test]
    fn dot_notation_match() {
        let record = json!({"id": "x", "payload": {"name": "beta", "type": "typeB"}});
        assert!(matches(&record, &json!({"payload.name": "beta"})));
        assert!(!matches(&record, &json!({"payload.name": "gamma"})));
    }

    #[test]
    fn multi_condition_match() {
        let record = json!({"id": "x", "payload": {"name": "delta", "type": "typeB"}});
        assert!(matches(
            &record,
            &json!({"payload.name": "delta", "payload.type": "typeB"})
        ));
        assert!(!matches(
            &record,
            &json!({"payload.name": "delta", "payload.type": "typeA"})
        ));
    }

    #[test]
    fn searcher_context_match() {
        let record_json = json!({
            "id": "s-id-2",
            "payload": {"name": "beta", "count": 20, "type": "typeB"}
        });
        let query = json!({"payload.name": "beta"});
        assert!(matches(&record_json, &query), "findByName should work");

        let record_json2 = json!({
            "id": "s-id-1",
            "payload": {"name": "alpha", "count": 10, "type": "typeA"}
        });
        let query2 = json!({"payload.type": "typeA"});
        assert!(matches(&record_json2, &query2), "findAllByType should work");
    }
}
