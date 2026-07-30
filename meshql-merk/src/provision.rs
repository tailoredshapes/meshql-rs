//! Explicit topic provisioning.
//!
//! `BrokerConfig::new()` sets `default_partitions: 1` and
//! `auto_create_topics: true`, so the **first `send` to an unknown topic
//! silently creates it with one partition** and nobody chose that. Partition
//! count is immutable — routing is `hash(key) % num_partitions`, changing the
//! modulus remaps every key, and `create_topic` refuses to reshape an existing
//! topic — so that accident is a one-way door. On merk-cloud one partition is
//! also the pathological configuration: a partition is a serial resource and a
//! many-writer gateway needs the load spread, not funnelled.
//!
//! Hence: a plan, read from a file, applied by an idempotent call, with a test
//! that asserts the counts. The file is the record of a decision that cannot be
//! revisited later.

use merk_object::backend::Backend;
use merk_object::broker::BrokerRef;
use meshql_core::{MeshqlError, Result};
use serde::Deserialize;

/// One topic and the partition count chosen for it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TopicSpec {
    pub name: String,
    pub partitions: u32,
    /// Why this number. Not decoration: the count cannot be changed later, so
    /// the reasoning is the only thing a future reader has to go on.
    #[serde(default)]
    pub reason: String,
}

/// Every topic a deployment provisions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TopicPlan {
    #[serde(rename = "topic", default)]
    pub topics: Vec<TopicSpec>,
}

impl TopicPlan {
    /// Parse a `topics.toml` of the form
    ///
    /// ```toml
    /// [[topic]]
    /// name = "story_event"
    /// partitions = 8
    /// reason = "highest-volume topic"
    /// ```
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let plan: TopicPlan =
            toml::from_str(text).map_err(|e| MeshqlError::Parse(e.to_string()))?;
        plan.validate()?;
        Ok(plan)
    }

    fn validate(&self) -> Result<()> {
        if self.topics.is_empty() {
            return Err(MeshqlError::Validation(
                "topic plan is empty: a plan that provisions nothing lets \
                 auto-creation pick one partition per topic instead"
                    .into(),
            ));
        }
        for spec in &self.topics {
            if spec.partitions == 0 {
                return Err(MeshqlError::Validation(format!(
                    "topic '{}' asks for 0 partitions",
                    spec.name
                )));
            }
        }
        let mut names: Vec<&str> = self.topics.iter().map(|t| t.name.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        if names.len() != before {
            return Err(MeshqlError::Validation(
                "topic plan names a topic twice; the second count would be silently ignored".into(),
            ));
        }
        Ok(())
    }

    pub fn partitions_for(&self, topic: &str) -> Option<u32> {
        self.topics
            .iter()
            .find(|t| t.name == topic)
            .map(|t| t.partitions)
    }

    pub fn total_partitions(&self) -> u32 {
        self.topics.iter().map(|t| t.partitions).sum()
    }
}

/// Apply a plan. Idempotent: `create_topic` leaves an existing topic alone.
///
/// It does **not** reshape, and that silence is upstream behaviour rather than a
/// choice here — so this also reports back what each topic actually has, which
/// is the only way to notice that a topic was auto-created with one partition
/// before the provisioner ever ran.
pub fn provision<B: Backend>(
    broker: &BrokerRef<B>,
    plan: &TopicPlan,
) -> Result<Vec<(String, u32)>> {
    let mut actual = Vec::with_capacity(plan.topics.len());
    for spec in &plan.topics {
        broker
            .create_topic(&spec.name, spec.partitions)
            .map_err(|e| MeshqlError::Storage(e.to_string()))?;

        let observed = broker
            .topic(&spec.name)
            .ok_or_else(|| {
                MeshqlError::Storage(format!("topic '{}' missing after create", spec.name))
            })?
            .partition_ids()
            .len() as u32;

        if observed != spec.partitions {
            return Err(MeshqlError::Storage(format!(
                "topic '{}' has {observed} partitions, not the {} the plan asks for. \
                 create_topic does not reshape an existing topic, and partition count is \
                 immutable, so this cannot be corrected in place: the fix is a new topic \
                 name and a replay.",
                spec.name, spec.partitions
            )));
        }
        actual.push((spec.name.clone(), observed));
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    use merk_object::broker::BrokerConfig;
    use merk_object::mem::broker::Broker;

    const PLAN: &str = r#"
[[topic]]
name = "story_event"
partitions = 8
reason = "highest-volume topic"

[[topic]]
name = "moderation_event"
partitions = 2
reason = "human-rate, low volume, high consequence"
"#;

    fn open(location: &str) -> BrokerRef<merk_object::memory::MemoryBackend> {
        Broker::open(BrokerConfig::new(location)).unwrap()
    }

    #[test]
    fn parses_a_plan() {
        let plan = TopicPlan::from_toml_str(PLAN).unwrap();
        assert_eq!(plan.partitions_for("story_event"), Some(8));
        assert_eq!(plan.partitions_for("moderation_event"), Some(2));
        assert_eq!(plan.total_partitions(), 10);
    }

    #[test]
    fn rejects_an_empty_plan() {
        assert!(TopicPlan::from_toml_str("").is_err());
    }

    #[test]
    fn rejects_zero_partitions() {
        let bad = "[[topic]]\nname = \"t\"\npartitions = 0\n";
        assert!(TopicPlan::from_toml_str(bad).is_err());
    }

    #[test]
    fn rejects_a_duplicated_topic() {
        let bad = "[[topic]]\nname=\"t\"\npartitions=2\n[[topic]]\nname=\"t\"\npartitions=8\n";
        assert!(TopicPlan::from_toml_str(bad).is_err());
    }

    #[test]
    fn provisioning_creates_the_requested_counts_and_is_idempotent() {
        let broker = open("mem://provision-counts");
        let plan = TopicPlan::from_toml_str(PLAN).unwrap();

        let first = provision(&broker, &plan).unwrap();
        assert_eq!(
            first,
            vec![
                ("story_event".to_string(), 8),
                ("moderation_event".to_string(), 2)
            ]
        );

        let second = provision(&broker, &plan).unwrap();
        assert_eq!(first, second, "provisioning twice must not change anything");
    }

    #[test]
    fn a_topic_auto_created_with_one_partition_is_caught_not_reshaped() {
        // This is the accident the provisioner exists to prevent, played out:
        // something produced to the topic before provisioning ran, so the topic
        // exists with the default single partition. `create_topic` will not
        // reshape it, and a provisioner that only called `create_topic` and
        // checked its `Ok` would report success on a permanently wrong topology.
        let broker = open("mem://provision-accident");
        broker.ensure_topic("story_event").unwrap();
        assert_eq!(
            broker.topic("story_event").unwrap().partition_ids().len(),
            1
        );

        let plan = TopicPlan::from_toml_str(PLAN).unwrap();
        let err = provision(&broker, &plan).expect_err("must not report success");
        let message = err.to_string();
        assert!(message.contains("has 1 partitions"), "{message}");
        assert!(message.contains("replay"), "{message}");
    }
}
