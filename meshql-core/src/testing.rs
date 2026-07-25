use crate::{Envelope, Repository, Searcher, Stash};
use chrono::{DateTime, Utc};
use serde_json::json;

const STAR: &str = "*";
fn star() -> Vec<String> {
    vec![STAR.to_string()]
}

// ---- Repository Certification Tests ----

pub async fn test_create_should_store_and_return_envelope(repo: &dyn Repository) {
    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!("test farm"));

    let envelope = Envelope::new("id-1", payload, star());
    let result = repo.create(envelope, &star()).await.unwrap();

    assert_eq!(result.id, "id-1");
    assert!(!result.deleted);
    assert_eq!(result.payload.get("name").unwrap(), &json!("test farm"));
}

pub async fn test_read_should_retrieve_existing_envelope(repo: &dyn Repository) {
    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!("read test"));

    let envelope = Envelope::new("id-read", payload, star());
    repo.create(envelope, &star()).await.unwrap();

    let result = repo.read("id-read", &star(), None).await.unwrap();
    assert!(result.is_some());
    let found = result.unwrap();
    assert_eq!(found.id, "id-read");
    assert_eq!(found.payload.get("name").unwrap(), &json!("read test"));
}

pub async fn test_list_should_retrieve_all_created_envelopes(repo: &dyn Repository) {
    for i in 0..3 {
        let mut payload = Stash::new();
        payload.insert("name".to_string(), json!(format!("item-{i}")));
        let env = Envelope::new(format!("list-id-{i}"), payload, star());
        repo.create(env, &star()).await.unwrap();
    }

    let results = repo.list(&star()).await.unwrap();
    assert!(results.len() >= 3);
    let ids: Vec<&str> = results.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"list-id-0"));
    assert!(ids.contains(&"list-id-1"));
    assert!(ids.contains(&"list-id-2"));
}

pub async fn test_remove_should_delete_envelope(repo: &dyn Repository) {
    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!("to delete"));

    let env = Envelope::new("id-delete", payload, star());
    repo.create(env, &star()).await.unwrap();

    let deleted = repo.remove("id-delete", &star()).await.unwrap();
    assert!(deleted);

    let result = repo.read("id-delete", &star(), None).await.unwrap();
    assert!(result.is_none());
}

pub async fn test_create_many_should_store_multiple_envelopes(repo: &dyn Repository) {
    let envelopes: Vec<Envelope> = (0..3)
        .map(|i| {
            let mut payload = Stash::new();
            payload.insert("name".to_string(), json!(format!("bulk-{i}")));
            Envelope::new(format!("bulk-id-{i}"), payload, star())
        })
        .collect();

    let results = repo.create_many(envelopes, &star()).await.unwrap();
    assert_eq!(results.len(), 3);
}

pub async fn test_read_many_should_retrieve_multiple_envelopes(repo: &dyn Repository) {
    for i in 0..3 {
        let mut payload = Stash::new();
        payload.insert("name".to_string(), json!(format!("readmany-{i}")));
        let env = Envelope::new(format!("rm-id-{i}"), payload, star());
        repo.create(env, &star()).await.unwrap();
    }

    let ids: Vec<String> = (0..3).map(|i| format!("rm-id-{i}")).collect();
    let results = repo.read_many(&ids, &star()).await.unwrap();
    assert_eq!(results.len(), 3);
}

pub async fn test_remove_many_should_delete_multiple_envelopes(repo: &dyn Repository) {
    for i in 0..3 {
        let mut payload = Stash::new();
        payload.insert("name".to_string(), json!(format!("rmmany-{i}")));
        let env = Envelope::new(format!("rmmany-id-{i}"), payload, star());
        repo.create(env, &star()).await.unwrap();
    }

    let ids: Vec<String> = (0..3).map(|i| format!("rmmany-id-{i}")).collect();
    let results = repo.remove_many(&ids, &star()).await.unwrap();
    assert_eq!(results.len(), 3);
    assert!(results.values().all(|&v| v));
}

pub async fn test_temporal_versioning(repo: &dyn Repository) {
    let mut payload_v1 = Stash::new();
    payload_v1.insert("name".to_string(), json!("version-1"));
    let env_v1 = Envelope {
        id: "temporal-id".to_string(),
        payload: payload_v1,
        created_at: chrono::Utc::now() - chrono::Duration::seconds(10),
        deleted: false,
        authorized_tokens: star(),
    };
    repo.create(env_v1, &star()).await.unwrap();

    let between = chrono::Utc::now() - chrono::Duration::seconds(5);

    let mut payload_v2 = Stash::new();
    payload_v2.insert("name".to_string(), json!("version-2"));
    let env_v2 = Envelope {
        id: "temporal-id".to_string(),
        payload: payload_v2,
        created_at: chrono::Utc::now(),
        deleted: false,
        authorized_tokens: star(),
    };
    repo.create(env_v2, &star()).await.unwrap();

    // Read at time between the two versions — should get v1
    let at_v1 = repo
        .read("temporal-id", &star(), Some(between))
        .await
        .unwrap();
    assert!(at_v1.is_some());
    assert_eq!(
        at_v1.unwrap().payload.get("name").unwrap(),
        &json!("version-1")
    );

    // Read now — should get v2
    let current = repo.read("temporal-id", &star(), None).await.unwrap();
    assert!(current.is_some());
    assert_eq!(
        current.unwrap().payload.get("name").unwrap(),
        &json!("version-2")
    );
}

