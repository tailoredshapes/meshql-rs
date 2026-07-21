//! Certification suite for `ChangeSource` implementations (invariant 5:
//! storage is pluggable, behavior is certified). Drive writes through the
//! provided `Repository`; assert the source under test emits the right
//! events. Every `ChangeSource` impl must pass all of these before merging.

use crate::{ChangeEvent, ChangeSource};
use meshql_core::{Envelope, Repository, Stash};
use serde_json::json;
use std::time::Duration;

fn wildcard() -> Vec<String> {
    vec!["*".to_string()]
}

fn payload(name: &str) -> Stash {
    let mut s = Stash::new();
    s.insert("name".to_string(), json!(name));
    s
}

async fn drain(source: &dyn ChangeSource) -> Vec<ChangeEvent> {
    source.poll().await.expect("poll succeeds")
}

fn unique_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("cert-{}", N.fetch_add(1, Ordering::Relaxed))
}

/// A create is emitted with the envelope's commit time and tokens.
pub async fn test_detects_create(source: &dyn ChangeSource, repo: &dyn Repository) {
    drain(source).await; // settle any pre-existing state

    // NB: Repository::create tags the envelope with its `tokens` argument
    // (the caller's credentials become the ACL — the restlette convention),
    // overwriting whatever Envelope::new was given.
    let env = repo
        .create(
            Envelope::new(unique_id(), payload("henrietta"), vec![]),
            &["farm-team".to_string()],
        )
        .await
        .expect("create");

    let events = drain(source).await;
    let ev = events
        .iter()
        .find(|e| e.id == env.id)
        .expect("create event emitted");
    assert!(!ev.deleted);
    assert_eq!(ev.entity, source.entity());
    assert_eq!(ev.created_at, env.created_at.timestamp_millis());
    assert_eq!(ev.authorized_tokens, vec!["farm-team".to_string()]);
}

/// An update (new version, same id, changed payload) is emitted.
pub async fn test_detects_update(source: &dyn ChangeSource, repo: &dyn Repository) {
    let id = unique_id();
    repo.create(
        Envelope::new(id.clone(), payload("v1"), vec![]),
        &wildcard(),
    )
    .await
    .expect("create");
    drain(source).await;

    tokio::time::sleep(Duration::from_millis(5)).await; // distinct commit ms
    let v2 = repo
        .create(
            Envelope::new(id.clone(), payload("v2"), vec![]),
            &wildcard(),
        )
        .await
        .expect("update-as-new-version");

    let events = drain(source).await;
    let ev = events
        .iter()
        .find(|e| e.id == id)
        .expect("update event emitted");
    assert!(!ev.deleted);
    assert_eq!(ev.created_at, v2.created_at.timestamp_millis());
}

/// A byte-identical rewrite produces no observable change: no event.
pub async fn test_ignores_identical_rewrite(source: &dyn ChangeSource, repo: &dyn Repository) {
    let id = unique_id();
    repo.create(
        Envelope::new(id.clone(), payload("same"), vec![]),
        &wildcard(),
    )
    .await
    .expect("create");
    drain(source).await;

    tokio::time::sleep(Duration::from_millis(5)).await;
    repo.create(
        Envelope::new(id.clone(), payload("same"), vec![]),
        &wildcard(),
    )
    .await
    .expect("rewrite");

    let events = drain(source).await;
    assert!(
        events.iter().all(|e| e.id != id),
        "identical payload must not emit"
    );
}

/// A delete is emitted as deleted=true carrying the last-known tokens.
pub async fn test_detects_delete(source: &dyn ChangeSource, repo: &dyn Repository) {
    let id = unique_id();
    repo.create(
        Envelope::new(id.clone(), payload("doomed"), vec![]),
        &["farm-team".to_string()],
    )
    .await
    .expect("create");
    drain(source).await;

    assert!(repo.remove(&id, &wildcard()).await.expect("remove"));

    let events = drain(source).await;
    let ev = events
        .iter()
        .find(|e| e.id == id)
        .expect("delete event emitted");
    assert!(ev.deleted);
    assert_eq!(ev.authorized_tokens, vec!["farm-team".to_string()]);
}

/// Create+update+delete strictly between polls collapses to a delete.
pub async fn test_update_then_delete_between_polls(
    source: &dyn ChangeSource,
    repo: &dyn Repository,
) {
    let id = unique_id();
    repo.create(
        Envelope::new(id.clone(), payload("v1"), vec![]),
        &wildcard(),
    )
    .await
    .expect("create");
    drain(source).await;

    tokio::time::sleep(Duration::from_millis(5)).await;
    repo.create(
        Envelope::new(id.clone(), payload("v2"), vec![]),
        &wildcard(),
    )
    .await
    .expect("update");
    assert!(repo.remove(&id, &wildcard()).await.expect("remove"));

    let events = drain(source).await;
    let for_id: Vec<_> = events.iter().filter(|e| e.id == id).collect();
    assert!(
        for_id.iter().any(|e| e.deleted),
        "a delete must be emitted; got {for_id:?}"
    );
}

/// A quiet store emits nothing (poll idempotence).
pub async fn test_quiet_store_emits_nothing(source: &dyn ChangeSource, repo: &dyn Repository) {
    repo.create(
        Envelope::new(unique_id(), payload("steady"), vec![]),
        &wildcard(),
    )
    .await
    .expect("create");
    drain(source).await;

    let events = drain(source).await;
    assert!(events.is_empty(), "no writes → no events; got {events:?}");
}
