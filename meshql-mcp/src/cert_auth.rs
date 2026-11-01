//! Fixture plugins: no auth, non-token ownership, and refusing reads.
use meshql_core::{Auth, AuthContext, AuthMark, Envelope, Operation, Session};
use std::sync::Arc;
pub(crate) struct FixtureAuth {
    pub mode: String,
}
struct FixtureSession {
    mode: String,
    user: Option<String>,
}
impl Auth for FixtureAuth {
    fn authenticate(&self, context: &AuthContext) -> Arc<dyn Session> {
        Arc::new(FixtureSession {
            mode: self.mode.clone(),
            user: context
                .stash()
                .get("user")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        })
    }
}
impl FixtureSession {
    fn owns(&self, e: &Envelope) -> bool {
        self.user
            .as_deref()
            .is_some_and(|u| e.payload.get("owner").and_then(serde_json::Value::as_str) == Some(u))
    }
}
impl Session for FixtureSession {
    fn stamp(&self, mut e: Envelope) -> Envelope {
        e.auth = AuthMark::empty();
        if self.mode == "owner" {
            if let Some(u) = &self.user {
                e.payload.insert("owner".into(), u.clone().into());
            }
        }
        e
    }
    fn is_authorized(&self, operation: Operation, e: &Envelope) -> bool {
        match self.mode.as_str() {
            "refuse-reads" => operation != Operation::Read,
            "owner" => match operation {
                Operation::Create => {
                    self.user.is_some() && (e.payload.get("owner").is_none() || self.owns(e))
                }
                _ => self.owns(e),
            },
            _ => true,
        }
    }
    fn is_principal(&self, name: &str) -> bool {
        self.user.as_deref() == Some(name)
    }
}
