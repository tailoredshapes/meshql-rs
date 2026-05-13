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
        let user_ids = self.inner.get_auth_token(context);
        let Some(user_id) = user_ids.first() else {
            return Vec::new();
        };
        self.enforcer.get_roles_for_user(user_id, None)
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
}
