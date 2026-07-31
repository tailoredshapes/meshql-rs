//! The index set, derived from the deployment's own query templates.
//!
//! # Why derivation, and not a list
//!
//! meshql never does arbitrary-attribute search. Every query template is a fixed
//! string handed to [`RootConfig::builder`]'s `singleton`/`vector` at build time,
//! in source-controlled configuration; nothing about a *request* chooses a field
//! to filter on. So the complete set of fields a deployment will ever filter on
//! is knowable before the first request, by walking the same `RootConfig` that
//! generates the queries.
//!
//! That is a strictly stronger property than a hand-maintained index list. A list
//! can drift away from the queries; a derivation **cannot**, because both come
//! from the same object. A deployment therefore declares nothing extra:
//!
//! ```no_run
//! # async fn example() -> meshql_core::Result<()> {
//! # let config = meshql_core::RootConfig::builder()
//! #     .vector("getCoopsByFarm", r#"{"payload.farmId": "{{id}}"}"#)
//! #     .build();
//! use meshql_dynamo::DynamoCollection;
//!
//! // The same `config` the graphlette gets. The `farmId` index follows from it.
//! let coops = DynamoCollection::open(None, "coops", &config).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # What a field becomes
//!
//! DynamoDB cannot index a nested attribute, so an indexed payload field is
//! *promoted* on write to a top-level scalar — `payload.farmId` becomes the
//! attribute `ix_farmId` — and gets a global secondary index
//! `meshql_ix_farmId`, hash key `ix_farmId`, range key `sk`.
//!
//! Range key `sk` is not decoration: it carries the temporal cutoff into the
//! index, so `sk < :hi` stays a *key condition* rather than a filter, and a
//! query with an `at:` reads only the versions it is allowed to see.
//!
//! The projection is [`KEYS_ONLY`](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/GSI.html).
//! That follows from the two-phase resolution in [`crate::searcher`]: the index
//! is used for *candidate ids only* and every candidate is re-read from the base
//! table anyway, so a wider projection costs more to write and to store and is
//! never read. See `docs/cost-model-dynamodb.md` §6.
//!
//! # Only string-valued fields
//!
//! A promoted attribute is written only when the payload's value at that path is
//! a JSON **string**, and the index is only used when the rendered template's
//! value is a JSON string. This is not a limitation dodged, it is the soundness
//! condition: [`crate::matcher`] compares JSON values, so `"42"` never equals
//! `42`, so a record whose payload holds a *number* at an indexed path can never
//! match a *string* predicate — and its absence from the index is therefore
//! correct rather than a miss. A template that filters an indexed field on a
//! non-string value is refused by [`IndexPlan::derive`] at startup, because on
//! DynamoDB it cannot be served without a scan.
//!
//! # The guards
//!
//! Three, and each exists because the alternative is a deployment that believes
//! it is indexed when it is not:
//!
//! 1. **A template naming a payload field with no index is an error, not a
//!    scan.** Silent fallback to an O(V) scan is how a table gets to a million
//!    versions before anyone notices. Unreachable from [`IndexPlan::derive`] by
//!    construction — it fires when a plan is hand-built, or when a searcher is
//!    wired to a *different* config from the one its plan came from, which is a
//!    copy-paste away in any multi-entity `lib.rs`.
//! 2. **More than [`MAX_GLOBAL_SECONDARY_INDEXES`] derived indexes is a startup
//!    error**, naming the fields, rather than a `CreateTable` rejection at first
//!    boot in production.
//! 3. **A key that is neither `id` nor `payload.…` is a startup error.** Such a
//!    template matches nothing on every meshql backend (see [`crate::matcher`]),
//!    so it is a configuration bug, and startup is the cheapest place to learn
//!    that. At *runtime* the same shape is not an error — it returns empty
//!    without touching DynamoDB, which is exactly what a scan would have
//!    returned, for nothing.
//!
//! Derivation is convenience; the runtime check in [`crate::searcher`] is what
//! makes it sound. If a template renders to keys derivation did not predict — a
//! Handlebars helper generating keys, say — the runtime check still refuses to
//! scan.

