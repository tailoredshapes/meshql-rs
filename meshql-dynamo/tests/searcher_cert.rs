//! Searcher certification, DynamoDB adapter — the base suite plus the
//! authorization, ordering and result-shape certs, matching
//! `meshql-sqlite/tests/searcher_cert.rs` function for function.
//!
//! # Every case, twice
//!
//! Once against a plain table, where a non-`id` search is a `Scan`, and once
//! against one carrying the indexes derived from `common::cert_config`, where
//! the same search is a two-phase GSI query. This is the suite where that
//! matters most: an index holds *versions*, and the whole risk of using one is
//! that a superseded version leaks back into a result set. Running these
//! against only the scan path would certify the half of the adapter that cannot
//! exhibit the bug.
//!
//! The failure is not hypothetical. Resolving latest-per-id *inside* the index
//! results — the obvious way to save a round trip — was measured against real
//! AWS returning `{stayer, mover}` where the correct answer is `{stayer}`, and
//! `the_index_cannot_resurrect_a_superseded_version` below is that case pinned
//! on the shipped path.

#[macro_use]
mod common;

use common::{Indexing, SearcherFixture};
use meshql_core::testing as cert;
use meshql_core::{Searcher, Stash};
use serde_json::json;

cert_case!(
    should_return_empty_for_nonexistent_id,
    SearcherFixture,
    cert::test_searcher_empty_result_for_nonexistent
);
cert_case!(
    should_find_by_id,
    SearcherFixture,
    cert::test_searcher_find_by_id
);
cert_case!(
    should_find_by_name,
    SearcherFixture,
    cert::test_searcher_find_by_name
);
cert_case!(
    should_find_all_by_type,
    SearcherFixture,
    cert::test_searcher_find_all_by_type
);
cert_case!(
    should_find_all_by_type_and_name,
    SearcherFixture,
    cert::test_searcher_find_all_by_type_and_name
);
cert_case!(
    should_return_empty_for_nonexistent_type,
    SearcherFixture,
    cert::test_searcher_empty_array_for_nonexistent_type
);
cert_case!(
    should_respect_limit,
    SearcherFixture,
    cert::test_searcher_respects_limit
);
cert_case!(
    should_handle_empty_query,
    SearcherFixture,
    cert::test_searcher_empty_query
);
cert_case!(
    auth_wildcard_caller_sees_all,
    SearcherFixture,
    cert::test_searcher_auth_wildcard_caller_sees_all
);
cert_case!(
    auth_restricted_caller_sees_only_intersecting,
    SearcherFixture,
    cert::test_searcher_auth_restricted_caller_sees_only_intersecting
);
cert_case!(
    auth_denies_non_intersecting,
    SearcherFixture,
    cert::test_searcher_auth_denies_non_intersecting
);
cert_case!(
    auth_empty_tokens_are_public,
    SearcherFixture,
    cert::test_searcher_auth_empty_tokens_are_public
);
cert_case!(
    auth_star_token_visible_to_all,
    SearcherFixture,
    cert::test_searcher_auth_star_token_visible_to_all
);
cert_case!(
    auth_latest_version_controls_visibility,
    SearcherFixture,
    cert::test_searcher_auth_latest_version_controls_visibility
);
cert_case!(
    ordering_limit_truncates_in_insertion_order,
    SearcherFixture,
    cert::test_searcher_ordering_limit_truncates_in_insertion_order
);
cert_case!(
    ordering_is_stable_across_repeated_queries,
    SearcherFixture,
    cert::test_searcher_ordering_is_stable_across_repeated_queries
);
cert_case!(
    ordering_uses_resolved_version_position,
    SearcherFixture,
    cert::test_searcher_ordering_uses_resolved_version_position
);
cert_case!(
    ordering_breaks_millisecond_ties_by_id,
    SearcherFixture,
    cert::test_searcher_ordering_breaks_millisecond_ties_by_id
);
cert_case!(
    ordering_as_of_uses_version_resolved_at_cutoff,
    SearcherFixture,
    cert::test_searcher_ordering_as_of_uses_version_resolved_at_cutoff
);
cert_case!(
    result_carries_id_and_created_at,
    SearcherFixture,
    cert::test_searcher_result_carries_id_and_created_at
);

