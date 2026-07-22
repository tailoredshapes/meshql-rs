//! farm-worker — the shared, language-agnostic worker half of the merkql
//! CDC pipeline (see `main.rs` for the binary entry point and the design
//! doc it links). Points at any farm deployment (Rust, Java, or TS) purely
//! via env vars, with no rebuild. The modules split along the pipeline's
//! two halves plus the config that drives both:
//!
//! - `config` — env-driven `WorkerConfig`/`QueryDialect`, read once at
//!   startup and threaded through everything below.
//! - `event`, `graphql`, `detail` — the read side: a thin `ThinEvent`
//!   notification off the merkql topic, resolved into full lay_report
//!   detail via a GraphQL fetch against the source deployment.
//! - `productivity` — the pure, idempotent fold from a lay_report into an
//!   updated `hen_productivity` record; safe to re-run on redelivery.
//! - `rest_client` — the write side: reads current `hen_productivity` and
//!   writes the recomputed result back via REST/GraphQL against the
//!   target deployment.
//! - `worker` — the consumer loop (`process_batch`/`run_forever`) that
//!   ties the above together: poll, fold, write, commit.
pub mod config;
pub mod detail;
pub mod event;
pub mod graphql;
pub mod productivity;
pub mod rest_client;
pub mod worker;
