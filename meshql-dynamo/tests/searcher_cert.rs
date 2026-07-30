//! Searcher certification, DynamoDB adapter — the base suite plus the
//! authorization, ordering and result-shape certs, matching
//! `meshql-sqlite/tests/searcher_cert.rs` function for function.

mod common;

use common::SearcherFixture;
use meshql_core::testing as cert;
use meshql_core::{Searcher, Stash};
use serde_json::json;

#[tokio::test]
async fn should_return_empty_for_nonexistent_id() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_empty_result_for_nonexistent(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_find_by_id() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_find_by_id(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_find_by_name() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_find_by_name(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_find_all_by_type() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_find_all_by_type(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_find_all_by_type_and_name() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_find_all_by_type_and_name(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_return_empty_for_nonexistent_type() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_empty_array_for_nonexistent_type(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_respect_limit() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_respects_limit(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn should_handle_empty_query() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_empty_query(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_wildcard_caller_sees_all() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_auth_wildcard_caller_sees_all(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_restricted_caller_sees_only_intersecting() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_auth_restricted_caller_sees_only_intersecting(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_denies_non_intersecting() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_auth_denies_non_intersecting(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_empty_tokens_are_public() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_auth_empty_tokens_are_public(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_star_token_visible_to_all() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_auth_star_token_visible_to_all(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn auth_latest_version_controls_visibility() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_auth_latest_version_controls_visibility(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn ordering_limit_truncates_in_insertion_order() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_ordering_limit_truncates_in_insertion_order(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn ordering_is_stable_across_repeated_queries() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_ordering_is_stable_across_repeated_queries(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn ordering_uses_resolved_version_position() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_ordering_uses_resolved_version_position(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn ordering_breaks_millisecond_ties_by_id() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_ordering_breaks_millisecond_ties_by_id(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn ordering_as_of_uses_version_resolved_at_cutoff() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_ordering_as_of_uses_version_resolved_at_cutoff(&f.searcher).await;
    f.cleanup().await;
}

#[tokio::test]
async fn result_carries_id_and_created_at() {
    let f = SearcherFixture::seeded().await;
    cert::test_searcher_result_carries_id_and_created_at(&f.searcher).await;
    f.cleanup().await;
}

/// Adapter-specific, and required by `sociallymeshy/docs/architecture.md` §2.3:
/// a payload field written bare, without the `payload.` prefix, must return
/// **zero** results end-to-end — not every record, which is how the SQL adapters
/// fail.
///
/// The pure version of this lives in `src/searcher.rs`; this one goes through a
/// real table so a future "optimisation" that pushes the predicate into a
/// `FilterExpression` cannot quietly change the failure mode.
#[tokio::test]
async fn bare_payload_key_returns_nothing_not_everything() {
    let f = SearcherFixture::seeded().await;
    let now = chrono::Utc::now().timestamp_millis();
    let star = vec!["*".to_string()];

    // Sanity: the correctly-prefixed query does find rows, so an empty result
    // below is about the prefix and not about an empty table.
    let prefixed = f
        .searcher
        .find_all(r#"{"payload.type": "typeA"}"#, &Stash::new(), &star, now)
        .await
        .unwrap();
    assert_eq!(prefixed.len(), 2, "the prefixed form must work");

    for template in [r#"{"type": "typeA"}"#, r#"{"kind": "tool"}"#] {
        let results = f
            .searcher
            .find_all(template, &Stash::new(), &star, now)
            .await
            .unwrap();
        assert!(
            results.is_empty(),
            "{template} must fail empty like merkql, not wide like the SQL adapters; \
             got {} results",
            results.len()
        );

        let one = f
            .searcher
            .find(template, &Stash::new(), &star, now)
            .await
            .unwrap();
        assert!(one.is_none(), "{template} must find nothing");
    }

    // A single bad condition poisons an otherwise-good query rather than being
    // skipped.
    let mixed = f
        .searcher
        .find_all(
            r#"{"payload.type": "typeA", "type": "typeA"}"#,
            &Stash::new(),
            &star,
            now,
        )
        .await
        .unwrap();
    assert!(mixed.is_empty(), "got {mixed:?}");

    f.cleanup().await;
}

/// The `id` pushdown must be a cost optimisation and nothing more: it has to
/// respect the *other* conditions in the template, and it must not resurrect a
/// deleted record.
#[tokio::test]
async fn id_pushdown_still_applies_the_other_conditions() {
    let f = SearcherFixture::seeded().await;
    let now = chrono::Utc::now().timestamp_millis();
    let star = vec!["*".to_string()];

    let mut args = Stash::new();
    args.insert("id".to_string(), json!("s-id-1"));

    let hit = f
        .searcher
        .find(
            r#"{"id": "{{id}}", "payload.name": "alpha"}"#,
            &args,
            &star,
            now,
        )
        .await
        .unwrap();
    assert!(hit.is_some(), "a matching extra condition must still match");

    let miss = f
        .searcher
        .find(
            r#"{"id": "{{id}}", "payload.name": "beta"}"#,
            &args,
            &star,
            now,
        )
        .await
        .unwrap();
    assert!(
        miss.is_none(),
        "the pushdown must not drop the remaining conditions"
    );

    f.cleanup().await;
}
