//! The auth plugin, and the only place authorization is decided.
//!
//! See `meshql-cert/tests/features/contract/specs/auth-plugin-owns-authorization.md`.
//!
//! Authorization in meshql is one question — can this requester see this
//! record, or create it — and only the auth plugin may answer it. The surface
//! is deliberately tiny:
//!
//! ```text
//! authenticate(request_context) -> Session
//!
//! Session:
//!     stamp(envelope)                    -> envelope
//!     is_authorized(operation, envelope) -> bool
//! ```
//!
//! There is no visibility helper here on purpose. A helper anyone can call is
//! how eleven storage adapters came to answer the authorization question for
//! themselves, eleven different ways, with nothing detecting the difference.
//! Storage holds no credentials at all now: it hands the plugin an envelope
//! and takes the answer it is given.

use crate::{Envelope, Stash};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// What a caller is asking to do with a record.
///
/// One question with an operand, not three questions. This absorbs the
/// old `Auth::authorize_action`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operation {
    Read,
    Create,
    Remove,
}

/// The opaque, plugin-owned authorization mark carried by every envelope.
///
/// Storage persists it verbatim, inside the same write as the payload, and
/// never reads it: a crash can therefore never leave a record present with its
/// authorization missing. Nothing outside an auth plugin may interpret the
/// contents — the accessors below exist so an adapter can round-trip the
/// value through a column, not so it can re-derive a rule from it.
///
/// The persisted shape is the JSON array the `authorized_tokens` column
/// already holds, so existing rows migrate by doing nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthMark(Vec<String>);

impl AuthMark {
    /// Build a mark. Only an auth plugin has any business calling this.
    pub fn new(parts: Vec<String>) -> Self {
        Self(parts)
    }

    /// The empty mark. What a plugin that keeps its authorization elsewhere —
    /// in the payload, in a join table — stamps.
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    /// The mark's persisted form, for storage to write out verbatim.
    pub fn as_parts(&self) -> &[String] {
        &self.0
    }

