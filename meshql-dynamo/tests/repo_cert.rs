//! Repository certification, DynamoDB adapter.
//! Requires DynamoDB Local at `MESHQL_DYNAMO_ENDPOINT` (default
//! `http://localhost:8123`).
//!
//! Every case runs **twice** — once against a plain table and once against one
//! carrying the indexes derived from `common::cert_config`. The repository's
//! only index-related job is promoting `ix_` attributes on write, and the write
//! path must be indifferent to it: same envelopes in, same envelopes out. See
//! `common`.

#[macro_use]
mod common;

use common::{Indexing, RepoFixture};
use meshql_core::testing as cert;

cert_case!(
    create_should_store_and_return_envelope,
    RepoFixture,
    cert::test_create_should_store_and_return_envelope
);
cert_case!(
    read_should_retrieve_existing_envelope,
    RepoFixture,
    cert::test_read_should_retrieve_existing_envelope
);
cert_case!(
    list_should_retrieve_all_created_envelopes,
    RepoFixture,
    cert::test_list_should_retrieve_all_created_envelopes
);
cert_case!(
    remove_should_delete_envelope,
    RepoFixture,
    cert::test_remove_should_delete_envelope
);
cert_case!(
    create_many_should_store_multiple_envelopes,
    RepoFixture,
    cert::test_create_many_should_store_multiple_envelopes
);
cert_case!(
    read_many_should_retrieve_multiple_envelopes,
    RepoFixture,
    cert::test_read_many_should_retrieve_multiple_envelopes
);
cert_case!(
    remove_many_should_delete_multiple_envelopes,
    RepoFixture,
    cert::test_remove_many_should_delete_multiple_envelopes
);
cert_case!(
    should_allow_multiple_versions_and_temporal_reads,
    RepoFixture,
    cert::test_temporal_versioning
);
cert_case!(
    should_only_list_latest_version,
    RepoFixture,
    cert::test_list_shows_only_latest_version
);
cert_case!(
    should_not_list_deleted_envelope_with_prior_version,
    RepoFixture,
    cert::test_list_excludes_deleted_envelope_with_prior_version
);
