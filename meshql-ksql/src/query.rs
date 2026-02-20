/// A built WHERE clause for ksqlDB pull queries.
///
/// Unlike SQLite which uses bind params (`?`), ksqlDB pull queries
/// don't support bind params, so values are inlined with escaping.
pub struct QueryPart {
    pub clause: String,
}

/// Build a ksqlDB WHERE clause from a JSON query object.
///
/// Follows the same template convention as meshql-sqlite/query.rs:
/// - `"id"` → `id = 'escaped_value'`
/// - `"payload.field"` → `EXTRACTJSONFIELD(payload, '$.field') = 'escaped_value'`
/// - `{}` → empty (match all)
pub fn build_where(query_obj: &serde_json::Map<String, serde_json::Value>) -> QueryPart {
    if query_obj.is_empty() {
        return QueryPart {
            clause: String::new(),
        };
    }

    let mut clauses = Vec::new();

    for (key, val) in query_obj {
        let str_val = match val {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let escaped = escape_sql_string(&str_val);

        if key == "id" {
            clauses.push(format!("id = '{}'", escaped));
        } else if let Some(field) = key.strip_prefix("payload.") {
            clauses.push(format!(
                "EXTRACTJSONFIELD(payload, '$.{}') = '{}'",
                field, escaped
            ));
        } else {
            // Unknown key — skip
            continue;
        }
    }

    QueryPart {
        clause: clauses.join(" AND "),
    }
}

/// Escape single quotes for ksqlDB SQL strings.
fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_empty_query() {
        let obj = serde_json::Map::new();
        let result = build_where(&obj);
        assert!(result.clause.is_empty());
    }

    #[test]
    fn test_id_query() {
        let mut obj = serde_json::Map::new();
        obj.insert("id".to_string(), json!("abc-123"));
        let result = build_where(&obj);
        assert_eq!(result.clause, "id = 'abc-123'");
    }

    #[test]
    fn test_payload_field_query() {
        let mut obj = serde_json::Map::new();
        obj.insert("payload.name".to_string(), json!("Alice"));
        let result = build_where(&obj);
        assert_eq!(
            result.clause,
            "EXTRACTJSONFIELD(payload, '$.name') = 'Alice'"
        );
    }

    #[test]
    fn test_combined_query() {
        let mut obj = serde_json::Map::new();
        // Use a BTreeMap-backed approach to get deterministic ordering
        obj.insert("id".to_string(), json!("test-id"));
        obj.insert("payload.type".to_string(), json!("typeA"));
        let result = build_where(&obj);
        // Both clauses should be present (order may vary)
        assert!(result.clause.contains("id = 'test-id'"));
        assert!(result
            .clause
            .contains("EXTRACTJSONFIELD(payload, '$.type') = 'typeA'"));
        assert!(result.clause.contains(" AND "));
    }

    #[test]
    fn test_sql_injection_prevention() {
        let mut obj = serde_json::Map::new();
        obj.insert("id".to_string(), json!("'; DROP TABLE foo; --"));
        let result = build_where(&obj);
        assert_eq!(result.clause, "id = '''; DROP TABLE foo; --'");
    }

    #[test]
    fn test_numeric_value() {
        let mut obj = serde_json::Map::new();
        obj.insert("payload.count".to_string(), json!(42));
        let result = build_where(&obj);
        assert_eq!(result.clause, "EXTRACTJSONFIELD(payload, '$.count') = '42'");
    }

    #[test]
    fn test_unknown_key_skipped() {
        let mut obj = serde_json::Map::new();
        obj.insert("unknown_field".to_string(), json!("value"));
        let result = build_where(&obj);
        assert!(result.clause.is_empty());
    }
}
