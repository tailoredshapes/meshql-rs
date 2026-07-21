/// A thin change notification: something about `entity`/`id` changed at
/// `created_at` (epoch millis, the store's commit time). `authorized_tokens`
/// ride along for per-subscriber filtering and are NEVER serialized to the
/// wire — see `wire_json`.
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub entity: String,
    pub id: String,
    pub created_at: i64,
    pub deleted: bool,
    pub authorized_tokens: Vec<String>,
}

#[derive(serde::Serialize)]
struct WireEvent<'a> {
    entity: &'a str,
    id: &'a str,
    created_at: i64,
    deleted: bool,
}

impl ChangeEvent {
    /// The SSE `data:` payload. Tokens are stripped by construction — the
    /// wire struct has no field for them.
    pub fn wire_json(&self) -> String {
        serde_json::to_string(&WireEvent {
            entity: &self.entity,
            id: &self.id,
            created_at: self.created_at,
            deleted: self.deleted,
        })
        .expect("WireEvent is always serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> ChangeEvent {
        ChangeEvent {
            entity: "hen".into(),
            id: "abc-123".into(),
            created_at: 1751892345123,
            deleted: false,
            authorized_tokens: vec!["secret-team".into()],
        }
    }

    #[test]
    fn wire_json_contains_the_thin_fields() {
        let v: serde_json::Value = serde_json::from_str(&event().wire_json()).unwrap();
        assert_eq!(v["entity"], "hen");
        assert_eq!(v["id"], "abc-123");
        assert_eq!(v["created_at"], 1751892345123i64);
        assert_eq!(v["deleted"], false);
    }

    #[test]
    fn wire_json_never_leaks_tokens() {
        let wire = event().wire_json();
        assert!(!wire.contains("secret-team"));
        assert!(!wire.contains("authorized_tokens"));
    }
}
