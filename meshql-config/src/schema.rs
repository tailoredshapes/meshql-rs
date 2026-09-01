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

/// Walk a dotted resolver path to the type that owns its last segment.
///
/// `hens.layReports` on root `Coop` means: field `hens` has type `Hen`, and the
/// resolver attaches to `layReports` on `Hen`. Returns the owning type and the
/// final field name.
pub fn walk_path<'a>(schema: &str, root: &'a str, path: &'a str) -> Option<(String, &'a str)> {
    let mut owner = root.to_string();
    let mut parts = path.split('.').peekable();
    while let Some(seg) = parts.next() {
        if parts.peek().is_none() {
            return Some((owner, seg));
        }
        owner = field_type_name(schema, &owner, seg)?;
    }
    None
}

/// The bare type name of a field, with list and non-null markers stripped.
pub fn field_type_name(schema: &str, type_name: &str, field: &str) -> Option<String> {
    let body = type_body(schema, type_name)?;
    for line in body.lines() {
        let line = line.trim();
        let Some((name, _)) = line.split_once(':') else {
            continue;
        };
        let name = name.split('(').next().unwrap_or(name).trim();
        if name != field {
            continue;
        }
        let after_args = match line.rfind(')') {
            Some(i) => &line[i + 1..],
            None => line,
        };
        let ty = after_args.split_once(':').map(|(_, t)| t)?;
        return Some(
            ty.trim()
                .trim_start_matches('[')
                .trim_end_matches('!')
                .trim_end_matches(']')
                .trim_end_matches('!')
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod path_tests {
    use super::*;

    // Shaped like a real schema: one field per line, with blank lines and a
    // comment, because those are what broke the first version of this scanner.
    const FARM: &str = r#"
scalar Date

type Coop {
  name: String!
  farm: Farm!
  hens: [Hen]
}

# The nested hop the shared farm config resolves through.
type Hen {
  name: String!
  coop: Coop
  layReports: [LayReport]
}

type Query {
  getById(id: ID, at: Float): Coop
}
"#;

    /// The case the shared farm config uses: a resolver two hops from the root.
    #[test]
    fn a_dotted_path_resolves_to_the_type_that_owns_the_last_field() {
        assert_eq!(
            walk_path(FARM, "Coop", "hens.layReports"),
            Some(("Hen".to_string(), "layReports"))
        );
    }

    #[test]
    fn an_undotted_name_stays_on_the_root() {
        assert_eq!(
            walk_path(FARM, "Coop", "farm"),
            Some(("Coop".to_string(), "farm"))
        );
    }

    #[test]
    fn a_path_through_a_field_that_does_not_exist_is_none() {
        assert_eq!(walk_path(FARM, "Coop", "nope.layReports"), None);
    }

    #[test]
    fn list_and_non_null_markers_do_not_leak_into_the_type_name() {
        assert_eq!(
            field_type_name(FARM, "Coop", "hens").as_deref(),
            Some("Hen")
        );
        assert_eq!(
            field_type_name(FARM, "Coop", "farm").as_deref(),
            Some("Farm")
        );
    }
}
