use crate::ChangeEvent;
use async_trait::async_trait;

/// Something that observes committed writes for one entity and yields them
/// as change events. Promotion of egg-economy's `EventSource` (see
/// examples/egg-economy/src/source.rs for the CDC rationale: derive from
/// the committed store, never the request path — no dual write).
///
/// Delivery contract: at-least-once, per-entity ordered by `created_at`.
/// Consumers tolerate duplicates because the client response is an
/// idempotent refetch.
#[async_trait]
pub trait ChangeSource: Send + Sync {
    /// The entity this source tails (e.g. "hen").
    fn entity(&self) -> &str;
    /// Changes committed since the last poll.
    async fn poll(&self) -> anyhow::Result<Vec<ChangeEvent>>;
}
