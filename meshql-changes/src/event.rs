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
    /// Filtering input only — NEVER serialized. See `wire_json`.
    pub authorized_tokens: Vec<String>,
    /// Resume cursor, `"{partition}:{offset}"`. `Some` only for sources that
    /// can seek (merkql); `None` for tail-based sources, which don't resume.
    pub cursor: Option<String>,
    /// The changed payload, when the streamlette is configured to carry it.
    ///
    /// **This must be the Envelope's `payload`, never the whole Envelope.**
    /// An Envelope carries `authorized_tokens`; this field is serialized
    /// verbatim, so handing it a whole Envelope puts tokens on the wire and
    /// defeats the `WireEvent` split entirely. The split protects the
    /// top-level field only — it cannot police what a producer nests inside.
    ///
    /// Only sound on log-backed (merkql) sources. `SearcherTail` cannot see
    /// a token-only ACL change and retains stale tokens until the next
    /// payload change or delete, so a tail-sourced payload may be visible to
    /// a subscriber who should no longer see it.
    pub payload: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct WireEvent<'a> {
    entity: &'a str,
    id: &'a str,
    created_at: i64,
    deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<&'a serde_json::Value>,
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
            cursor: self.cursor.as_deref(),
            payload: self.payload.as_ref(),
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
            cursor: None,
            payload: None,
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

    #[test]
    fn wire_json_includes_cursor_and_payload_when_present() {
        let mut e = event();
        e.cursor = Some("0:42".into());
        e.payload = Some(serde_json::json!({"eggs": 3}));
        let v: serde_json::Value = serde_json::from_str(&e.wire_json()).unwrap();
        assert_eq!(v["cursor"], "0:42");
        assert_eq!(v["payload"]["eggs"], 3);
    }

    #[test]
    fn wire_json_omits_cursor_and_payload_when_absent() {
        let v: serde_json::Value = serde_json::from_str(&event().wire_json()).unwrap();
        assert!(
            v.get("cursor").is_none(),
            "absent cursor must be omitted, not null"
        );
        assert!(
            v.get("payload").is_none(),
            "absent payload must be omitted, not null"
        );
    }

    #[test]
    fn wire_json_never_leaks_tokens_even_with_payload() {
        let mut e = event();
        e.payload = Some(serde_json::json!({"note": "fine"}));
        let wire = e.wire_json();
        assert!(!wire.contains("secret-team"));
        assert!(!wire.contains("authorized_tokens"));
    }
}
