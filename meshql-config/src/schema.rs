//! Reading enough GraphQL to decide what a resolver is.
//!
//! The shared configs carry one flat `resolvers` list. TypeScript can afford
//! that: its graphlette calls the remote query and hands the result straight to
//! GraphQL, which knows from the schema whether the field is a list. Rust's
//! `RootConfig` splits the two — `singleton_resolvers` and `vector_resolvers` —
//! so the loader has to recover the distinction the config never wrote down.
//!
//! The schema already holds the answer: `farm: Farm!` is a singleton,
//! `hens: [Hen]` is a vector. This finds it without pulling in a parser, because
//! the question is narrow enough to answer by scanning.

/// Is `field` on `type_name` a list?
///
/// Returns `None` when the type or the field is absent, so a caller can tell
/// "not a list" from "not there" and complain about the second.
pub fn field_is_list(schema: &str, type_name: &str, field: &str) -> Option<bool> {
    let body = type_body(schema, type_name)?;
    for line in body.lines() {
        let line = line.trim();
        let Some((name, _)) = line.split_once(':') else {
            continue;
        };
        // A field can carry arguments: `getById(id: ID, at: Float): Farm`, so
        // the name stops at the paren and the type starts after it.
        let name = name.split('(').next().unwrap_or(name).trim();
        if name != field {
            continue;
        }
        let after_args = match line.rfind(')') {
            Some(i) => &line[i + 1..],
            None => line,
        };
        let ty = after_args.split_once(':').map(|(_, t)| t).unwrap_or("");
        return Some(ty.trim().starts_with('['));
    }
    None
}

/// The type a graphlette's queries return, taken from the `Query` type.
///
/// Every root query on one graphlette returns the same entity, so the first
/// field settles it.
pub fn root_type(schema: &str) -> Option<String> {
    let body = type_body(schema, "Query")?;
    for line in body.lines() {
        // `getByName(name: String, at: Float): Coop` — the colons inside the
        // argument list are not the one that names the return type.
        let after_args = match line.rfind(')') {
            Some(i) => &line[i + 1..],
            None => line,
        };
        let Some((_, ty)) = after_args.split_once(':') else {
            continue;
        };
        let ty = ty
            .trim()
            .trim_start_matches('[')
            .trim_end_matches('!')
            .trim_end_matches(']')
            .trim_end_matches('!');
        if !ty.is_empty() {
            return Some(ty.to_string());
        }
    }
    None
}

fn type_body<'a>(schema: &'a str, type_name: &str) -> Option<&'a str> {
    let needle = format!("type {type_name} ");
    let start = schema
        .find(&needle)
        .or_else(|| schema.find(&format!("type {type_name}{{")))?;
    let open = schema[start..].find('{')? + start;
    let close = schema[open..].find('}')? + open;
    Some(&schema[open + 1..close])
}

#[cfg(test)]
mod tests {
    use super::*;

    const COOP: &str = r#"
scalar Date
type Coop {
  name: String!
  farm: Farm!
  id: ID
  hens: [Hen]
}
type Query {
  getByName(name: String, at: Float): Coop
  getById(id: ID, at: Float): Coop
}
"#;

    #[test]
    fn a_list_field_is_a_vector_and_a_bare_one_is_not() {
        assert_eq!(field_is_list(COOP, "Coop", "hens"), Some(true));
        assert_eq!(field_is_list(COOP, "Coop", "farm"), Some(false));
        assert_eq!(field_is_list(COOP, "Coop", "name"), Some(false));
    }

    /// A field that is not there is not the same as one that is not a list. The
    /// loader complains about the first and quietly handles the second.
    #[test]
    fn an_absent_field_is_distinguishable_from_a_scalar_one() {
        assert_eq!(field_is_list(COOP, "Coop", "nope"), None);
        assert_eq!(field_is_list(COOP, "Nope", "hens"), None);
    }

    #[test]
    fn the_root_type_comes_from_the_query_type() {
        assert_eq!(root_type(COOP).as_deref(), Some("Coop"));
    }

    /// Query fields carry arguments, and the argument list must not be mistaken
    /// for part of the field name.
    #[test]
    fn arguments_do_not_confuse_the_field_name() {
        assert_eq!(field_is_list(COOP, "Query", "getById"), Some(false));
    }
}
