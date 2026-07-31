//! Repository *authorization* certification, DynamoDB adapter.
//!
//! The rest of the repository suite creates every envelope with `"*"` tokens, so
//! an adapter that ignored `tokens` entirely would pass it. These close that gap.
//!
//! Both the plain and the indexed table, because `list(tokens)` is the one read
//! path indexing deliberately does **not** touch: `authorized_tokens` is a list
//! attribute and a GSI key must be scalar, so token visibility is not indexable
//! and `list` stays a `Scan`. These cases are what would catch an "optimisation"
//! that changed that.

#[macro_use]
mod common;

use common::{Indexing, RepoAuthFixture};
use meshql_core::testing as cert;

cert_case!(
    auth_wildcard_caller_sees_all,
    RepoAuthFixture,
    cert::test_repository_auth_wildcard_caller_sees_all
);
cert_case!(
    auth_restricted_caller_sees_only_intersecting,
    RepoAuthFixture,
    cert::test_repository_auth_restricted_caller_sees_only_intersecting
);
cert_case!(
    auth_denies_non_intersecting,
    RepoAuthFixture,
    cert::test_repository_auth_denies_non_intersecting
);
cert_case!(
    auth_empty_tokens_are_public,
    RepoAuthFixture,
    cert::test_repository_auth_empty_tokens_are_public
);
cert_case!(
    auth_star_token_visible_to_all,
    RepoAuthFixture,
    cert::test_repository_auth_star_token_visible_to_all
);
cert_case!(
    auth_latest_version_controls_visibility,
    RepoAuthFixture,
    cert::test_repository_auth_latest_version_controls_visibility
);
