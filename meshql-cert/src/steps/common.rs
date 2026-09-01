//! Steps shared by more than one certification feature.
//!
//! Every step module in this crate is linked into every cucumber runner, so a
//! step defined twice makes cucumber report an ambiguous match and fail. The
//! timestamp and wait steps appear in both `repository.feature` and
//! `farm.feature`, so they live here rather than in either module. This mirrors
//! `meshobj/core/cert/src/steps/common_steps.ts`.

use chrono::Utc;
use cucumber::{given, then, when};

use crate::world::CertWorld;

/// Record the current instant under a label that a later step reads back.
///
/// The farm and egg-economy features write through HTTP before checking a
/// temporal read, so the capture also pins `first_stamp_ms` for the two labels
/// those features use, and pauses long enough that the instant it captured
/// sorts strictly before whatever the next step writes.
#[given(regex = r#"^I capture the current timestamp as "([^"]+)"$"#)]
#[when(regex = r#"^I capture the current timestamp as "([^"]+)"$"#)]
#[then(regex = r#"^I capture the current timestamp as "([^"]+)"$"#)]
async fn capture_timestamp(world: &mut CertWorld, key: String) {
    let ms = Utc::now().timestamp_millis();
    if key == "first_stamp" || key == "before_update" {
        world.first_stamp_ms = Some(ms);
    }
    world.timestamps.insert(key, Utc::now());
    // Separate this instant from whatever the next step writes.
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
}

#[given(regex = r"^I wait (\d+) milliseconds$")]
#[when(regex = r"^I wait (\d+) milliseconds$")]
#[then(regex = r"^I wait (\d+) milliseconds$")]
async fn wait_milliseconds(_world: &mut CertWorld, millis: u64) {
    tokio::time::sleep(tokio::time::Duration::from_millis(millis)).await;
}
