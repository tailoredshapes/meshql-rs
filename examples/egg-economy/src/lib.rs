//! egg-economy: a fully event-sourced meshql system.
//!
//! The only write surface is the business-event log (the verbs in [`events`]).
//! Every domain model — the actor nouns (farm, coop, hen, container, consumer)
//! and the analytic nouns (hen_productivity, container_inventory, farm_output) —
//! is materialized by a [`worker::Worker`] folding events, never written
//! directly. Events reach the workers from the storage layer via a CDC
//! connector ([`source`]), so nothing lives in the application write path.
//!
//! ```text
//!   POST /<verb>/api ──► event mesh (Mongo) ──► CDC connector ──► egg-events topic
//!                                                                       │
//!                            noun mesh (Mongo) ◄── worker (fold) ◄──────┘
//!                                    │
//!                            GET /<noun>/graph
//! ```

pub mod events;
pub mod manifest;
pub mod projectors;
pub mod source;
pub mod worker;

pub use events::{EventRecord, ALL_VERBS, TOPIC};
pub use projectors::{all_projectors, Projector, Upsert};
pub use source::{publish, run_connector, EventSource, RepositoryTail};
pub use worker::Worker;