use std::collections::{BTreeSet, HashMap};

use aws_sdk_dynamodb::types::AttributeValue;
use meshql_core::{MeshqlError, QueryConfig, Result, RootConfig, Stash};
use serde_json::Value;

/// DynamoDB's default limit on global secondary indexes per table.
///
/// It is a soft limit — AWS will raise it on request — but a deployment that
/// needs it raised should find out at startup and not from `CreateTable` in
/// production.
pub const MAX_GLOBAL_SECONDARY_INDEXES: usize = 20;

/// Prefix of the promoted top-level attribute: `payload.farmId` → `ix_farmId`.
pub const ATTRIBUTE_PREFIX: &str = "ix_";

/// Prefix of the index name: `payload.farmId` → `meshql_ix_farmId`.
///
/// Namespaced so that [`ensure_indexed_table`](crate::store::ensure_indexed_table)
/// can tell an index *this crate* manages from one a client added by hand, and
/// leave the latter alone.
pub const INDEX_PREFIX: &str = "meshql_ix_";

/// The attribute a payload field is promoted to.
pub fn attribute_name(field: &str) -> String {
    format!("{ATTRIBUTE_PREFIX}{field}")
}

/// The global secondary index covering a payload field.
pub fn index_name(field: &str) -> String {
    format!("{INDEX_PREFIX}{field}")
}

/// The payload field an index name covers, or `None` if this crate does not
/// manage that index.
pub fn field_of_index(index: &str) -> Option<&str> {
    index.strip_prefix(INDEX_PREFIX)
}

/// What a rendered template key means to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key<'a> {
    /// `"id"` — the base table's hash key. Answerable by a point `Query`; needs
    /// no index.
    Id,
    /// `"payload.<path>"` — a dot path into the payload, nesting allowed.
    Payload(&'a str),
    /// Anything else. [`crate::matcher`] resolves no path for it, so it matches
    /// nothing, on every backend.
    Unmatchable,
}

/// Classify one key of a rendered query template.
pub fn classify(key: &str) -> Key<'_> {
    if key == "id" {
        Key::Id
    } else if let Some(field) = key.strip_prefix("payload.") {
        if field.is_empty() {
            Key::Unmatchable
        } else {
            Key::Payload(field)
        }
    } else {
        Key::Unmatchable
    }
}

/// The set of payload fields a deployment filters on, and therefore the set of
/// global secondary indexes its table needs.
///
/// Ordered and de-duplicated: two entities sharing a table, or two templates
/// naming the same foreign key, produce one index, not two.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexPlan {
    fields: BTreeSet<String>,
}

impl IndexPlan {
    /// The plan a `RootConfig` implies.
    ///
    /// Fails, naming the query and the template, when a template cannot be
    /// served from an index — see the module docs' three guards.
    pub fn derive(config: &RootConfig) -> Result<Self> {
        Self::derive_all([config])
    }

    /// The plan several `RootConfig`s imply, for the deployments that put more
    /// than one graphlette over one table.
    pub fn derive_all<'a, I>(configs: I) -> Result<Self>
    where
        I: IntoIterator<Item = &'a RootConfig>,
    {
        let mut fields = BTreeSet::new();
        for config in configs {
            for query in &config.queries {
                for field in template_fields(query)? {
                    fields.insert(field);
                }
            }
        }
        Self::from_fields(fields)
    }

    /// A plan over an explicit field list, for a caller who is not driving from
    /// a `RootConfig`.
    ///
    /// Prefer [`Self::derive`]. A hand-written list is the thing that can drift
    /// away from the queries, which is the whole failure this module exists to
    /// remove; if you build one, pass it through [`Self::verify_covers`].
    pub fn from_fields<I, S>(fields: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let fields: BTreeSet<String> = fields.into_iter().map(Into::into).collect();
        for field in &fields {
            check_indexable_name(field)?;
        }
        if fields.len() > MAX_GLOBAL_SECONDARY_INDEXES {
            return Err(MeshqlError::Validation(format!(
                "this configuration needs {} global secondary indexes and DynamoDB allows \
                 {MAX_GLOBAL_SECONDARY_INDEXES} per table. The fields are: {}. Split the \
                 entity across tables, drop a query, or ask AWS to raise the limit.",
                fields.len(),
                fields.iter().cloned().collect::<Vec<_>>().join(", "),
            )));
        }
        Ok(Self { fields })
    }

