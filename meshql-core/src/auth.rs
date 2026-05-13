use crate::{Envelope, Stash};

pub trait Auth: Send + Sync {
    fn get_auth_token(&self, context: &Stash) -> Vec<String>;
    fn is_authorized(&self, credentials: &[String], envelope: &Envelope) -> bool;
}

/// Request-scoped auth context: a Stash populated by edge middleware from
/// trusted identity headers. Both `meshql-restlette` and `meshql-graphlette`
/// read this from axum request extensions and feed it to the configured
/// `Auth::get_auth_token`.
#[derive(Clone, Debug, Default)]
pub struct AuthContext(pub Stash);

impl AuthContext {
    pub fn new(stash: Stash) -> Self {
        Self(stash)
    }

    pub fn into_stash(self) -> Stash {
        self.0
    }
}

/// Default visibility predicate used by Repository implementations.
///
/// An envelope is visible to a caller iff any of:
///   - the envelope has no `authorized_tokens` (treated as public),
///   - the caller holds the wildcard token `"*"` (system / NoAuth caller),
///   - the envelope is tagged with `"*"` (everyone with any credential),
///   - any of the caller's tokens appears in the envelope's
///     `authorized_tokens`.
///
/// The token-overlap rule mirrors the canonical Java implementations
/// (`RDBMSSearcher`, `MongoSearcher`, `InMemorySearcher`) which apply the
/// same overlap via `Authorizer::isAuthorized`. The `"*"` wildcard is a
/// meshql-rs convention preserved from `NoAuth`, used for internal /
/// system reads where no caller identity is available.
pub fn envelope_visible_to(envelope: &Envelope, tokens: &[String]) -> bool {
    if envelope.authorized_tokens.is_empty() {
        return true;
    }
    if tokens.iter().any(|t| t == "*") {
        return true;
    }
    if envelope.authorized_tokens.iter().any(|t| t == "*") {
        return true;
    }
    envelope
        .authorized_tokens
        .iter()
        .any(|t| tokens.iter().any(|c| c == t))
}

pub struct NoAuth;

impl Auth for NoAuth {
    fn get_auth_token(&self, _context: &Stash) -> Vec<String> {
        vec!["*".to_string()]
    }

    fn is_authorized(&self, _credentials: &[String], _envelope: &Envelope) -> bool {
        true
    }
}

/// Auth leaf that pulls a single identity value out of the request `Stash`
/// under a configured key.
///
/// Intended as the "inner" Auth that wrapping authorizers (e.g. `CasbinAuth`
/// in `meshql-casbin`) delegate to in order to learn *who* the caller is.
/// Authorization is always `true` here — the wrapping Auth makes the actual
/// allow/deny call against `Envelope::authorized_tokens`.
///
/// Mirrors the role of `JWTSubAuthorizer` in the canonical Java meshql and
/// the JWT/Stash leaf Auth in meshobj/casbin_auth.
pub struct StashKeyAuth {
    key: String,
}

impl StashKeyAuth {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

impl Auth for StashKeyAuth {
    fn get_auth_token(&self, context: &Stash) -> Vec<String> {
        match context.get(&self.key).and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => vec![id.to_string()],
            _ => vec![],
        }
    }

    fn is_authorized(&self, _credentials: &[String], _envelope: &Envelope) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stash_with(key: &str, value: serde_json::Value) -> Stash {
        let mut s = Stash::new();
        s.insert(key.to_string(), value);
        s
    }

    #[test]
    fn stash_key_auth_reads_configured_key() {
        let auth = StashKeyAuth::new("user_id");
        let stash = stash_with("user_id", json!("alice@example.dev"));
        assert_eq!(auth.get_auth_token(&stash), vec!["alice@example.dev"]);
    }

    #[test]
    fn stash_key_auth_returns_empty_when_key_missing() {
        let auth = StashKeyAuth::new("user_id");
        let stash = Stash::new();
        assert_eq!(auth.get_auth_token(&stash), Vec::<String>::new());
    }

    #[test]
    fn stash_key_auth_returns_empty_for_non_string_value() {
        let auth = StashKeyAuth::new("user_id");
        let stash = stash_with("user_id", json!(42));
        assert_eq!(auth.get_auth_token(&stash), Vec::<String>::new());
    }

    #[test]
    fn stash_key_auth_returns_empty_for_blank_value() {
        let auth = StashKeyAuth::new("user_id");
        let stash = stash_with("user_id", json!(""));
        assert_eq!(auth.get_auth_token(&stash), Vec::<String>::new());
    }

    #[test]
    fn stash_key_auth_always_authorizes() {
        let auth = StashKeyAuth::new("user_id");
        let envelope = Envelope::new("x".to_string(), Stash::new(), vec!["admin".to_string()]);
        assert!(auth.is_authorized(&[], &envelope));
        assert!(auth.is_authorized(&["random".to_string()], &envelope));
    }
}
