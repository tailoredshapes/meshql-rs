use serde_json;

pub struct QueryPart {
    pub clause: String,
    pub values: Vec<String>,
}

pub fn build_where(query_obj: &serde_json::Map<String, serde_json::Value>) -> QueryPart {
    if query_obj.is_empty() {
        return QueryPart {
            clause: String::new(),
            values: vec![],
        };
    }

    let mut clauses = Vec::new();
    let mut values = Vec::new();

    for (key, val) in query_obj {
        let str_val = match val {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };

        if key == "id" {
            clauses.push("id = ?".to_string());
        } else if let Some(field) = key.strip_prefix("payload.") {
            clauses.push(format!("json_extract(payload, '$.{}') = ?", field));
        } else {
            // Unknown key — skip
            continue;
        }

        values.push(str_val);
    }

    QueryPart {
        clause: clauses.join(" AND "),
        values,
    }
}