    /// Every payload field in the plan, in a stable order.
    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().map(String::as_str)
    }

    /// How many indexes the plan needs.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Whether `field` (a payload dot path, without the `payload.` prefix) has
    /// an index.
    pub fn covers(&self, field: &str) -> bool {
        self.fields.contains(field)
    }

    /// Every index name the plan implies.
    pub fn index_names(&self) -> impl Iterator<Item = String> + '_ {
        self.fields.iter().map(|f| index_name(f))
    }

    /// Startup check: every field `config` filters on has an index here.
    ///
    /// Trivially true for a plan from [`Self::derive`] over the same config —
    /// which is the point. It earns its keep when a plan derived from *one*
    /// entity's config is handed to a searcher serving another's, a mistake that
    /// is one copy-pasted line away in any multi-entity `lib.rs` and that would
    /// otherwise surface as a full table scan.
    pub fn verify_covers(&self, config: &RootConfig) -> Result<()> {
        for query in &config.queries {
            for field in template_fields(query)? {
                if !self.covers(&field) {
                    return Err(MeshqlError::Validation(format!(
                        "query {:?} filters on payload field {:?}, which has no index in this \
                         plan (indexed: {}). Template: {}. Deriving the plan from the same \
                         RootConfig the graphlette uses makes this impossible.",
                        query.name,
                        field,
                        self.describe(),
                        query.template,
                    )));
                }
            }
        }
        Ok(())
    }

    /// `"none"`, or the indexed fields comma-separated — for error messages.
    pub fn describe(&self) -> String {
        if self.fields.is_empty() {
            "none".to_string()
        } else {
            self.fields.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    }

    /// Add the promoted attributes for `payload` to an item on its way to
    /// `PutItem`.
    ///
    /// Only string values are promoted; see the module docs for why that is the
    /// soundness condition and not a shortcut. A payload that lacks the field
    /// promotes nothing, which makes the index **sparse** — and a sparse miss
    /// costs no write capacity at all, so an optional field is free to index
    /// (measured: `tests/index_cost.rs`).
    pub fn promote(&self, payload: &Stash, item: &mut HashMap<String, AttributeValue>) {
        for field in &self.fields {
            if let Some(Value::String(s)) = payload_at(payload, field) {
                item.insert(attribute_name(field), AttributeValue::S(s.clone()));
            }
        }
    }
}

