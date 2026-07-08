//! meshql-changes: thin change notifications for meshql deployments.
//!
//! A `ChangeSource` observes committed writes at the storage layer (CDC
//! model). `SearcherTail` is the portable, poll-based source that works
//! against any certified `Searcher`+`Repository` pair; native change-stream
//! sources slot in behind the same trait. A `ChangeHub` broadcasts events
//! to an SSE route (`changes_router`) that filters per subscriber by auth
//! tokens. Clients respond to notifications by refetching through the
//! normal graphlette — reads never bypass GraphQL.
//!
//! Design: docs/superpowers/specs/2026-07-07-meshql-changes-design.md

mod event;
mod source;
mod tail;
pub mod testing;

pub use event::ChangeEvent;
pub use source::ChangeSource;
pub use tail::SearcherTail;
