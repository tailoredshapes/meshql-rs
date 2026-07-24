use crate::{Envelope, Repository, Searcher, Stash};
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
