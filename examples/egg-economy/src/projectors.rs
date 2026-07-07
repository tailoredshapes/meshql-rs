//! Projectors — the folds that translate business events (verbs) into domain
//! models (nouns). One projector owns one noun.
//!
//! Each projector is a pure fold: `reset` clears its state, `apply` folds one
//! event in, and `snapshot` renders the final payload for every noun id it has
//! seen. Because the whole thing is a fold over the ordered event log, replay
//! (fold from empty over history) and steady state are the *same computation* —
//! that is the property that makes projections disposable and rebuildable.

use crate::events::{verb, EventRecord};
use meshql_core::Stash;
use std::collections::{HashMap, HashSet};

/// A noun write produced by a projector: the domain id and its full payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Upsert {
    pub id: String,
    pub payload: Stash,
}

/// One projector builds one noun from the events it consumes.
pub trait Projector: Send {
    /// The noun mesh this projector writes (for wiring and logging).
    fn noun(&self) -> &'static str;
    /// Which verbs this projector folds.
    fn consumes(&self, verb: &str) -> bool;
    /// Drop all accumulated state (used before a replay).
    fn reset(&mut self);
    /// Fold one event into the accumulator.
    fn apply(&mut self, ev: &EventRecord);
    /// Render the final payload for every noun id seen so far.
    fn snapshot(&self) -> Vec<Upsert>;
}

fn stash(v: serde_json::Value) -> Stash {
    v.as_object().cloned().unwrap_or_default()
}

// ============================================================================
// Actor nouns
// ============================================================================

/// Builds an actor noun from a single "build" event by copying named fields.
/// Covers farm, coop, container, consumer — the create-once actors. The noun's
/// id is the id of the event that created it, so references are stable and
/// replay-deterministic.
pub struct BuildProjector {
    noun: &'static str,
    verb: &'static str,
    fields: &'static [&'static str],
    state: HashMap<String, Stash>,
}

impl BuildProjector {
    pub fn farm() -> Self {
        Self::new("farm", verb::BUILD_FARM, &["name", "farm_type", "zone", "owner"])
    }
    pub fn coop() -> Self {
        Self::new("coop", verb::BUILD_COOP, &["farm_id", "name", "capacity", "coop_type"])
    }
    pub fn container() -> Self {
        Self::new(
            "container",
            verb::BUILD_CONTAINER,
            &["name", "container_type", "capacity", "zone"],
        )
    }
    pub fn consumer() -> Self {
        Self::new(
            "consumer",
            verb::REGISTER_CONSUMER,
            &["name", "consumer_type", "zone", "weekly_demand"],
        )
    }

    fn new(noun: &'static str, verb: &'static str, fields: &'static [&'static str]) -> Self {
        Self { noun, verb, fields, state: HashMap::new() }
    }
}

impl Projector for BuildProjector {
    fn noun(&self) -> &'static str {
        self.noun
    }
    fn consumes(&self, v: &str) -> bool {
        v == self.verb
    }
    fn reset(&mut self) {
        self.state.clear();
    }
    fn apply(&mut self, ev: &EventRecord) {
        let mut p = Stash::new();
        for f in self.fields {
            if let Some(val) = ev.payload.get(*f) {
                p.insert((*f).to_string(), val.clone());
            }
        }
        self.state.insert(ev.id.clone(), p);
    }
    fn snapshot(&self) -> Vec<Upsert> {
        self.state
            .iter()
            .map(|(id, payload)| Upsert { id: id.clone(), payload: payload.clone() })
            .collect()
    }
}

/// Builds the `hen` noun. A single `buy_hens` event with `count = N` fans out
/// to N hen nouns (deterministic ids `{event_id}-{i}`); later `move_hen_to_coop`
/// and `retire_hen` events mutate the hens they name.
#[derive(Default)]
pub struct HenProjector {
    state: HashMap<String, Stash>,
}

