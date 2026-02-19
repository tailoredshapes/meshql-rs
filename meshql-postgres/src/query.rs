pub struct QueryPart {
    pub clause: String,
    pub values: Vec<String>,
}

/// Build a SQL WHERE clause fragment and bind values for PostgreSQL.
///
/// `start_param` is the `$N` index of the first dynamic parameter (e.g. 2 if
/// `$1` is already used for `cutoff_ms`).
pub fn build_where(
    query_obj: &serde_json::Map<String, serde_json::Value>,
    start_param: usize,
) -> QueryPart {
    if query_obj.is_empty() {
        return QueryPart {
            clause: String::new(),
            values: vec![],
        };
    }

    let mut clauses = Vec::new();
    let mut values = Vec::new();
    let mut idx = start_param;

    for (key, val) in query_obj {
        let str_val = match val {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };

        if key == "id" {
            clauses.push(format!("id = ${}", idx));
        } else if let Some(field) = key.strip_prefix("payload.") {
            clauses.push(format!("(payload::jsonb)->>'{}' = ${}", field, idx));
        } else {
            // Unknown key — skip
            continue;
        }

        values.push(str_val);
        idx += 1;
    }

    QueryPart {
        clause: clauses.join(" AND "),
        values,
    }
}