/// Adapter-specific, and required by `sociallymeshy/docs/architecture.md` §2.3:
/// a payload field written bare, without the `payload.` prefix, must return
/// **zero** results end-to-end — not every record, which is how the SQL adapters
/// fail.
///
/// The pure version of this lives in `src/searcher.rs`; this one goes through a
/// real table so a future "optimisation" that pushes the predicate into a
/// `FilterExpression` cannot quietly change the failure mode. Indexed, the same
/// query is answered from the plan with no request at all — which must be the
/// same *answer*, not merely a cheaper one.
async fn bare_payload_key_case(indexing: Indexing) {
    let f = SearcherFixture::seeded(indexing).await;
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

#[tokio::test]
async fn bare_payload_key_returns_nothing_not_everything_unindexed() {
    bare_payload_key_case(Indexing::Off).await;
}

#[tokio::test]
async fn bare_payload_key_returns_nothing_not_everything_indexed() {
    bare_payload_key_case(Indexing::On).await;
}

/// The `id` pushdown must be a cost optimisation and nothing more: it has to
/// respect the *other* conditions in the template, and it must not resurrect a
/// deleted record.
async fn id_pushdown_case(indexing: Indexing) {
    let f = SearcherFixture::seeded(indexing).await;
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

#[tokio::test]
async fn id_pushdown_still_applies_the_other_conditions_unindexed() {
    id_pushdown_case(Indexing::Off).await;
}

#[tokio::test]
async fn id_pushdown_still_applies_the_other_conditions_indexed() {
    id_pushdown_case(Indexing::On).await;
}

/// **The reason phase 2 exists**, on the shipped path.
///
/// A record whose indexed field changes leaves its old value in the index
/// forever — a GSI is keyed on the *version*, not on the record. So a query for
/// the old value still finds a candidate, and the only thing that can reject it
/// is re-reading the id from the base table and testing the predicate against
/// the version that resolution returns.
///
/// The shortcut this rules out — resolve latest-per-id *inside* the index
/// results, then re-check — looks equivalent and is not: `mover`'s current
/// version lives in a different partition of the index, so it is not in the
/// result set to be re-checked. Measured against real AWS returning
/// `{stayer, mover}` where the answer is `{stayer}`; see
/// `docs/cost-model-dynamodb.md` §6.
///
/// It is the searcher's analogue of
/// `test_searcher_auth_latest_version_controls_visibility`, and it is written to
/// fail loudly rather than to pass vacuously: the `stayer` assertion means an
/// implementation that returned nothing at all could not sneak through.
#[tokio::test]
async fn the_index_cannot_resurrect_a_superseded_version() {
    use meshql_core::{Envelope, Repository};

    let f = SearcherFixture::new(Indexing::On).await;
    let star = vec!["*".to_string()];

    let envelope = |id: &str, kind: &str| {
        let mut payload = Stash::new();
        payload.insert("type".to_string(), json!(kind));
        payload.insert("name".to_string(), json!(id));
        Envelope::new(id, payload, star.clone())
    };

    // "mover" is typeA, then becomes typeB. "stayer" never moves.
    f.repo
        .create(envelope("mover", "typeA"), &star)
        .await
        .unwrap();
    f.repo
        .create(envelope("stayer", "typeA"), &star)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    f.repo
        .create(envelope("mover", "typeB"), &star)
        .await
        .unwrap();

    let now = chrono::Utc::now().timestamp_millis();
    let found = f
        .searcher
        .find_all(r#"{"payload.type": "typeA"}"#, &Stash::new(), &star, now)
        .await
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|s| s["id"].as_str().unwrap()).collect();

    assert!(
        ids.contains(&"stayer"),
        "a record that never changed must still be found — otherwise this test \
         passes against an implementation that returns nothing. Got {ids:?}"
    );
    assert!(
        !ids.contains(&"mover"),
        "the index still holds mover's typeA version, and always will. Only \
         re-resolving the id from the base table can reject it — this is what \
         phase 2 is for. Got {ids:?}"
    );

    // ...and it is found under its *current* type, so the index is genuinely
    // being read rather than the whole thing quietly falling back to a scan
    // that happens to be right.
    let moved = f
        .searcher
        .find_all(r#"{"payload.type": "typeB"}"#, &Stash::new(), &star, now)
        .await
        .unwrap();
    let ids: Vec<&str> = moved.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["mover"], "got {ids:?}");

    f.cleanup().await;
}

/// Guard: with a plan attached, a template naming a field the plan does not
/// cover is an **error**, not a quiet `Scan`.
///
/// `payload.zone` is not in `common::cert_config`, so an indexed searcher must
/// refuse it — and the same template against an unindexed searcher must still
/// work, because that adapter never claimed to be indexed.
#[tokio::test]
async fn an_unindexed_field_is_refused_rather_than_scanned() {
    let indexed = SearcherFixture::seeded(Indexing::On).await;
    let now = chrono::Utc::now().timestamp_millis();
    let star = vec!["*".to_string()];

    let err = indexed
        .searcher
        .find_all(r#"{"payload.zone": "north"}"#, &Stash::new(), &star, now)
        .await
        .expect_err("an unindexed field must not silently become an O(V) Scan");
    let message = err.to_string();
    assert!(message.contains("zone"), "name the field: {message}");
    assert!(
        message.contains("payload.zone"),
        "name the template: {message}"
    );
    indexed.cleanup().await;

    // The unindexed adapter is unchanged: it scans, and it answers.
    let plain = SearcherFixture::seeded(Indexing::Off).await;
    let ok = plain
        .searcher
        .find_all(r#"{"payload.zone": "north"}"#, &Stash::new(), &star, now)
        .await
        .expect("the unindexed adapter still scans");
    assert!(ok.is_empty());
    plain.cleanup().await;
}
