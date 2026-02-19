use bson::{Bson, Document, doc};
use chrono::DateTime;
use meshql_core::{Envelope, Stash};
use serde_json::{Map, Value};

pub fn stash_to_doc(stash: &Stash) -> Document {
    stash
        .iter()
        .map(|(k, v)| (k.clone(), json_to_bson(v)))
        .collect()
}

pub fn doc_to_stash(doc: &Document) -> Stash {
    doc.iter()
        .filter(|(k, _)| !k.starts_with('_'))
        .map(|(k, v)| (k.clone(), bson_to_json(v)))
        .collect()
}

pub fn envelope_to_document(env: &Envelope) -> Document {
    let tokens: Vec<Bson> = env
        .authorized_tokens
        .iter()
        .map(|s| Bson::String(s.clone()))
        .collect();
    doc! {
        "id": &env.id,
        "createdAt": bson::DateTime::from_chrono(env.created_at),
        "deleted": env.deleted,
        "authorizedTokens": tokens,
        "payload": stash_to_doc(&env.payload),
    }
}

pub fn document_to_envelope(doc: &Document) -> Option<Envelope> {
    let id = doc.get_str("id").ok()?.to_string();
    let created_at_bson = doc.get_datetime("createdAt").ok()?;
    let created_at: DateTime<chrono::Utc> = created_at_bson.to_chrono();
    let deleted = doc.get_bool("deleted").unwrap_or(false);
    let authorized_tokens = doc
        .get_array("authorizedTokens")
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|b| b.as_str().map(String::from))
        .collect();
    let payload_doc = doc.get_document("payload").ok()?;
    let payload = doc_to_stash(payload_doc);

    Some(Envelope {
        id,
        payload,
        created_at,
        deleted,
        authorized_tokens,
    })
}

/// Returns a Stash with payload fields + the top-level "id" merged in.
/// This is what the Searcher returns — a flat map ready for GraphQL resolvers.
pub fn document_to_result_stash(doc: &Document) -> Option<Stash> {
    let id = doc.get_str("id").ok()?.to_string();
    let payload_doc = doc.get_document("payload").ok()?;
    let mut stash = doc_to_stash(payload_doc);
    stash.insert("id".to_string(), Value::String(id));
    Some(stash)
}

pub fn json_to_bson(value: &Value) -> Bson {
    match value {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Bson::Int64(i)
            } else if let Some(f) = n.as_f64() {
                Bson::Double(f)
            } else {
                Bson::Null
            }
        }
        Value::String(s) => Bson::String(s.clone()),
        Value::Array(arr) => Bson::Array(arr.iter().map(json_to_bson).collect()),
        Value::Object(obj) => {
            let doc: Document = obj.iter().map(|(k, v)| (k.clone(), json_to_bson(v))).collect();
            Bson::Document(doc)
        }
    }
}

pub fn bson_to_json(bson: &Bson) -> Value {
    match bson {
        Bson::Null => Value::Null,
        Bson::Boolean(b) => Value::Bool(*b),
        Bson::Int32(i) => Value::Number((*i).into()),
        Bson::Int64(i) => Value::Number((*i).into()),
        Bson::Double(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Bson::String(s) => Value::String(s.clone()),
        Bson::Array(arr) => Value::Array(arr.iter().map(bson_to_json).collect()),
        Bson::Document(doc) => {
            let map: Map<String, Value> = doc
                .iter()
                .filter(|(k, _)| !k.starts_with('_'))
                .map(|(k, v)| (k.clone(), bson_to_json(v)))
                .collect();
            Value::Object(map)
        }
        Bson::DateTime(dt) => Value::Number(dt.timestamp_millis().into()),
        _ => Value::Null,
    }
}