    /// The mark's persisted form, consuming the mark.
    pub fn into_parts(self) -> Vec<String> {
        self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<String>> for AuthMark {
    fn from(parts: Vec<String>) -> Self {
        Self(parts)
    }
}

impl FromIterator<String> for AuthMark {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// A request-scoped authorization session.
///
/// Stateful must mean per request: a long-lived plugin holding mutable caller
/// state corrupts under concurrency. meshql-rs carries the session as a
/// handle, passed down every storage call where `tokens` used to go, so an
/// unset session is a compile error rather than a silent allow.
pub trait Session: Send + Sync {
    /// Mark an envelope on write with whatever this plugin needs persisted.
    /// The plugin decides; storage stores what it gets back without reading
    /// it.
    fn stamp(&self, envelope: Envelope) -> Envelope;

    /// Can this caller perform `operation` on this envelope?
    fn is_authorized(&self, operation: Operation, envelope: &Envelope) -> bool;
}

/// The plugin surface. One method.
pub trait Auth: Send + Sync {
    /// Resolve the caller from the portable request context an edge lifted out
    /// of trusted identity headers, and hold whatever is needed for the rest
    /// of that request.
    fn authenticate(&self, context: &AuthContext) -> Arc<dyn Session>;
}

/// Request-scoped auth context: a Stash populated by edge middleware from
/// trusted identity headers. `meshql-restlette`, `meshql-graphlette` and the
/// change surfaces read this from axum request extensions and feed it to the
/// configured `Auth::authenticate`.
#[derive(Clone, Debug, Default)]
pub struct AuthContext(pub Stash);

impl AuthContext {
    pub fn new(stash: Stash) -> Self {
        Self(stash)
    }

    pub fn stash(&self) -> &Stash {
        &self.0
    }

    pub fn into_stash(self) -> Stash {
        self.0
    }
}

/// The session a caller *outside* a request runs under: a worker, the change
/// feed's own polling, a migration, a test inspecting what storage actually
/// holds.
///
/// There is no unset session. An absent session fails closed — "no session
/// means allow" is the bypass this design exists to remove — so a caller with
/// no request says so explicitly, by naming this.
pub struct SystemSession;

impl Session for SystemSession {
    /// Leaves the envelope exactly as handed over: a system caller carries the
    /// mark the record is meant to have, rather than acquiring one.
    fn stamp(&self, envelope: Envelope) -> Envelope {
        envelope
    }

    fn is_authorized(&self, _operation: Operation, _envelope: &Envelope) -> bool {
        true
    }
}

/// The explicit system session. See [`SystemSession`].
pub fn system_session() -> Arc<dyn Session> {
    Arc::new(SystemSession)
}

/// The plugin that authorizes nothing and marks nothing.
///
/// `is_authorized` returns `true`, because that is what No Auth means. It does
/// **not** hand out a wildcard token: stamping every record with `"*"` was a
/// framework rule leaking into data, and a deployment that later turns auth on
/// would find every historical record public.
pub struct NoAuth;

struct NoAuthSession;

impl Session for NoAuthSession {
    fn stamp(&self, envelope: Envelope) -> Envelope {
        envelope
    }

    fn is_authorized(&self, _operation: Operation, _envelope: &Envelope) -> bool {
        true
    }
}

impl Auth for NoAuth {
    fn authenticate(&self, _context: &AuthContext) -> Arc<dyn Session> {
        Arc::new(NoAuthSession)
    }
}

/// The default token plugin's session: the caller's tokens, and the three
/// rules that used to live in framework code.
///
/// - the wildcard `"*"` — a caller holding it sees everything, an envelope
///   marked with it is visible to everyone,
/// - an empty mark means public,
/// - on write, the caller's tokens become the record's mark.
///
/// All three are *this plugin's* rules now. Nothing outside this file knows
/// them.
pub struct TokenSession {
    tokens: Vec<String>,
}

impl TokenSession {
    pub fn new(tokens: Vec<String>) -> Self {
        Self { tokens }
    }

    /// The tokens this caller resolved to. For a plugin that wraps this one
    /// (see `meshql-casbin`) and for the harnesses that certify the token
    /// plugin's own behaviour.
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }
}

impl Session for TokenSession {
    fn stamp(&self, mut envelope: Envelope) -> Envelope {
        envelope.auth = AuthMark::new(self.tokens.clone());
        envelope
    }

    fn is_authorized(&self, _operation: Operation, envelope: &Envelope) -> bool {
        let marks = envelope.auth.as_parts();
        if marks.is_empty() {
            return true;
        }
        if self.tokens.iter().any(|t| t == "*") {
            return true;
        }
        if marks.iter().any(|t| t == "*") {
            return true;
        }
        marks.iter().any(|t| self.tokens.iter().any(|c| c == t))
    }
}

/// A session over a fixed token set. The token plugin's rules, addressed
/// directly — used by the certification harnesses, by workers that write on
/// behalf of a known principal, and by anything else that already knows the
/// tokens rather than a request to resolve them from.
pub fn token_session(tokens: &[String]) -> Arc<dyn Session> {
    Arc::new(TokenSession::new(tokens.to_vec()))
}

/// Resolves a caller identity out of the request `Stash` under a configured
/// key, and authorizes with the token rules.
///
/// This is meshql's default plugin. It is also the "inner" identity source
/// that wrapping authorizers (e.g. `CasbinAuth` in `meshql-casbin`) delegate
/// to in order to learn *who* the caller is.
pub struct StashKeyAuth {
    key: String,
}

impl StashKeyAuth {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

/// How a wrapping plugin learns who the caller is without going through the
/// `Auth` surface, which answers a different question.
pub trait Identity: Send + Sync {
    fn identify(&self, context: &Stash) -> Vec<String>;
}

impl Identity for StashKeyAuth {
    fn identify(&self, context: &Stash) -> Vec<String> {
        match context.get(&self.key).and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => vec![id.to_string()],
            _ => vec![],
        }
    }
}

impl Auth for StashKeyAuth {
    fn authenticate(&self, context: &AuthContext) -> Arc<dyn Session> {
        Arc::new(TokenSession::new(self.identify(context.stash())))
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

    fn toks(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn marked(items: &[&str]) -> Envelope {
        Envelope::new("x", Stash::new(), toks(items))
    }

    #[test]
    fn stash_key_auth_reads_configured_key() {
        let auth = StashKeyAuth::new("user_id");
        let stash = stash_with("user_id", json!("alice@example.dev"));
        assert_eq!(auth.identify(&stash), vec!["alice@example.dev"]);
    }

    #[test]
    fn stash_key_auth_returns_empty_when_key_missing() {
        let auth = StashKeyAuth::new("user_id");
        assert_eq!(auth.identify(&Stash::new()), Vec::<String>::new());
    }

    #[test]
    fn stash_key_auth_returns_empty_for_non_string_value() {
        let auth = StashKeyAuth::new("user_id");
        let stash = stash_with("user_id", json!(42));
        assert_eq!(auth.identify(&stash), Vec::<String>::new());
    }

    #[test]
    fn stash_key_auth_returns_empty_for_blank_value() {
        let auth = StashKeyAuth::new("user_id");
        let stash = stash_with("user_id", json!(""));
        assert_eq!(auth.identify(&stash), Vec::<String>::new());
    }

    #[test]
    fn an_empty_mark_is_public() {
        let s = TokenSession::new(toks(&["anyone"]));
        assert!(s.is_authorized(Operation::Read, &marked(&[])));
        assert!(TokenSession::new(vec![]).is_authorized(Operation::Read, &marked(&[])));
    }

    #[test]
    fn a_wildcard_caller_sees_everything() {
        assert!(TokenSession::new(toks(&["*"])).is_authorized(Operation::Read, &marked(&["alice"])));
        assert!(TokenSession::new(toks(&["x", "*"]))
            .is_authorized(Operation::Read, &marked(&["alice", "bob"])));
    }

    #[test]
    fn a_wildcard_mark_is_visible_to_all() {
        assert!(
            TokenSession::new(toks(&["charlie"])).is_authorized(Operation::Read, &marked(&["*"]))
        );
        assert!(TokenSession::new(vec![]).is_authorized(Operation::Read, &marked(&["*"])));
    }

    #[test]
    fn otherwise_visibility_requires_intersection() {
        assert!(TokenSession::new(toks(&["bob"]))
            .is_authorized(Operation::Read, &marked(&["alice", "bob"])));
        assert!(
            !TokenSession::new(toks(&["bob"])).is_authorized(Operation::Read, &marked(&["alice"]))
        );
        assert!(!TokenSession::new(vec![]).is_authorized(Operation::Read, &marked(&["alice"])));
    }

    #[test]
    fn stamp_writes_the_callers_tokens_onto_the_envelope() {
        let s = TokenSession::new(toks(&["alice"]));
        let stamped = s.stamp(Envelope::new("x", Stash::new(), Vec::new()));
        assert_eq!(stamped.auth.as_parts(), ["alice".to_string()]);
    }

    /// No Auth means no authorization, not a wildcard token smuggled into
    /// every record's data.
    #[test]
    fn no_auth_authorizes_everything_and_marks_nothing() {
        let session = NoAuth.authenticate(&AuthContext::default());
        assert!(session.is_authorized(Operation::Read, &marked(&["alice"])));
        assert!(session.is_authorized(Operation::Create, &marked(&["alice"])));
        assert!(session.is_authorized(Operation::Remove, &marked(&["alice"])));
        let stamped = session.stamp(Envelope::new("x", Stash::new(), Vec::new()));
        assert!(
            stamped.auth.is_empty(),
            "NoAuth must not hand out a wildcard token"
        );
    }

    #[test]
    fn the_system_session_authorizes_everything_and_preserves_the_mark() {
        let session = system_session();
        assert!(session.is_authorized(Operation::Read, &marked(&["alice"])));
        let stamped = session.stamp(marked(&["alice"]));
        assert_eq!(stamped.auth.as_parts(), ["alice".to_string()]);
    }
}
