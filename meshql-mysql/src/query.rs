use serde_json;

pub struct QueryPart {
    pub clause: String,
    pub values: Vec<String>,
}

/// Build a SQL WHERE clause fragment from a JSON query object.
///
/// Supported key patterns:
/// - `"id"` -> `` `id` = ? ``
/// - `"payload.field"` -> `JSON_UNQUOTE(JSON_EXTRACT(payload, '$.field')) = ?`
/// - Empty object `{}` -> empty clause (no filter)
pub fn build_where(query_obj: &serde_json::Map<String, serde_json::Value>) -> QueryPart {
    if query_obj.is_empty() {
        return QueryPart {
            clause: String::new(),
            values: Vec::new(),
        };
    }

    let mut conditions: Vec<String> = Vec::new();
    let mut values: Vec<String> = Vec::new();

    for (key, val) in query_obj {
        let str_val = match val {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };

        let condition = if key == "id" {
            "`id` = ?".to_string()
        } else if let Some(field) = key.strip_prefix("payload.") {
            format!("JSON_UNQUOTE(JSON_EXTRACT(payload, '$.{field}')) = ?")
        } else {
            // Unknown key -- try it as a top-level column
            format!("`{key}` = ?")
        };

        conditions.push(condition);
        values.push(str_val);
    }

    QueryPart {
        clause: conditions.join(" AND "),
        values,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_query_produces_no_clause() {
        let obj = serde_json::Map::new();
        let part = build_where(&obj);
        assert!(part.clause.is_empty());
        assert!(part.values.is_empty());
    }

    #[test]
    fn id_query_produces_id_condition() {
        let mut obj = serde_json::Map::new();
        obj.insert("id".to_string(), json!("abc-123"));
        let part = build_where(&obj);
        assert_eq!(part.clause, "`id` = ?");
        assert_eq!(part.values, vec!["abc-123"]);
    }

    #[test]
    fn payload_field_query_produces_json_extract() {
        let mut obj = serde_json::Map::new();
        obj.insert("payload.name".to_string(), json!("Alice"));
        let part = build_where(&obj);
        assert_eq!(
            part.clause,
            "JSON_UNQUOTE(JSON_EXTRACT(payload, '$.name')) = ?"
        );
        assert_eq!(part.values, vec!["Alice"]);
    }
}
