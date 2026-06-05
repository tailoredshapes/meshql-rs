//! Casbin-based authorizer for meshql.
//!
//! Wraps any [`meshql_core::Auth`] implementation and uses a Casbin
//! [`Enforcer`] to resolve the wrapped Auth's user identity into a set of
//! roles. Those roles are then matched against
//! [`meshql_core::Envelope::authorized_tokens`] in [`Auth::is_authorized`].
//!
//! Mirrors the canonical Java implementation in
//! `meshql/auth/casbin/src/main/java/com/meshql/auth/casbin/CasbinAuth.java`
//! and the TypeScript implementation in
//! `meshobj/core/casbin_auth/src/index.ts`.
//!
//! # Example
//!
//! ```no_run
//! use meshql_core::{Auth, StashKeyAuth};
//! use meshql_casbin::CasbinAuth;
//!
//! # async fn run() -> Result<(), meshql_casbin::CasbinAuthError> {
//! let inner = StashKeyAuth::new("user_id");
//! let auth = CasbinAuth::new("model.conf", "policy.csv", inner).await?;
//! // `auth` is now a meshql Auth that delegates identity to StashKeyAuth
//! // and resolves roles via Casbin.
//! # Ok(())
//! # }
//! ```

use casbin::{CoreApi, DefaultModel, Enforcer, FileAdapter, MgmtApi, RbacApi};
use meshql_core::{Auth, Envelope, Stash};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CasbinAuthError {
    #[error("casbin enforcer initialization failed: {0}")]
    Casbin(#[from] casbin::Error),
}

/// `Auth` impl that wraps another `Auth` and resolves the inner user identity
/// into a list of roles via a Casbin policy.
///
/// `get_auth_token` returns the caller's roles.
/// `is_authorized` returns true when any of those roles appears in the
/// envelope's `authorized_tokens` — or when `authorized_tokens` is empty
/// (treated as a public record).
pub struct CasbinAuth<A: Auth> {
    enforcer: Enforcer,
    inner: A,
}

impl<A: Auth> CasbinAuth<A> {
    /// Construct from filesystem paths to a Casbin model and policy.
    pub async fn new(
        model_path: impl AsRef<Path> + Send + Sync,
        policy_path: impl AsRef<Path> + Send + Sync + 'static,
        inner: A,
    ) -> Result<Self, CasbinAuthError> {
        let model = DefaultModel::from_file(model_path).await?;
        let adapter = FileAdapter::new(policy_path);
        let enforcer = Enforcer::new(model, adapter).await?;
        Ok(Self { enforcer, inner })
    }

    /// Construct from in-memory strings — useful when the model and policy
    /// are embedded in the binary via `include_str!`. Empty rows and rows
    /// starting with `#` are ignored.
    pub async fn from_strings(
        model_str: &str,
        policy_str: &str,
        inner: A,
    ) -> Result<Self, CasbinAuthError> {
        let model = DefaultModel::from_str(model_str).await?;
        let mut enforcer = Enforcer::new(model, ()).await?;
        for line in policy_str.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, ',');
            let Some(ptype) = parts.next().map(str::trim) else {
                continue;
            };
            let Some(rest) = parts.next() else { continue };
            let rule: Vec<String> = rest.split(',').map(|s| s.trim().to_string()).collect();
            if ptype == "g" {
                enforcer.add_grouping_policy(rule).await?;
            } else if ptype == "p" {
                enforcer.add_policy(rule).await?;
            }
        }
        Ok(Self { enforcer, inner })
    }

    /// Construct from an already-built Enforcer (handy for tests or when the
    /// host application wants to load model/policy from a non-filesystem
    /// adapter).
    pub fn from_enforcer(enforcer: Enforcer, inner: A) -> Self {
        Self { enforcer, inner }
    }

    /// Borrow the underlying Enforcer for advanced use cases (adding policies
    /// at runtime, custom queries, etc.).
    pub fn enforcer(&self) -> &Enforcer {
        &self.enforcer
    }
}

impl<A: Auth> Auth for CasbinAuth<A> {
    fn get_auth_token(&self, context: &Stash) -> Vec<String> {
        let mut creds: Vec<String> = Vec::new();
        // Roles bound to the user_id in the embedded g-policy (e.g. a known
        // operator -> admin).
        if let Some(user_id) = self.inner.get_auth_token(context).first() {
            creds.extend(self.enforcer.get_roles_for_user(user_id, None));
        }
        // Roles the trusted edge injected directly as groups (the standard
        // trusted-header model: the edge resolves identity -> role and stamps
        // it, so callers we've never seen — SSO prospects — still get a role).
        if let Some(groups) = context.get("groups").and_then(|v| v.as_array()) {
            for role in groups.iter().filter_map(|g| g.as_str()) {
                if !creds.iter().any(|c| c == role) {
                    creds.push(role.to_string());
                }
            }
        }
        creds
    }

    fn is_authorized(&self, credentials: &[String], envelope: &Envelope) -> bool {
        if envelope.authorized_tokens.is_empty() {
            return true;
        }
        envelope
            .authorized_tokens
            .iter()
            .any(|t| credentials.iter().any(|c| c == t))
    }

    fn authorize_action(&self, credentials: &[String], action: &str) -> bool {
        // Allowed iff any of the caller's roles permits `action` on the API
        // surface per the embedded policy (admin: `*`, editor: write, viewer:
        // read). `/api` matches the policy's `/*` object glob.
        credentials.iter().any(|role| {
            self.enforcer
                .enforce((role.as_str(), "/api", action))
                .unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;
    use meshql_core::{Auth, Stash, StashKeyAuth};
    use serde_json::json;

    const MODEL: &str = "[request_definition]\nr = sub, obj, act\n[policy_definition]\np = sub, obj, act\n[role_definition]\ng = _, _\n[policy_effect]\ne = some(where (p.eft == allow))\n[matchers]\nm = g(r.sub, p.sub) && keyMatch(r.obj, p.obj) && (r.act == p.act || p.act == \"*\")\n";
    const POLICY: &str = "p, admin, /*, *\np, editor, /*, read\np, editor, /*, write\np, viewer, /*, read\ng, alice@example.dev, admin\n";

    async fn auth() -> CasbinAuth<StashKeyAuth> {
        CasbinAuth::from_strings(MODEL, POLICY, StashKeyAuth::new("user_id"))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn write_action_enforced_by_role() {
        let a = auth().await;
        assert!(a.authorize_action(&["admin".into()], "write"));
        assert!(a.authorize_action(&["editor".into()], "write"));
        assert!(!a.authorize_action(&["viewer".into()], "write"));
        assert!(!a.authorize_action(&[], "write"));
        assert!(a.authorize_action(&["viewer".into()], "read"));
    }

    #[tokio::test]
    async fn edge_injected_groups_become_roles() {
        let a = auth().await;
        let mut stash = Stash::new();
        // A user we've never seen (an SSO prospect) — no g-binding — but the
        // edge stamped a role via groups.
        stash.insert("user_id".into(), json!("prospect@example.com"));
        stash.insert("groups".into(), json!(["viewer"]));
        let creds = a.get_auth_token(&stash);
        assert!(creds.contains(&"viewer".to_string()));
        assert!(!a.authorize_action(&creds, "write"));
        assert!(a.authorize_action(&creds, "read"));
    }
}