pub async fn test_list_shows_only_latest_version(repo: &dyn Repository) {
    let mut payload_v1 = Stash::new();
    payload_v1.insert("version".to_string(), json!("old"));
    let env_v1 = Envelope {
        id: "latest-test-id".to_string(),
        payload: payload_v1,
        created_at: chrono::Utc::now() - chrono::Duration::seconds(10),
        deleted: false,
        authorized_tokens: star(),
    };
    repo.create(env_v1, &star()).await.unwrap();

    let mut payload_v2 = Stash::new();
    payload_v2.insert("version".to_string(), json!("new"));
    let env_v2 = Envelope {
        id: "latest-test-id".to_string(),
        payload: payload_v2,
        created_at: chrono::Utc::now(),
        deleted: false,
        authorized_tokens: star(),
    };
    repo.create(env_v2, &star()).await.unwrap();

    let all = repo.list(&star()).await.unwrap();
    let for_id: Vec<_> = all.iter().filter(|e| e.id == "latest-test-id").collect();
    assert_eq!(for_id.len(), 1, "Should only show latest version");
    assert_eq!(for_id[0].payload.get("version").unwrap(), &json!("new"));
}

/// `remove` appends a tombstone version rather than mutating rows, so a backend
/// that drops deleted rows *before* resolving the latest version per id will
/// throw away the tombstone and resurface the previous version. One version is
/// not enough to catch that — there has to be an older one left behind to
/// resurface.
pub async fn test_list_excludes_deleted_envelope_with_prior_version(repo: &dyn Repository) {
    let mut payload_v1 = Stash::new();
    payload_v1.insert("version".to_string(), json!("old"));
    let env_v1 = Envelope {
        id: "deleted-with-history".to_string(),
        payload: payload_v1,
        created_at: chrono::Utc::now() - chrono::Duration::seconds(10),
        deleted: false,
        authorized_tokens: star(),
    };
    repo.create(env_v1, &star()).await.unwrap();

    let mut payload_v2 = Stash::new();
    payload_v2.insert("version".to_string(), json!("new"));
    let env_v2 = Envelope {
        id: "deleted-with-history".to_string(),
        payload: payload_v2,
        created_at: chrono::Utc::now() - chrono::Duration::seconds(5),
        deleted: false,
        authorized_tokens: star(),
    };
    repo.create(env_v2, &star()).await.unwrap();

    let removed = repo.remove("deleted-with-history", &star()).await.unwrap();
    assert!(removed, "remove should report that it deleted the envelope");

    let read_back = repo
        .read("deleted-with-history", &star(), None)
        .await
        .unwrap();
    assert!(read_back.is_none(), "read should not return a removed id");

    let all = repo.list(&star()).await.unwrap();
    let resurrected: Vec<_> = all
        .iter()
        .filter(|e| e.id == "deleted-with-history")
        .map(|e| e.payload.get("version").cloned())
        .collect();
    assert!(
        resurrected.is_empty(),
        "list resurrected a removed envelope with versions {resurrected:?}"
    );
}

// ---- Searcher Certification Tests ----

pub async fn seed_searcher_data(repo: &dyn Repository) {
    let items = vec![
        ("s-id-1", "alpha", 10i64, "typeA"),
        ("s-id-2", "beta", 20, "typeB"),
        ("s-id-3", "gamma", 30, "typeA"),
        ("s-id-4", "delta", 40, "typeB"),
    ];

    for (id, name, count, item_type) in items {
        let mut payload = Stash::new();
        payload.insert("name".to_string(), json!(name));
        payload.insert("count".to_string(), json!(count));
        payload.insert("type".to_string(), json!(item_type));
        let env = Envelope::new(id, payload, star());
        repo.create(env, &star()).await.unwrap();
    }
}

