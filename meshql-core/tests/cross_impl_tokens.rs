//! The version token must be identical in Rust, Java and TypeScript.
//!
//! A version URL is derived from content precisely so it survives a move
//! between stores — and, by the same argument, between implementations. These
//! five envelopes are pinned identically in
//! `meshql-java/core/src/test/java/com/meshql/core/VersionsTest.java` and
//! `meshobj/core/common/test/versions.spec.ts`. If one implementation's
//! encoding drifts, exactly one of the three suites goes red, which is the
//! point of pinning the digest rather than the algorithm.

use chrono::{TimeZone, Utc};
use meshql_core::{version_token, Envelope};

fn envelope(
    id: &str,
    ms: i64,
    deleted: bool,
    mark: Vec<&str>,
    payload: serde_json::Value,
) -> Envelope {
    let mut e = Envelope::new(
        id.to_string(),
        payload.as_object().cloned().unwrap_or_default(),
        mark.into_iter().map(String::from).collect::<Vec<String>>(),
    );
    e.created_at = Utc.timestamp_millis_opt(ms).unwrap();
    e.deleted = deleted;
    e
}

#[test]
fn tokens_match_the_other_implementations_byte_for_byte() {
    assert_eq!(
        version_token(&envelope(
            "d1",
            1000,
            false,
            vec!["*"],
            serde_json::json!({"name": "Auth", "tier": "prod"})
        )),
        "178d0564f9eb77a977ea9ff0eb0836917413ba5dc2e6d5bd6b416883cdfb690b"
    );

    // A nested object written out of key order, beside an array whose order is
    // data and must survive.
    assert_eq!(
        version_token(&envelope(
            "d1",
            1000,
            false,
            vec!["*"],
            serde_json::json!({"meta": {"y": 2, "x": 1}, "a": 1, "tags": ["z", "a"]})
        )),
        "0a848449f0854ce7140dbcf7cdb74db6c8e5175675e5057d1ef1d30c913d6007"
    );

    // A two-part mark, given out of order: the mark is sorted before hashing.
    assert_eq!(
        version_token(&envelope(
            "w7",
            1730000000123,
            false,
            vec!["tenant-b", "tenant-a"],
            serde_json::json!({"kind": "tool", "name": "alpha"})
        )),
        "fb3f0e8650898e0157596385e0bc93310a43d788ac130bc64fefb42d9e4fb2ef"
    );

    // The tombstone of that same document.
    assert_eq!(
        version_token(&envelope(
            "w7",
            1730000000123,
            true,
            vec!["tenant-a"],
            serde_json::json!({"kind": "tool", "name": "alpha"})
        )),
        "233cf2ad2994b27dfaf616f946efb0941e08abf4c512fb1d3cbe89058d034e05"
    );

    // An empty mark, and a payload mixing an integer with a float.
    assert_eq!(
        version_token(&envelope(
            "w7",
            1730000000123,
            false,
            vec![],
            serde_json::json!({"kind": "tool", "n": 1.5, "big": 42})
        )),
        "83e33beb1d3fd11e3d1bd002bd59976b099ac65ecd694ade0a16633204c75c82"
    );
}
