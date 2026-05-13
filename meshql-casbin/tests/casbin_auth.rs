//! Tests for `CasbinAuth`, parallel to the canonical Java suite
//! (`CasbinAuthTest.java`) and TypeScript suite
//! (`meshobj/core/casbin_auth/test/casbin.spec.ts`).

use casbin::CoreApi;
use meshql_casbin::CasbinAuth;
use meshql_core::{Auth, Envelope, Stash, StashKeyAuth};
use serde_json::json;

const MODEL: &str = "tests/fixtures/model.conf";
const POLICY: &str = "tests/fixtures/policy.csv";

fn stash_with_user(id: &str) -> Stash {
    let mut s = Stash::new();
    s.insert("user_id".to_string(), json!(id));
    s
}

fn envelope_with_tokens(tokens: Vec<String>) -> Envelope {
    Envelope::new("env-1".to_string(), Stash::new(), tokens)
}

#[tokio::test]
async fn initializes_from_model_and_policy_paths() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .expect("should load model/policy");
    // smoke: the enforcer is reachable
    assert!(auth.enforcer().get_model().get_model().contains_key("g"));
}

#[tokio::test]
async fn get_auth_token_returns_roles_for_known_user() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .unwrap();
    let roles = auth.get_auth_token(&stash_with_user("alice"));
    assert_eq!(roles, vec!["admin"]);
}

#[tokio::test]
async fn get_auth_token_returns_roles_for_editor() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .unwrap();
    let roles = auth.get_auth_token(&stash_with_user("bob"));
    assert_eq!(roles, vec!["editor"]);
}

#[tokio::test]
async fn get_auth_token_returns_empty_for_unknown_user() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .unwrap();
    let roles = auth.get_auth_token(&stash_with_user("nobody@example.dev"));
    assert!(roles.is_empty());
}

#[tokio::test]
async fn get_auth_token_returns_empty_when_inner_yields_no_user() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .unwrap();
    let roles = auth.get_auth_token(&Stash::new());
    assert!(roles.is_empty());
}

#[tokio::test]
async fn authorizes_when_credentials_overlap_authorized_tokens() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .unwrap();
    let env = envelope_with_tokens(vec!["admin".into(), "editor".into()]);
    assert!(auth.is_authorized(&["editor".to_string()], &env));
}

#[tokio::test]
async fn denies_when_no_credentials_overlap() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .unwrap();
    let env = envelope_with_tokens(vec!["admin".into(), "editor".into()]);
    assert!(!auth.is_authorized(&["viewer".to_string()], &env));
}

#[tokio::test]
async fn allows_everyone_when_authorized_tokens_is_empty() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .unwrap();
    let env = envelope_with_tokens(vec![]);
    assert!(auth.is_authorized(&[], &env));
    assert!(auth.is_authorized(&["anything".to_string()], &env));
}

#[tokio::test]
async fn end_to_end_alice_admin_can_access_admin_record() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .unwrap();
    let roles = auth.get_auth_token(&stash_with_user("alice"));
    let env = envelope_with_tokens(vec!["admin".into()]);
    assert!(auth.is_authorized(&roles, &env));
}

#[tokio::test]
async fn end_to_end_bob_editor_cannot_access_admin_only_record() {
    let auth = CasbinAuth::new(MODEL, POLICY, StashKeyAuth::new("user_id"))
        .await
        .unwrap();
    let roles = auth.get_auth_token(&stash_with_user("bob"));
    let env = envelope_with_tokens(vec!["admin".into()]);
    assert!(!auth.is_authorized(&roles, &env));
}
