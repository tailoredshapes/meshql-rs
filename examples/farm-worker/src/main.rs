//! farm-worker — the shared, language-agnostic worker half of the merkql
//! CDC pipeline. Consumes lay_report events off a merkql topic (written by
//! meshql-changes' merkql sink, wired into examples/farm), looks up full
//! event detail via GraphQL, folds into hen_productivity, and writes the
//! result back via REST/GraphQL. Every endpoint this binary talks to is
//! config (env vars) — the SAME compiled binary points at a Rust, Java, or
//! TS farm deployment with no rebuild.
//!
//! Config: docs/superpowers/specs/2026-07-22-merkql-worker-pipeline-design.md
//! Env vars: see farm_worker::config::WorkerConfig::from_lookup.

use farm_worker::config::WorkerConfig;
use farm_worker::worker::run_forever;
use merkql::broker::{Broker, BrokerConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = WorkerConfig::from_env();
    println!(
        "[farm-worker] topic={} group={} source_graphql={} target_rest={} target_graphql={} merkql_dir={}",
        cfg.topic,
        cfg.group_id,
        cfg.source_graphql_base,
        cfg.target_rest_base,
        cfg.target_graphql_base,
        cfg.merkql_dir.display(),
    );

    let broker = Broker::open(BrokerConfig::new(&cfg.merkql_dir))?;
    let client = reqwest::Client::new();
    run_forever(broker, client, cfg).await;
    Ok(())
}