pub async fn test_searcher_empty_result_for_nonexistent(searcher: &dyn Searcher) {
    let args = Stash::new();
    let result = searcher
        .find(
            r#"{"id": "nonexistent-id"}"#,
            &args,
            &star(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert!(result.is_none());
}

pub async fn test_searcher_find_by_id(searcher: &dyn Searcher) {
    let mut args = Stash::new();
    args.insert("id".to_string(), json!("s-id-1"));
    let result = searcher
        .find(
            r#"{"id": "{{id}}"}"#,
            &args,
            &star(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert!(result.is_some());
    let stash = result.unwrap();
    assert_eq!(stash.get("id").unwrap(), &json!("s-id-1"));
    assert_eq!(stash.get("name").unwrap(), &json!("alpha"));
}

pub async fn test_searcher_find_by_name(searcher: &dyn Searcher) {
    let mut args = Stash::new();
    args.insert("name".to_string(), json!("beta"));
    let result = searcher
        .find(
            r#"{"payload.name": "{{name}}"}"#,
            &args,
            &star(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().get("name").unwrap(), &json!("beta"));
}

pub async fn test_searcher_find_all_by_type(searcher: &dyn Searcher) {
    let mut args = Stash::new();
    args.insert("type".to_string(), json!("typeA"));
    let results = searcher
        .find_all(
            r#"{"payload.type": "{{type}}"}"#,
            &args,
            &star(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    for r in &results {
        assert_eq!(r.get("type").unwrap(), &json!("typeA"));
    }
}

pub async fn test_searcher_find_all_by_type_and_name(searcher: &dyn Searcher) {
    let mut args = Stash::new();
    args.insert("type".to_string(), json!("typeB"));
    args.insert("name".to_string(), json!("delta"));
    let results = searcher
        .find_all(
            r#"{"payload.type": "{{type}}", "payload.name": "{{name}}"}"#,
            &args,
            &star(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].get("name").unwrap(), &json!("delta"));
}

pub async fn test_searcher_empty_array_for_nonexistent_type(searcher: &dyn Searcher) {
    let mut args = Stash::new();
    args.insert("type".to_string(), json!("typeZ"));
    let results = searcher
        .find_all(
            r#"{"payload.type": "{{type}}"}"#,
            &args,
            &star(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert!(results.is_empty());
}

pub async fn test_searcher_respects_limit(searcher: &dyn Searcher) {
    let mut args = Stash::new();
    args.insert("limit".to_string(), json!(1));
    let results = searcher
        .find_all(
            r#"{}"#,
            &args,
            &star(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
}

// ---- Searcher Authorization Certification Tests ----
//
// These certify the visibility convention of `meshql_core::envelope_visible_to`
// on every Searcher read path (architecture invariant 4):
//   - a caller holding "*" sees everything,
//   - an envelope with empty `authorized_tokens` is public,
//   - an envelope tagged "*" is visible to any caller,
//   - otherwise visibility requires token intersection.
// Visibility applies to the *latest* version of an envelope: an older visible
// version must not resurface when the current version is restricted.

fn creds(token: &str) -> Vec<String> {
    vec![token.to_string()]
}

pub async fn seed_searcher_auth_data(repo: &dyn Repository) {
    // (id, name, type, tokens) — repositories stamp the create() creds onto
    // the envelope, so the same tokens are passed both places.
    let items: Vec<(&str, &str, &str, Vec<String>)> = vec![
        ("auth-public", "public-doc", "authPublic", vec![]),
        ("auth-star", "star-doc", "authStar", star()),
        ("auth-alice", "alice-doc", "authShared", creds("alice")),
        ("auth-bob", "bob-doc", "authShared", creds("bob")),
    ];

    for (id, name, item_type, tokens) in items {
        let mut payload = Stash::new();
        payload.insert("name".to_string(), json!(name));
        payload.insert("type".to_string(), json!(item_type));
        let env = Envelope::new(id, payload, tokens.clone());
        repo.create(env, &tokens).await.unwrap();
    }

    // Versioned envelope: v1 visible to alice, later v2 visible only to bob.
    let mut payload_v1 = Stash::new();
    payload_v1.insert("name".to_string(), json!("versioned-v1"));
    payload_v1.insert("type".to_string(), json!("authVersioned"));
    let v1 = Envelope {
        id: "auth-versioned".to_string(),
        payload: payload_v1,
        created_at: chrono::Utc::now() - chrono::Duration::seconds(10),
        deleted: false,
        authorized_tokens: creds("alice"),
    };
    repo.create(v1, &creds("alice")).await.unwrap();

    let mut payload_v2 = Stash::new();
    payload_v2.insert("name".to_string(), json!("versioned-v2"));
    payload_v2.insert("type".to_string(), json!("authVersioned"));
    let v2 = Envelope {
        id: "auth-versioned".to_string(),
        payload: payload_v2,
        created_at: chrono::Utc::now(),
        deleted: false,
        authorized_tokens: creds("bob"),
    };
    repo.create(v2, &creds("bob")).await.unwrap();
}

pub async fn test_searcher_auth_wildcard_caller_sees_all(searcher: &dyn Searcher) {
    let args = Stash::new();
    let now = chrono::Utc::now().timestamp_millis();

    let results = searcher
        .find_all(r#"{"payload.type": "authShared"}"#, &args, &star(), now)
        .await
        .unwrap();
    assert_eq!(
        results.len(),
        2,
        "a '*' caller must see all token-restricted envelopes"
    );

    let result = searcher
        .find(r#"{"payload.name": "bob-doc"}"#, &args, &star(), now)
        .await
        .unwrap();
    assert!(
        result.is_some(),
        "a '*' caller must see a token-restricted envelope via find"
    );
}

pub async fn test_searcher_auth_restricted_caller_sees_only_intersecting(searcher: &dyn Searcher) {
    let args = Stash::new();
    let now = chrono::Utc::now().timestamp_millis();

    let results = searcher
        .find_all(
            r#"{"payload.type": "authShared"}"#,
            &args,
            &creds("alice"),
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        results.len(),
        1,
        "caller 'alice' must see exactly her own envelope"
    );
    assert_eq!(results[0].get("name").unwrap(), &json!("alice-doc"));

    // find must return the visible match even when an invisible envelope also
    // matches the query — visibility filtering happens before any limit.
    let result = searcher
        .find(
            r#"{"payload.type": "authShared"}"#,
            &args,
            &creds("alice"),
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        result
            .expect("find must locate the visible match")
            .get("name")
            .unwrap(),
        &json!("alice-doc")
    );
}

pub async fn test_searcher_auth_denies_non_intersecting(searcher: &dyn Searcher) {
    let args = Stash::new();
    let now = chrono::Utc::now().timestamp_millis();

    let result = searcher
        .find(
            r#"{"payload.name": "bob-doc"}"#,
            &args,
            &creds("alice"),
            now,
        )
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "caller 'alice' must not see an envelope restricted to 'bob'"
    );

    let results = searcher
        .find_all(
            r#"{"payload.name": "bob-doc"}"#,
            &args,
            &creds("alice"),
            now,
        )
        .await
        .unwrap();
    assert!(
        results.is_empty(),
        "find_all must not leak envelopes restricted to other tokens"
    );

    let result = searcher
        .find(r#"{"payload.name": "bob-doc"}"#, &args, &[], now)
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "a caller with no credentials must not see a restricted envelope"
    );
}

pub async fn test_searcher_auth_empty_tokens_are_public(searcher: &dyn Searcher) {
    let args = Stash::new();
    let now = chrono::Utc::now().timestamp_millis();

    for caller in [creds("charlie"), vec![]] {
        let result = searcher
            .find(r#"{"payload.name": "public-doc"}"#, &args, &caller, now)
            .await
            .unwrap();
        assert!(
            result.is_some(),
            "an envelope with no authorized_tokens is public (caller {caller:?})"
        );
    }
}

pub async fn test_searcher_auth_star_token_visible_to_all(searcher: &dyn Searcher) {
    let args = Stash::new();
    let now = chrono::Utc::now().timestamp_millis();

    for caller in [creds("charlie"), vec![]] {
        let result = searcher
            .find(r#"{"payload.name": "star-doc"}"#, &args, &caller, now)
            .await
            .unwrap();
        assert!(
            result.is_some(),
            "an envelope tagged '*' is visible to any caller (caller {caller:?})"
        );
    }
}

pub async fn test_searcher_auth_latest_version_controls_visibility(searcher: &dyn Searcher) {
    let args = Stash::new();
    let now = chrono::Utc::now().timestamp_millis();

    // Latest version is restricted to bob: alice must see nothing — the older
    // alice-visible version must not resurface.
    let result = searcher
        .find(
            r#"{"payload.type": "authVersioned"}"#,
            &args,
            &creds("alice"),
            now,
        )
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "an older visible version must not resurface when the latest is restricted"
    );

    let result = searcher
        .find(
            r#"{"payload.type": "authVersioned"}"#,
            &args,
            &creds("bob"),
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        result
            .expect("bob must see the latest version")
            .get("name")
            .unwrap(),
        &json!("versioned-v2")
    );
}

// ---- Repository Authorization Certification Tests ----
//
// The mirror image of the searcher auth certs above, for the Repository read
// paths (architecture invariant 4: *every* read path filters by tokens).
// The rest of the repository cert suite creates every envelope with `"*"`
// authorized_tokens, so an adapter that ignores `tokens` entirely still
// passes it — these certs close that gap.
//
// Same convention as `meshql_core::envelope_visible_to`:
//   - a caller holding "*" sees everything,
//   - an envelope with empty `authorized_tokens` is public,
//   - an envelope tagged "*" is visible to any caller,
//   - otherwise visibility requires token intersection.
// Visibility applies to the *latest* version of an envelope: an older visible
// version must not resurface when the current version is restricted.

const REPO_AUTH_PUBLIC: &str = "repo-auth-public";
const REPO_AUTH_STAR: &str = "repo-auth-star";
const REPO_AUTH_ALICE: &str = "repo-auth-alice";
const REPO_AUTH_BOB: &str = "repo-auth-bob";
const REPO_AUTH_VERSIONED: &str = "repo-auth-versioned";

fn ids_of(envelopes: &[Envelope]) -> Vec<&str> {
    envelopes.iter().map(|e| e.id.as_str()).collect()
}

pub async fn seed_repository_auth_data(repo: &dyn Repository) {
    // (id, name, tokens) — adapters that stamp the create() creds onto the
    // envelope and adapters that persist the envelope as given must agree, so
    // the same tokens are passed both places.
    let items: Vec<(&str, &str, Vec<String>)> = vec![
        (REPO_AUTH_PUBLIC, "public-doc", vec![]),
        (REPO_AUTH_STAR, "star-doc", star()),
        (REPO_AUTH_ALICE, "alice-doc", creds("alice")),
        (REPO_AUTH_BOB, "bob-doc", creds("bob")),
    ];

    for (id, name, tokens) in items {
        let mut payload = Stash::new();
        payload.insert("name".to_string(), json!(name));
        let env = Envelope::new(id, payload, tokens.clone());
        repo.create(env, &tokens).await.unwrap();
    }

    // Versioned envelope: v1 visible to alice, later v2 visible only to bob.
    let mut payload_v1 = Stash::new();
    payload_v1.insert("name".to_string(), json!("versioned-v1"));
    let v1 = Envelope {
        id: REPO_AUTH_VERSIONED.to_string(),
        payload: payload_v1,
        created_at: chrono::Utc::now() - chrono::Duration::seconds(10),
        deleted: false,
        authorized_tokens: creds("alice"),
    };
    repo.create(v1, &creds("alice")).await.unwrap();

    let mut payload_v2 = Stash::new();
    payload_v2.insert("name".to_string(), json!("versioned-v2"));
    let v2 = Envelope {
        id: REPO_AUTH_VERSIONED.to_string(),
        payload: payload_v2,
        created_at: chrono::Utc::now(),
        deleted: false,
        authorized_tokens: creds("bob"),
    };
    repo.create(v2, &creds("bob")).await.unwrap();
}

pub async fn test_repository_auth_wildcard_caller_sees_all(repo: &dyn Repository) {
    for id in [
        REPO_AUTH_PUBLIC,
        REPO_AUTH_STAR,
        REPO_AUTH_ALICE,
        REPO_AUTH_BOB,
        REPO_AUTH_VERSIONED,
    ] {
        assert!(
            repo.read(id, &star(), None).await.unwrap().is_some(),
            "a '*' caller must be able to read {id}"
        );
    }

    let listed = repo.list(&star()).await.unwrap();
    let ids = ids_of(&listed);
    for id in [
        REPO_AUTH_PUBLIC,
        REPO_AUTH_STAR,
        REPO_AUTH_ALICE,
        REPO_AUTH_BOB,
        REPO_AUTH_VERSIONED,
    ] {
        assert!(ids.contains(&id), "list must return {id} to a '*' caller");
    }

    let all: Vec<String> = vec![REPO_AUTH_ALICE.to_string(), REPO_AUTH_BOB.to_string()];
    let found = repo.read_many(&all, &star()).await.unwrap();
    assert_eq!(
        found.len(),
        2,
        "read_many must return every envelope to a '*' caller"
    );
}

pub async fn test_repository_auth_restricted_caller_sees_only_intersecting(repo: &dyn Repository) {
    let alice = creds("alice");

    let own = repo.read(REPO_AUTH_ALICE, &alice, None).await.unwrap();
    assert_eq!(
        own.expect("caller 'alice' must read her own envelope")
            .payload
            .get("name")
            .unwrap(),
        &json!("alice-doc")
    );

    let listed = repo.list(&alice).await.unwrap();
    let ids = ids_of(&listed);
    assert!(
        ids.contains(&REPO_AUTH_ALICE),
        "list must return alice's own"
    );
    assert!(
        ids.contains(&REPO_AUTH_PUBLIC),
        "list must return the public envelope"
    );
    assert!(
        ids.contains(&REPO_AUTH_STAR),
        "list must return the '*'-tagged envelope"
    );
    assert!(
        !ids.contains(&REPO_AUTH_BOB),
        "list must not leak an envelope restricted to 'bob'"
    );

    let both: Vec<String> = vec![REPO_AUTH_ALICE.to_string(), REPO_AUTH_BOB.to_string()];
    let found = repo.read_many(&both, &alice).await.unwrap();
    assert_eq!(
        ids_of(&found),
        vec![REPO_AUTH_ALICE],
        "read_many must return only the envelopes visible to 'alice'"
    );
}

pub async fn test_repository_auth_denies_non_intersecting(repo: &dyn Repository) {
    for caller in [creds("alice"), creds("charlie"), vec![]] {
        assert!(
            repo.read(REPO_AUTH_BOB, &caller, None)
                .await
                .unwrap()
                .is_none(),
            "read must not return an envelope restricted to 'bob' (caller {caller:?})"
        );

        let listed = repo.list(&caller).await.unwrap();
        assert!(
            !ids_of(&listed).contains(&REPO_AUTH_BOB),
            "list must not leak an envelope restricted to 'bob' (caller {caller:?})"
        );

        let ids = vec![REPO_AUTH_BOB.to_string()];
        assert!(
            repo.read_many(&ids, &caller).await.unwrap().is_empty(),
            "read_many must not leak an envelope restricted to 'bob' (caller {caller:?})"
        );

        assert!(
            !repo.remove(REPO_AUTH_BOB, &caller).await.unwrap(),
            "remove must not act on an envelope restricted to 'bob' (caller {caller:?})"
        );
    }

    // ...and after all those denied attempts, bob's envelope is still there.
    assert!(
        repo.read(REPO_AUTH_BOB, &creds("bob"), None)
            .await
            .unwrap()
            .is_some(),
        "a denied remove must not have deleted the envelope"
    );
}

pub async fn test_repository_auth_empty_tokens_are_public(repo: &dyn Repository) {
    for caller in [creds("charlie"), vec![]] {
        assert!(
            repo.read(REPO_AUTH_PUBLIC, &caller, None)
                .await
                .unwrap()
                .is_some(),
            "an envelope with no authorized_tokens is public (caller {caller:?})"
        );

        let listed = repo.list(&caller).await.unwrap();
        assert!(
            ids_of(&listed).contains(&REPO_AUTH_PUBLIC),
            "list must return the public envelope (caller {caller:?})"
        );

        let ids = vec![REPO_AUTH_PUBLIC.to_string()];
        assert_eq!(
            repo.read_many(&ids, &caller).await.unwrap().len(),
            1,
            "read_many must return the public envelope (caller {caller:?})"
        );
    }
}

pub async fn test_repository_auth_star_token_visible_to_all(repo: &dyn Repository) {
    for caller in [creds("charlie"), vec![]] {
        assert!(
            repo.read(REPO_AUTH_STAR, &caller, None)
                .await
                .unwrap()
                .is_some(),
            "an envelope tagged '*' is visible to any caller (caller {caller:?})"
        );

        let listed = repo.list(&caller).await.unwrap();
        assert!(
            ids_of(&listed).contains(&REPO_AUTH_STAR),
            "list must return the '*'-tagged envelope (caller {caller:?})"
        );

        let ids = vec![REPO_AUTH_STAR.to_string()];
        assert_eq!(
            repo.read_many(&ids, &caller).await.unwrap().len(),
            1,
            "read_many must return the '*'-tagged envelope (caller {caller:?})"
        );
    }
}

pub async fn test_repository_auth_latest_version_controls_visibility(repo: &dyn Repository) {
    // Latest version is restricted to bob: alice must see nothing — the older
    // alice-visible version must not resurface.
    assert!(
        repo.read(REPO_AUTH_VERSIONED, &creds("alice"), None)
            .await
            .unwrap()
            .is_none(),
        "an older visible version must not resurface when the latest is restricted"
    );

    let listed = repo.list(&creds("alice")).await.unwrap();
    assert!(
        !ids_of(&listed).contains(&REPO_AUTH_VERSIONED),
        "list must not resurface an older visible version"
    );

    let current = repo
        .read(REPO_AUTH_VERSIONED, &creds("bob"), None)
        .await
        .unwrap();
    assert_eq!(
        current
            .expect("bob must read the latest version")
            .payload
            .get("name")
            .unwrap(),
        &json!("versioned-v2")
    );

    let listed = repo.list(&creds("bob")).await.unwrap();
    assert!(
        ids_of(&listed).contains(&REPO_AUTH_VERSIONED),
        "list must return the latest version to 'bob'"
    );
}

// ---- Searcher Ordering Certification Tests ----
//
// These certify the canonical result ordering documented on
// `meshql_core::envelope_order`: a result set comes back in the insertion order
// of the envelopes it contains — the *resolved* (latest-at-or-before-`at`)
// version's `created_at`, then the envelope `id` to break millisecond ties —
// and `limit` truncates that order, so it returns a meaningful prefix rather
// than an arbitrary subset.
//
// The rest of the searcher cert suite asserts only set membership and result
// counts, so an adapter returning rows in arbitrary order passes it — these
// certs close that gap. They also pin cross-adapter equivalence: the same data
// returns the same sequence from every adapter, including the millisecond-tie
// case, which is the reason the `id` tiebreaker is applied uniformly instead of
// deferring to each backend's own sequence (log offset / rowid).

const ORDER_TYPE: &str = "ordering";
const ORDER_TIE_TYPE: &str = "orderingTie";
const ORDER_COUNT: usize = 12;
/// Sorts before every `ord-NN` id, so an adapter ordering by id alone — or by
/// the *first* version's position — puts it first instead of last.
const ORDER_MULTI: &str = "aaa-multi";
const ORDER_STEP_MS: i64 = 10_000;
/// 2024-01-01T00:00:00Z. The seeded timeline is absolute rather than relative
/// to "now" so a cert can address a specific point in it by arithmetic, without
/// depending on how fast the seeding loop ran.
const ORDER_BASE_MS: i64 = 1_704_067_200_000;

fn order_at(offset_ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ORDER_BASE_MS + offset_ms).expect("valid seed timestamp")
}

fn order_query() -> String {
    format!(r#"{{"payload.type": "{ORDER_TYPE}"}}"#)
}

fn ordered_ids(results: &[Stash]) -> Vec<String> {
    results
        .iter()
        .map(|s| {
            s.get("id")
                .and_then(|v| v.as_str())
                .expect("every result carries its envelope id")
                .to_string()
        })
        .collect()
}

/// The full expected sequence for `ORDER_TYPE`: the twelve single-version
/// envelopes in insertion order, then the multi-version envelope — whose
/// *latest* version was written after all of them.
fn expected_order() -> Vec<String> {
    let mut ids: Vec<String> = (0..ORDER_COUNT).map(|i| format!("ord-{i:02}")).collect();
    ids.push(ORDER_MULTI.to_string());
    ids
}

fn ordering_envelope(id: &str, name: &str, item_type: &str, created_at: DateTime<Utc>) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!(name));
    payload.insert("type".to_string(), json!(item_type));
    Envelope {
        id: id.to_string(),
        payload,
        created_at,
        deleted: false,
        authorized_tokens: star(),
    }
}

pub async fn seed_searcher_ordering_data(repo: &dyn Repository) {
    for i in 0..ORDER_COUNT {
        let id = format!("ord-{i:02}");
        let at = order_at(ORDER_STEP_MS * i as i64);
        repo.create(ordering_envelope(&id, &id, ORDER_TYPE, at), &star())
            .await
            .unwrap();
    }

    // A multi-version record. v1 lands between ord-00 and ord-01; v2 supersedes
    // it after the whole run. Reading now must place it last (v2's position);
    // reading as of a cutoff before v2 must place it second (v1's position).
    let v1_at = order_at(ORDER_STEP_MS / 2);
    let v2_at = order_at(ORDER_STEP_MS * (ORDER_COUNT as i64 + 1));
    repo.create(
        ordering_envelope(ORDER_MULTI, "multi-v1", ORDER_TYPE, v1_at),
        &star(),
    )
    .await
    .unwrap();
    repo.create(
        ordering_envelope(ORDER_MULTI, "multi-v2", ORDER_TYPE, v2_at),
        &star(),
    )
    .await
    .unwrap();

    // Two envelopes sharing an identical created_at, written in reverse id
    // order. `created_at` is millisecond-precision, so this is not a contrived
    // case — concurrent writers collide. Every adapter must resolve the tie the
    // same way: by id, regardless of which was physically written first.
    let tie_at = order_at(1_000);
    for id in ["tie-b", "tie-a"] {
        repo.create(ordering_envelope(id, id, ORDER_TIE_TYPE, tie_at), &star())
            .await
            .unwrap();
    }
}

pub async fn test_searcher_ordering_limit_truncates_in_insertion_order(searcher: &dyn Searcher) {
    let now = Utc::now().timestamp_millis();
    let expected = expected_order();

    let all = searcher
        .find_all(&order_query(), &Stash::new(), &star(), now)
        .await
        .unwrap();
    assert_eq!(
        ordered_ids(&all),
        expected,
        "an unlimited result set must come back in insertion order"
    );

    // The limit must select a prefix of that order — not an arbitrary subset of
    // the same size.
    for lim in [1usize, 5, ORDER_COUNT] {
        let mut args = Stash::new();
        args.insert("limit".to_string(), json!(lim));
        let limited = searcher
            .find_all(&order_query(), &args, &star(), now)
            .await
            .unwrap();
        assert_eq!(
            ordered_ids(&limited),
            expected[..lim],
            "limit {lim} must return the first {lim} envelopes in insertion order"
        );
    }

    // find() is the same order, taken one deep.
    let first = searcher
        .find(&order_query(), &Stash::new(), &star(), now)
        .await
        .unwrap()
        .expect("find must locate the first envelope in insertion order");
    assert_eq!(
        first.get("id").unwrap(),
        &json!(expected[0]),
        "find must return the first envelope in insertion order"
    );
}

pub async fn test_searcher_ordering_is_stable_across_repeated_queries(searcher: &dyn Searcher) {
    let now = Utc::now().timestamp_millis();
    let mut args = Stash::new();
    args.insert("limit".to_string(), json!(5));

    let mut seen: Vec<Vec<String>> = Vec::new();
    for _ in 0..4 {
        let results = searcher
            .find_all(&order_query(), &args, &star(), now)
            .await
            .unwrap();
        seen.push(ordered_ids(&results));
    }

    for (i, run) in seen.iter().enumerate().skip(1) {
        assert_eq!(
            run, &seen[0],
            "repeat {i} of an identical query returned a different order"
        );
    }
    assert_eq!(
        seen[0],
        expected_order()[..5],
        "the stable order must be the canonical one"
    );
}

pub async fn test_searcher_ordering_uses_resolved_version_position(searcher: &dyn Searcher) {
    let now = Utc::now().timestamp_millis();

    let all = searcher
        .find_all(&order_query(), &Stash::new(), &star(), now)
        .await
        .unwrap();
    let ids = ordered_ids(&all);
    assert_eq!(
        ids.last().map(String::as_str),
        Some(ORDER_MULTI),
        "a record's position is its *latest* version's position, not its first"
    );
    assert_eq!(
        all.last().unwrap().get("name").unwrap(),
        &json!("multi-v2"),
        "the resolved version must be the latest one"
    );
}

/// The as-of half of the cert above: ordering follows the version resolved at
/// the `at` cutoff, so a record moves within the result set as the cutoff moves.
///
/// Only for adapters whose Searcher honours `at`. `KsqlSearcher` reads a ksqlDB
/// TABLE, which materializes the latest version per key only, so it ignores the
/// cutoff and cannot satisfy this.
pub async fn test_searcher_ordering_as_of_uses_version_resolved_at_cutoff(searcher: &dyn Searcher) {
    // Read as of ord-05's own timestamp. The multi-version record resolves to
    // v1, which was written near the start — so it must move back to second
    // place, and carry v1's payload.
    let cutoff = ORDER_BASE_MS + ORDER_STEP_MS * 5;

    let as_of = searcher
        .find_all(&order_query(), &Stash::new(), &star(), cutoff)
        .await
        .unwrap();
    let expected_as_of: Vec<String> = vec![
        "ord-00".to_string(),
        ORDER_MULTI.to_string(),
        "ord-01".to_string(),
        "ord-02".to_string(),
        "ord-03".to_string(),
        "ord-04".to_string(),
        "ord-05".to_string(),
    ];
    assert_eq!(
        ordered_ids(&as_of),
        expected_as_of,
        "as-of ordering must use the version resolved at the cutoff"
    );
    assert_eq!(
        as_of[1].get("name").unwrap(),
        &json!("multi-v1"),
        "the version resolved at the cutoff must be v1"
    );

    // ...and the limit still truncates that order.
    let mut args = Stash::new();
    args.insert("limit".to_string(), json!(2));
    let limited = searcher
        .find_all(&order_query(), &args, &star(), cutoff)
        .await
        .unwrap();
    assert_eq!(
        ordered_ids(&limited),
        expected_as_of[..2],
        "limit must truncate the as-of order too"
    );
}

pub async fn test_searcher_ordering_breaks_millisecond_ties_by_id(searcher: &dyn Searcher) {
    let now = Utc::now().timestamp_millis();
    let query = format!(r#"{{"payload.type": "{ORDER_TIE_TYPE}"}}"#);

    let results = searcher
        .find_all(&query, &Stash::new(), &star(), now)
        .await
        .unwrap();
    assert_eq!(
        ordered_ids(&results),
        vec!["tie-a".to_string(), "tie-b".to_string()],
        "envelopes sharing a created_at millisecond order by id — uniformly on \
         every adapter, so two adapters holding the same data agree"
    );

    let mut args = Stash::new();
    args.insert("limit".to_string(), json!(1));
    let limited = searcher
        .find_all(&query, &args, &star(), now)
        .await
        .unwrap();
    assert_eq!(
        ordered_ids(&limited),
        vec!["tie-a".to_string()],
        "a limit over tied envelopes must still be deterministic"
    );
}

pub async fn test_searcher_empty_query(searcher: &dyn Searcher) {
    let args = Stash::new();
    let results = searcher
        .find_all(
            r#"{}"#,
            &args,
            &star(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .unwrap();
    assert!(!results.is_empty());
}

// ---- Searcher Result Shape Certification Tests ----
//
// A Searcher returns a flat `Stash` per result: the resolved envelope's payload
// with two envelope-level fields merged in — `id` and `createdAt`, the RFC3339
// rendering of `created_at`. A GraphQL schema that declares a `createdAt` field
// resolves it straight off that key (see
// meshql-graphlette/tests/created_at_cert.rs), so an adapter that omits it does
// not error — the field silently comes back null.
//
// The rest of the searcher cert suite only ever reads `id` and payload fields,
// so an adapter dropping `createdAt` passes all of it. These certs close that
// gap and hold every backend to the same result shape, down to the rendering:
// two adapters holding the same envelope must hand a client the same string.

const SHAPE_TYPE: &str = "resultShape";
const SHAPE_SINGLE: &str = "shape-single";
const SHAPE_MULTI: &str = "shape-multi";
/// 2024-06-01T12:34:56.789Z, and an hour later for the second version.
/// Deliberately off a second boundary: `created_at` is millisecond-precision on
/// every backend, so an adapter that rounds it away should fail here rather
/// than pass on a coarser value that happens to look right.
const SHAPE_V1_MS: i64 = 1_717_245_296_789;
const SHAPE_V2_MS: i64 = 1_717_248_896_789;

fn shape_at(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).expect("valid seed timestamp")
}

fn shape_query() -> String {
    format!(r#"{{"payload.type": "{SHAPE_TYPE}"}}"#)
}

fn shape_envelope(id: &str, name: &str, created_at: DateTime<Utc>) -> Envelope {
    let mut payload = Stash::new();
    payload.insert("name".to_string(), json!(name));
    payload.insert("type".to_string(), json!(SHAPE_TYPE));
    Envelope {
        id: id.to_string(),
        payload,
        created_at,
        deleted: false,
        authorized_tokens: star(),
    }
}

pub async fn seed_searcher_result_shape_data(repo: &dyn Repository) {
    repo.create(
        shape_envelope(SHAPE_SINGLE, "single", shape_at(SHAPE_V1_MS)),
        &star(),
    )
    .await
    .unwrap();

    // A two-version record: `createdAt` must be the *resolved* version's, so an
    // adapter cannot satisfy the cert by echoing whichever version it happened
    // to read first.
    repo.create(
        shape_envelope(SHAPE_MULTI, "multi-v1", shape_at(SHAPE_V1_MS)),
        &star(),
    )
    .await
    .unwrap();
    repo.create(
        shape_envelope(SHAPE_MULTI, "multi-v2", shape_at(SHAPE_V2_MS)),
        &star(),
    )
    .await
    .unwrap();
}

fn shape_result_for<'a>(results: &'a [Stash], id: &str) -> &'a Stash {
    results
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))
        .unwrap_or_else(|| {
            panic!("no result carried the id {id:?} — every result must carry its envelope id; got {results:?}")
        })
}

fn assert_result_shape(stash: &Stash, expected_ms: i64, whence: &str) {
    let created_at = stash
        .get("createdAt")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!(
                "{whence}: every result must carry `createdAt` — a GraphQL schema \
                 declaring the field resolves it off this key, and gets null \
                 without it; stash: {stash:?}"
            )
        });

    let parsed = DateTime::parse_from_rfc3339(created_at).unwrap_or_else(|e| {
        panic!("{whence}: `createdAt` must be RFC3339, got {created_at:?}: {e}")
    });
    assert_eq!(
        parsed.timestamp_millis(),
        expected_ms,
        "{whence}: `createdAt` must be the resolved envelope's own created_at, to the millisecond"
    );
    assert_eq!(
        created_at,
        shape_at(expected_ms).to_rfc3339(),
        "{whence}: every adapter must render `createdAt` identically, so a client \
         comparing the strings agrees across backends"
    );
}

pub async fn test_searcher_result_carries_id_and_created_at(searcher: &dyn Searcher) {
    let now = Utc::now().timestamp_millis();

    let all = searcher
        .find_all(&shape_query(), &Stash::new(), &star(), now)
        .await
        .unwrap();
    assert_eq!(
        all.len(),
        2,
        "the result-shape seed holds two records: one single-version, one with two versions"
    );
    assert_result_shape(
        shape_result_for(&all, SHAPE_SINGLE),
        SHAPE_V1_MS,
        "find_all",
    );
    assert_result_shape(
        shape_result_for(&all, SHAPE_MULTI),
        SHAPE_V2_MS,
        "find_all, multi-version record",
    );

    // `find` is a separate read path on several adapters, so certify it too.
    let mut args = Stash::new();
    args.insert("id".to_string(), json!(SHAPE_SINGLE));
    let one = searcher
        .find(r#"{"id": "{{id}}"}"#, &args, &star(), now)
        .await
        .unwrap()
        .expect("find must locate the single-version result-shape record");
    assert_eq!(
        one.get("id").and_then(|v| v.as_str()),
        Some(SHAPE_SINGLE),
        "find: every result must carry its envelope id"
    );
    assert_result_shape(&one, SHAPE_V1_MS, "find");

    // The merged fields are additions, not a replacement: the payload is still
    // there alongside them.
    assert_eq!(
        one.get("name"),
        Some(&json!("single")),
        "find: `id` and `createdAt` are merged into the payload, not substituted for it"
    );
}