/// Resolve a dot path into a payload.
pub fn payload_at<'a>(payload: &'a Stash, field: &str) -> Option<&'a Value> {
    let mut segments = field.split('.');
    let mut current = payload.get(segments.next()?)?;
    for segment in segments {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

/// The payload fields one configured query filters on.
fn template_fields(query: &QueryConfig) -> Result<Vec<String>> {
    let context = |msg: String| {
        MeshqlError::Validation(format!(
            "query {:?}: {msg}\n  template: {}",
            query.name, query.template
        ))
    };

    let rendered = probe_render(&query.template).map_err(&context)?;
    let object = rendered
        .as_object()
        .ok_or_else(|| context("a query template must render to a JSON object".to_string()))?;

    let mut fields = Vec::new();
    for (key, value) in object {
        match classify(key) {
            Key::Id => {}
            Key::Payload(field) => {
                if !value.is_string() {
                    return Err(context(format!(
                        "filters {key:?} on a non-string value. DynamoDB index keys are \
                         scalars written from the payload, and this adapter promotes only \
                         string values — a number here could only be served by a full scan. \
                         Quote the placeholder."
                    )));
                }
                check_indexable_name(field).map_err(|e| context(e.to_string()))?;
                fields.push(field.to_string());
            }
            Key::Unmatchable => {
                return Err(context(format!(
                    "key {key:?} is neither \"id\" nor \"payload.<field>\", so it resolves to \
                     no path and the query matches nothing — on every meshql backend, not \
                     just this one. A payload field needs its \"payload.\" prefix."
                )));
            }
        }
    }
    Ok(fields)
}

/// Render a template with a stand-in value for every variable, so that the
/// *shape* of a query — its keys, and whether each value is a string — can be
/// read off configuration with no request in hand.
///
/// The stand-in is `"0"`, which is deliberate: substituted into `"{{n}}"` it
/// yields the string `"0"` and into a bare `{{n}}` the number `0`, so the two
/// are distinguishable, and both parse. Rendering with an empty context instead
/// turns a bare `{{n}}` into invalid JSON and loses the distinction in a parse
/// error.
fn probe_render(template: &str) -> std::result::Result<Value, String> {
    let mut handlebars = handlebars::Handlebars::new();
    handlebars.set_strict_mode(false);

    let mut probe = Stash::new();
    for name in template_variables(template) {
        probe.insert(name, Value::String("0".to_string()));
    }

    let rendered = handlebars
        .render_template(template, &Value::Object(probe))
        .map_err(|e| format!("does not render: {e}"))?;
    serde_json::from_str(&rendered)
        .map_err(|e| format!("does not render to JSON ({rendered:?}): {e}"))
}

/// The plain `{{variable}}` names in a template.
///
/// Block helpers, partials and paths are skipped rather than guessed at: a
/// template built from those may render to keys this cannot predict, and the
/// runtime check in [`crate::searcher`] — not this function — is what keeps that
/// case from becoming a silent scan.
fn template_variables(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let token = after[..end].trim().trim_matches(|c| c == '{' || c == '}');
        let token = token.trim();
        if !token.is_empty()
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            names.push(token.to_string());
        }
        rest = &after[end + 2..];
    }
    names
}

