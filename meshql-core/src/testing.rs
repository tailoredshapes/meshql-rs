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
    assert_eq!(
        for_id[0].payload.get("version").unwrap(),
        &json!("new")
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