impl HenProjector {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Projector for HenProjector {
    fn noun(&self) -> &'static str {
        "hen"
    }
    fn consumes(&self, v: &str) -> bool {
        matches!(v, verb::BUY_HENS | verb::MOVE_HEN_TO_COOP | verb::RETIRE_HEN)
    }
    fn reset(&mut self) {
        self.state.clear();
    }
    fn apply(&mut self, ev: &EventRecord) {
        match ev.verb.as_str() {
            verb::BUY_HENS => {
                let count = ev.int("count").max(0);
                let breed = ev.str("breed").unwrap_or("mixed");
                for i in 0..count {
                    let hen_id = format!("{}-{}", ev.id, i);
                    self.state.insert(
                        hen_id,
                        stash(serde_json::json!({
                            "name": format!("{} {}", breed, i + 1),
                            "breed": breed,
                            "coop_id": ev.str("coop_id").unwrap_or(""),
                            "farm_id": ev.str("farm_id").unwrap_or(""),
                            "status": "active",
                        })),
                    );
                }
            }
            verb::MOVE_HEN_TO_COOP => {
                if let Some(hen_id) = ev.str("hen_id") {
                    if let Some(hen) = self.state.get_mut(hen_id) {
                        hen.insert(
                            "coop_id".to_string(),
                            serde_json::Value::String(ev.str("coop_id").unwrap_or("").to_string()),
                        );
                    }
                }
            }
            verb::RETIRE_HEN => {
                if let Some(hen_id) = ev.str("hen_id") {
                    if let Some(hen) = self.state.get_mut(hen_id) {
                        hen.insert(
                            "status".to_string(),
                            serde_json::Value::String("retired".to_string()),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    fn snapshot(&self) -> Vec<Upsert> {
        self.state
            .iter()
            .map(|(id, payload)| Upsert { id: id.clone(), payload: payload.clone() })
            .collect()
    }
}

// ============================================================================
// Analytic nouns
// ============================================================================

/// One recorded lay, kept so windowed aggregates can be computed deterministically
/// against the latest event time rather than wall-clock.
struct Lay {
    ms: i64,
    eggs: i64,
    grade_a: bool,
}

const DAY_MS: i64 = 86_400_000;

/// Builds `hen_productivity` by folding `eggs_laid` events per hen.
#[derive(Default)]
pub struct HenProductivityProjector {
    lays: HashMap<String, Vec<Lay>>,
    farm_of: HashMap<String, String>,
}

impl HenProductivityProjector {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Projector for HenProductivityProjector {
    fn noun(&self) -> &'static str {
        "hen_productivity"
    }
    fn consumes(&self, v: &str) -> bool {
        v == verb::EGGS_LAID
    }
    fn reset(&mut self) {
        self.lays.clear();
        self.farm_of.clear();
    }
    fn apply(&mut self, ev: &EventRecord) {
        let hen_id = match ev.str("hen_id") {
            Some(h) => h.to_string(),
            None => return,
        };
        if let Some(f) = ev.str("farm_id") {
            self.farm_of.insert(hen_id.clone(), f.to_string());
        }
        self.lays.entry(hen_id).or_default().push(Lay {
            ms: ev.created_at_ms,
            eggs: ev.int("eggs"),
            grade_a: ev.str("quality") == Some("grade_a"),
        });
    }
    fn snapshot(&self) -> Vec<Upsert> {
        self.lays
            .iter()
            .map(|(hen_id, lays)| {
                let latest = lays.iter().map(|l| l.ms).max().unwrap_or(0);
                let earliest = lays.iter().map(|l| l.ms).min().unwrap_or(latest);
                let total: i64 = lays.iter().map(|l| l.eggs).sum();
                let grade_a: i64 = lays.iter().filter(|l| l.grade_a).map(|l| l.eggs).sum();
                let window = |days: i64| -> i64 {
                    lays.iter()
                        .filter(|l| l.ms >= latest - days * DAY_MS)
                        .map(|l| l.eggs)
                        .sum()
                };
                let weeks = ((latest - earliest) as f64 / (7.0 * DAY_MS as f64)).max(1.0);
                Upsert {
                    id: hen_id.clone(),
                    payload: stash(serde_json::json!({
                        "hen_id": hen_id,
                        "farm_id": self.farm_of.get(hen_id).cloned().unwrap_or_default(),
                        "eggs_today": window(1),
                        "eggs_week": window(7),
                        "eggs_month": window(30),
                        "avg_per_week": (total as f64 / weeks * 100.0).round() / 100.0,
                        "total_eggs": total,
                        "quality_rate": if total == 0 { 0.0 }
                            else { (grade_a as f64 / total as f64 * 1000.0).round() / 1000.0 },
                    })),
                }
            })
            .collect()
    }
}

/// Builds `farm_output` by folding `eggs_laid` events per farm.
#[derive(Default)]
pub struct FarmOutputProjector {
    detail: HashMap<String, Vec<Lay>>,
    hens: HashMap<String, HashSet<String>>,
}

impl FarmOutputProjector {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Projector for FarmOutputProjector {
    fn noun(&self) -> &'static str {
        "farm_output"
    }
    fn consumes(&self, v: &str) -> bool {
        v == verb::EGGS_LAID
    }
    fn reset(&mut self) {
        self.detail.clear();
        self.hens.clear();
    }
    fn apply(&mut self, ev: &EventRecord) {
        let farm_id = match ev.str("farm_id") {
            Some(f) => f.to_string(),
            None => return,
        };
        self.detail.entry(farm_id.clone()).or_default().push(Lay {
            ms: ev.created_at_ms,
            eggs: ev.int("eggs"),
            grade_a: ev.str("quality") == Some("grade_a"),
        });
        if let Some(h) = ev.str("hen_id") {
            self.hens.entry(farm_id).or_default().insert(h.to_string());
        }
    }
    fn snapshot(&self) -> Vec<Upsert> {
        self.detail
            .iter()
            .map(|(farm_id, lays)| {
                let latest = lays.iter().map(|l| l.ms).max().unwrap_or(0);
                let window = |days: i64| -> i64 {
                    lays.iter()
                        .filter(|l| l.ms >= latest - days * DAY_MS)
                        .map(|l| l.eggs)
                        .sum()
                };
                let hens = self.hens.get(farm_id).map(|s| s.len()).unwrap_or(0) as i64;
                let week = window(7);
                Upsert {
                    id: farm_id.clone(),
                    payload: stash(serde_json::json!({
                        "farm_id": farm_id,
                        "eggs_today": window(1),
                        "eggs_week": week,
                        "eggs_month": window(30),
                        "active_hens": hens,
                        "total_hens": hens,
                        "avg_per_hen_per_week": if hens == 0 { 0.0 }
                            else { (week as f64 / hens as f64 * 100.0).round() / 100.0 },
                    })),
                }
            })
            .collect()
    }
}

/// Signed running balance of a container.
#[derive(Default, Clone)]
struct Inv {
    deposits: i64,
    withdrawals: i64,
    transfers_in: i64,
    transfers_out: i64,
    consumed: i64,
}

/// Builds `container_inventory` by folding every egg-flow event that touches a
/// container. `eggs_transferred` touches two containers, so it emits two upserts.
#[derive(Default)]
pub struct ContainerInventoryProjector {
    state: HashMap<String, Inv>,
}

impl ContainerInventoryProjector {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Projector for ContainerInventoryProjector {
    fn noun(&self) -> &'static str {
        "container_inventory"
    }
    fn consumes(&self, v: &str) -> bool {
        matches!(
            v,
            verb::EGGS_STORED | verb::EGGS_WITHDRAWN | verb::EGGS_TRANSFERRED | verb::EGGS_CONSUMED
        )
    }
    fn reset(&mut self) {
        self.state.clear();
    }
    fn apply(&mut self, ev: &EventRecord) {
        let eggs = ev.int("eggs");
        match ev.verb.as_str() {
            verb::EGGS_STORED => {
                if let Some(c) = ev.str("container_id") {
                    self.state.entry(c.to_string()).or_default().deposits += eggs;
                }
            }
            verb::EGGS_WITHDRAWN => {
                if let Some(c) = ev.str("container_id") {
                    self.state.entry(c.to_string()).or_default().withdrawals += eggs;
                }
            }
            verb::EGGS_CONSUMED => {
                if let Some(c) = ev.str("container_id") {
                    self.state.entry(c.to_string()).or_default().consumed += eggs;
                }
            }
            verb::EGGS_TRANSFERRED => {
                if let Some(src) = ev.str("source_container_id") {
                    self.state.entry(src.to_string()).or_default().transfers_out += eggs;
                }
                if let Some(dst) = ev.str("dest_container_id") {
                    self.state.entry(dst.to_string()).or_default().transfers_in += eggs;
                }
            }
            _ => {}
        }
    }
    fn snapshot(&self) -> Vec<Upsert> {
        self.state
            .iter()
            .map(|(container_id, inv)| {
                let current = inv.deposits + inv.transfers_in
                    - inv.withdrawals
                    - inv.transfers_out
                    - inv.consumed;
                Upsert {
                    id: container_id.clone(),
                    payload: stash(serde_json::json!({
                        "container_id": container_id,
                        "current_eggs": current.max(0),
                        "total_deposits": inv.deposits,
                        "total_withdrawals": inv.withdrawals,
                        "total_transfers_in": inv.transfers_in,
                        "total_transfers_out": inv.transfers_out,
                        "total_consumed": inv.consumed,
                    })),
                }
            })
            .collect()
    }
}

/// Every projector in the system — one per noun.
pub fn all_projectors() -> Vec<Box<dyn Projector>> {
    vec![
        Box::new(BuildProjector::farm()),
        Box::new(BuildProjector::coop()),
        Box::new(HenProjector::new()),
        Box::new(BuildProjector::container()),
        Box::new(BuildProjector::consumer()),
        Box::new(HenProductivityProjector::new()),
        Box::new(ContainerInventoryProjector::new()),
        Box::new(FarmOutputProjector::new()),
    ]
}