/// DynamoDB index names are `[A-Za-z0-9_.-]{3,255}`, and the name is
/// [`INDEX_PREFIX`] plus the field.
fn check_indexable_name(field: &str) -> Result<()> {
    let name = index_name(field);
    let bad = |why: &str| {
        MeshqlError::Validation(format!(
            "payload field {field:?} cannot be indexed: {why}. It would need the DynamoDB \
             index name {name:?}, and index names are 3-255 characters of \
             [A-Za-z0-9_.-]."
        ))
    };
    if field.is_empty() {
        return Err(bad("it is empty"));
    }
    if name.len() > 255 {
        return Err(bad("the name is too long"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '.' || *c == '-'))
    {
        return Err(bad(&format!("it contains {c:?}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(queries: &[(&str, &str)]) -> RootConfig {
        let mut builder = RootConfig::builder();
        for (name, template) in queries {
            builder = builder.vector(*name, *template);
        }
        builder.build()
    }

    fn fields(plan: &IndexPlan) -> Vec<&str> {
        plan.fields().collect()
    }

    /// The property the whole module rests on: the index set falls out of the
    /// configuration, with nothing declared twice.
    #[test]
    fn the_plan_is_derived_from_the_templates() {
        let plan = IndexPlan::derive(&config(&[
            ("getCoop", r#"{"id": "{{id}}"}"#),
            ("getCoops", r#"{"payload.name": "{{name}}"}"#),
            ("getCoopsByFarm", r#"{"payload.farmId": "{{id}}"}"#),
            ("getAll", r#"{}"#),
        ]))
        .unwrap();
        assert_eq!(fields(&plan), vec!["farmId", "name"]);
    }

    /// `{"id": …}` is answered by the base table's hash key, and `{}` is the
    /// irreducible scan. Neither needs — or gets — an index.
    #[test]
    fn id_and_get_all_need_no_index() {
        let plan = IndexPlan::derive(&config(&[
            ("byId", r#"{"id": "{{id}}"}"#),
            ("all", r#"{}"#),
        ]))
        .unwrap();
        assert!(plan.is_empty(), "got {:?}", fields(&plan));
    }

    #[test]
    fn two_templates_on_one_field_are_one_index() {
        let plan = IndexPlan::derive(&config(&[
            ("byFarm", r#"{"payload.farm_id": "{{id}}"}"#),
            ("alsoByFarm", r#"{"payload.farm_id": "{{farmId}}"}"#),
        ]))
        .unwrap();
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn several_configs_over_one_table_merge() {
        let a = config(&[("byFarm", r#"{"payload.farm_id": "{{id}}"}"#)]);
        let b = config(&[("byZone", r#"{"payload.zone": "{{zone}}"}"#)]);
        let plan = IndexPlan::derive_all([&a, &b]).unwrap();
        assert_eq!(fields(&plan), vec!["farm_id", "zone"]);
    }

    #[test]
    fn a_multi_condition_template_indexes_every_field_it_names() {
        let plan = IndexPlan::derive(&config(&[(
            "byTypeAndName",
            r#"{"payload.type": "{{type}}", "payload.name": "{{name}}"}"#,
        )]))
        .unwrap();
        assert_eq!(fields(&plan), vec!["name", "type"]);
    }

    #[test]
    fn nested_payload_paths_are_indexable() {
        let plan =
            IndexPlan::derive(&config(&[("deep", r#"{"payload.a.b.c": "{{v}}"}"#)])).unwrap();
        assert_eq!(fields(&plan), vec!["a.b.c"]);
        assert_eq!(index_name("a.b.c"), "meshql_ix_a.b.c");
    }

    // ---- the guards ----

    #[test]
    fn more_than_twenty_indexes_is_a_startup_error_naming_the_fields() {
        let queries: Vec<(String, String)> = (0..21)
            .map(|i| {
                (
                    format!("q{i}"),
                    format!(r#"{{"payload.f{i}": "{{{{v}}}}"}}"#),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = queries
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        let err = IndexPlan::derive(&config(&borrowed))
            .unwrap_err()
            .to_string();
        assert!(err.contains("21"), "{err}");
        assert!(err.contains("20"), "{err}");
        assert!(
            err.contains("f13"),
            "the message must name the fields: {err}"
        );

        // ...and exactly twenty is fine, so the boundary is the limit and not an
        // off-by-one.
        let borrowed: Vec<(&str, &str)> = borrowed[..20].to_vec();
        assert_eq!(IndexPlan::derive(&config(&borrowed)).unwrap().len(), 20);
    }

    #[test]
    fn a_bare_payload_key_is_a_startup_error() {
        // `{"kind": …}` matches nothing on every backend — see `matcher`. It is
        // a configuration bug, and startup is the cheapest place to learn it.
        let err = IndexPlan::derive(&config(&[("byKind", r#"{"kind": "{{kind}}"}"#)]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("byKind"), "name the query: {err}");
        assert!(err.contains("payload."), "say what is wrong: {err}");
    }

    #[test]
    fn a_non_string_filter_value_is_a_startup_error() {
        let err = IndexPlan::derive(&config(&[("byCount", r#"{"payload.count": {{count}}}"#)]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("byCount"), "{err}");
        assert!(err.contains("payload.count"), "name the field: {err}");

        // The same field quoted is fine, which is what makes the message's
        // "quote the placeholder" advice actionable.
        assert!(
            IndexPlan::derive(&config(&[("byCount", r#"{"payload.count": "{{count}}"}"#)])).is_ok()
        );
    }

    #[test]
    fn a_template_that_is_not_a_json_object_is_a_startup_error() {
        let err = IndexPlan::derive(&config(&[("bad", r#"["not", "an", "object"]"#)]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("JSON object"), "{err}");
    }

    /// The drift the derivation exists to prevent, caught when someone builds a
    /// plan by hand or wires a searcher to the wrong config.
    #[test]
    fn verify_covers_names_the_field_and_the_template() {
        let coops = config(&[("getCoopsByFarm", r#"{"payload.farmId": "{{id}}"}"#)]);
        let hens = config(&[("getHensByCoop", r#"{"payload.coopId": "{{id}}"}"#)]);

        let plan = IndexPlan::derive(&coops).unwrap();
        assert!(plan.verify_covers(&coops).is_ok());

        let err = plan.verify_covers(&hens).unwrap_err().to_string();
        assert!(err.contains("coopId"), "name the field: {err}");
        assert!(err.contains("getHensByCoop"), "name the query: {err}");
        assert!(
            err.contains(r#"{"payload.coopId": "{{id}}"}"#),
            "name the template: {err}"
        );
    }

    #[test]
    fn a_field_whose_index_name_would_be_illegal_is_refused() {
        let err = IndexPlan::from_fields(["a b"]).unwrap_err().to_string();
        assert!(err.contains("' '"), "{err}");
        assert!(IndexPlan::from_fields(["a-b.c_d"]).is_ok());
    }

    // ---- promotion ----

    #[test]
    fn promotion_writes_string_values_and_skips_everything_else() {
        let plan = IndexPlan::from_fields(["name", "count", "missing", "a.b"]).unwrap();
        let payload: Stash = json!({
            "name": "alpha",
            "count": 3,
            "a": {"b": "deep"},
        })
        .as_object()
        .unwrap()
        .clone();

        let mut item = HashMap::new();
        plan.promote(&payload, &mut item);

        assert_eq!(
            item.get("ix_name"),
            Some(&AttributeValue::S("alpha".into()))
        );
        assert_eq!(item.get("ix_a.b"), Some(&AttributeValue::S("deep".into())));
        assert!(
            !item.contains_key("ix_count"),
            "a number cannot equal a string predicate, so indexing it would be noise"
        );
        assert!(
            !item.contains_key("ix_missing"),
            "an absent field promotes nothing — which is what makes the index sparse, and \
             sparse misses are free"
        );
    }

    #[test]
    fn classify_knows_the_two_shapes_and_rejects_the_rest() {
        assert_eq!(classify("id"), Key::Id);
        assert_eq!(classify("payload.name"), Key::Payload("name"));
        assert_eq!(classify("payload.a.b"), Key::Payload("a.b"));
        assert_eq!(classify("kind"), Key::Unmatchable);
        assert_eq!(classify("payload."), Key::Unmatchable);
        assert_eq!(classify("createdAt"), Key::Unmatchable);
    }

    #[test]
    fn index_names_round_trip_and_are_namespaced() {
        assert_eq!(index_name("farm_id"), "meshql_ix_farm_id");
        assert_eq!(attribute_name("farm_id"), "ix_farm_id");
        assert_eq!(field_of_index("meshql_ix_farm_id"), Some("farm_id"));
        assert_eq!(
            field_of_index("someone-elses-index"),
            None,
            "an index this crate does not manage must be recognisably not ours"
        );
    }

    #[test]
    fn template_variables_finds_the_plain_ones() {
        assert_eq!(template_variables(r#"{"id": "{{id}}"}"#), vec!["id"]);
        assert_eq!(
            template_variables(r#"{"payload.a": "{{a}}", "payload.b": "{{ b }}"}"#),
            vec!["a", "b"]
        );
        assert_eq!(template_variables(r#"{}"#), Vec::<String>::new());
        // A block helper is skipped, not guessed at.
        assert_eq!(
            template_variables(r#"{{#if x}}{"id": "{{id}}"}{{/if}}"#),
            vec!["id"]
        );
    }

    #[test]
    fn payload_at_walks_dot_paths() {
        let payload: Stash = json!({"a": {"b": {"c": "leaf"}}, "flat": "v"})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(payload_at(&payload, "flat"), Some(&json!("v")));
        assert_eq!(payload_at(&payload, "a.b.c"), Some(&json!("leaf")));
        assert_eq!(payload_at(&payload, "a.b.missing"), None);
        assert_eq!(payload_at(&payload, "flat.deeper"), None);
    }
}
