//! The two auth plugins `auth_plugin.feature` certifies against.
//!
//! Every other certification suite drives the *default* token plugin. Against
//! a token plugin, a storage adapter that reimplements the token rule
//! correctly and one that delegates are indistinguishable, so nothing in the
//! older suites detects an adapter answering the authorization question for
//! itself.
//!
//! These two cannot be second-guessed. `refuse-reads` says no to every read
//! while still writing normally, so an adapter that ignores the plugin returns
//! rows it was told not to. `owner` holds no tokens at all and authorizes on a
//! payload field, so an adapter that assumes the token shape returns nothing —
//! or everything.

use meshql_core::{
    Auth, AuthContext, AuthMark, Envelope, Identity, Operation, Session, StashKeyAuth,
};
use std::sync::Arc;

use crate::authz::IDENTITY_KEY;

/// A plugin that stamps normally on write and refuses every read.
pub struct RefuseReads;

struct RefuseReadsSession {
    tokens: Vec<String>,
}

impl Session for RefuseReadsSession {
    fn stamp(&self, mut envelope: Envelope) -> Envelope {
        envelope.auth = AuthMark::new(self.tokens.clone());
        envelope
    }

    fn is_authorized(&self, operation: Operation, _envelope: &Envelope) -> bool {
        // Writes go through; nothing is ever readable. An adapter that filters
        // for itself instead of asking will hand back the rows it just wrote.
        !matches!(operation, Operation::Read)
    }
}

impl Auth for RefuseReads {
    fn authenticate(&self, context: &AuthContext) -> Arc<dyn Session> {
        Arc::new(RefuseReadsSession {
            tokens: StashKeyAuth::new(IDENTITY_KEY).identify(context.stash()),
        })
    }
}

/// A plugin that holds no tokens: it records the caller as the payload's
/// `owner` on write, stamps no mark at all, and authorizes a read only when
/// that field names the caller.
///
/// This is the join case in miniature. It proves the framework never assumes
/// the token shape.
pub struct OwnerAuth;

struct OwnerSession {
    caller: Option<String>,
}

impl OwnerSession {
    fn owns(&self, envelope: &Envelope) -> bool {
        match (
            &self.caller,
            envelope.payload.get("owner").and_then(|v| v.as_str()),
        ) {
            (Some(caller), Some(owner)) => caller == owner,
            _ => false,
        }
    }
}

impl Session for OwnerSession {
    fn stamp(&self, mut envelope: Envelope) -> Envelope {
        if let Some(caller) = &self.caller {
            envelope.payload.insert(
                "owner".to_string(),
                serde_json::Value::String(caller.clone()),
            );
        }
        // Deliberately empty: this plugin keeps nothing in the mark, and the
        // certification asserts storage persisted exactly that.
        envelope.auth = AuthMark::empty();
        envelope
    }

    fn is_authorized(&self, operation: Operation, envelope: &Envelope) -> bool {
        match operation {
            Operation::Create => {
                // A record with no owner yet is a fresh create, and anyone
                // identified may make one — `stamp` decides whose it becomes.
                // A record that already names an owner is an update, and only
                // that owner may make it; otherwise a stranger could take a
                // record over by writing a new version of its id.
                self.caller.is_some()
                    && (envelope.payload.get("owner").is_none() || self.owns(envelope))
            }
            Operation::Read | Operation::Remove => self.owns(envelope),
        }
    }
}

impl Auth for OwnerAuth {
    fn authenticate(&self, context: &AuthContext) -> Arc<dyn Session> {
        Arc::new(OwnerSession {
            caller: StashKeyAuth::new(IDENTITY_KEY)
                .identify(context.stash())
                .into_iter()
                .next(),
        })
    }
}

/// Resolve a plugin by the name a scenario uses.
pub fn plugin_named(name: &str) -> Arc<dyn Auth> {
    match name {
        "refuse-reads" => Arc::new(RefuseReads),
        "owner" => Arc::new(OwnerAuth),
        other => panic!("no certification auth plugin named '{other}'"),
    }
}
