use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::facts::Facts;
use super::modules::{crate_module_for_path, path_in_scope};

#[derive(Clone, Debug, Serialize)]
pub(super) struct PassThrough {
    pub(super) module: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) name: String,
    pub(super) callee: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct GuardSite {
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct GuardFamily {
    pub(super) key: String,
    pub(super) files: usize,
    pub(super) sites: usize,
    pub(super) locations: Vec<GuardSite>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(super) struct GuardDrops {
    pub(super) vocabulary: usize,
    pub(super) predicate_use: usize,
}

pub(super) fn passthroughs(facts: &Facts, scope: &Path) -> Vec<PassThrough> {
    let mut rows = facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
        .flat_map(|file| {
            file.fns.iter().filter_map(|function| {
                function.forwards.as_ref().map(|callee| PassThrough {
                    module: crate_module_for_path(&file.path),
                    path: file.path.clone(),
                    line: function.line,
                    name: function.name.clone(),
                    callee: callee.clone(),
                })
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.module
            .cmp(&right.module)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    rows
}

pub(super) fn guard_families(facts: &Facts, scope: &Path) -> Vec<GuardFamily> {
    guard_families_with_dropped(facts, scope).0
}

pub(super) fn guard_families_with_dropped(
    facts: &Facts,
    scope: &Path,
) -> (Vec<GuardFamily>, GuardDrops) {
    guard_family_report(facts, scope, false)
}

pub(super) fn guard_families_all_with_dropped(
    facts: &Facts,
    scope: &Path,
) -> (Vec<GuardFamily>, GuardDrops) {
    guard_family_report(facts, scope, true)
}

fn guard_family_report(
    facts: &Facts,
    scope: &Path,
    include_all: bool,
) -> (Vec<GuardFamily>, GuardDrops) {
    let mut groups = BTreeMap::<String, Vec<GuardSite>>::new();
    for guard in facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
        .flat_map(|file| &file.guards)
    {
        groups
            .entry(guard.normalized.clone())
            .or_default()
            .push(GuardSite {
                path: guard.path.clone(),
                line: guard.line,
                kind: guard.kind.clone(),
            });
    }
    let mut dropped = GuardDrops::default();
    let mut families = groups
        .into_iter()
        .filter_map(|(key, mut locations)| {
            locations.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| left.line.cmp(&right.line))
            });
            let files = locations
                .iter()
                .map(|site| &site.path)
                .collect::<BTreeSet<_>>()
                .len();
            if files < 3 {
                return None;
            }
            let classification = classify_guard(&key, &facts.defined_names, &facts.unique_fields);
            match classification {
                GuardClassification::Vocabulary => dropped.vocabulary += 1,
                GuardClassification::PredicateUse => dropped.predicate_use += 1,
                GuardClassification::Composed => {}
            }
            if !include_all && classification != GuardClassification::Composed {
                return None;
            }
            Some(GuardFamily {
                key,
                files,
                sites: locations.len(),
                locations,
            })
        })
        .collect::<Vec<_>>();
    families.sort_by(|left, right| {
        right
            .files
            .cmp(&left.files)
            .then_with(|| right.sites.cmp(&left.sites))
            .then_with(|| left.key.cmp(&right.key))
    });
    (families, dropped)
}

/// Method and type names the crate also defines but that read as std
/// vocabulary at a guard site; they do not contribute to crate composition.
const STD_IDENTIFIERS: &[&str] = &[
    "is_empty",
    "len",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "contains",
    "contains_key",
    "starts_with",
    "ends_with",
    "trim",
    "kind",
    "exists",
    "is_dir",
    "is_file",
    "is_absolute",
    "is_relative",
    "parent",
    "strip_prefix",
    "strip_suffix",
    "get",
    "first",
    "last",
    "as_ref",
    "as_str",
    "as_deref",
    "unwrap_or",
    "unwrap_or_default",
    "now",
    "elapsed",
    "as_secs",
    "as_millis",
    "file_name",
    "extension",
    "to_string_lossy",
    "chars",
    "bytes",
    "lines",
    "split",
    "iter",
    "next",
    "peek",
    "any",
    "all",
    "matches",
    "insert",
    "remove",
    "push",
    "pop",
    "filter",
    "map",
    "success",
    "status",
    "var",
    "var_os",
    "env",
    "io",
    "fs",
    "stdin",
    "stdout",
    "stderr",
    "Some",
    "None",
    "Ok",
    "Err",
    "Instant",
    "SystemTime",
    "Duration",
    "Path",
    "PathBuf",
    "String",
    "Vec",
    "Option",
    "Result",
    "max",
    "min",
    "abs",
    "saturating_sub",
    "saturating_add",
    "checked_sub",
    "is_multiple_of",
    "is_zero",
    "is_ascii",
    "is_alphanumeric",
    "is_whitespace",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IdentifierRole {
    Method,
    Field,
    Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NamedIdentifier<'a> {
    pub(super) name: &'a str,
    pub(super) role: IdentifierRole,
}

/// Identifiers a normalized guard key names and the role each occupies.
/// Alpha-renamed bindings (`$0`) are not names.
pub(super) fn named_identifier_roles(key: &str) -> Vec<NamedIdentifier<'_>> {
    let mut names = Vec::new();
    let mut identifiers = key.char_indices().peekable();
    while let Some((start, first)) = identifiers.next() {
        if first != '_' && !first.is_alphabetic() {
            continue;
        }
        let mut end = start + first.len_utf8();
        while let Some(&(index, next)) = identifiers.peek() {
            if next != '_' && !next.is_alphanumeric() {
                break;
            }
            identifiers.next();
            end = index + next.len_utf8();
        }
        let before = &key[..start];
        let after = &key[end..];
        let role = if before.ends_with("::") || after.starts_with("::") {
            Some(IdentifierRole::Path)
        } else if after.starts_with('(') {
            Some(IdentifierRole::Method)
        } else if before.ends_with('.') {
            Some(IdentifierRole::Field)
        } else {
            None
        };
        if let Some(role) = role {
            names.push(NamedIdentifier {
                name: &key[start..end],
                role,
            });
        }
    }
    names
}

pub(super) fn is_std_identifier(name: &str) -> bool {
    STD_IDENTIFIERS.contains(&name)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuardClassification {
    Vocabulary,
    PredicateUse,
    Composed,
}

/// A guard duplicates knowledge only when it composes crate vocabulary.
/// Naming one crate-defined method is merely using its predicate, and a
/// field reads one type's state only when exactly one struct declares it.
fn classify_guard(
    key: &str,
    defined_names: &BTreeSet<String>,
    unique_fields: &BTreeSet<String>,
) -> GuardClassification {
    let identifiers = named_identifier_roles(key)
        .into_iter()
        .filter(|identifier| {
            !is_std_identifier(identifier.name)
                && match identifier.role {
                    IdentifierRole::Field => unique_fields.contains(identifier.name),
                    IdentifierRole::Method | IdentifierRole::Path => {
                        defined_names.contains(identifier.name)
                    }
                }
        })
        .collect::<Vec<_>>();
    let distinct = identifiers
        .iter()
        .map(|identifier| identifier.name)
        .collect::<BTreeSet<_>>();
    if distinct.is_empty() {
        GuardClassification::Vocabulary
    } else if distinct.len() >= 2
        || identifiers.iter().any(|identifier| {
            matches!(
                identifier.role,
                IdentifierRole::Field | IdentifierRole::Path
            )
        })
    {
        GuardClassification::Composed
    } else {
        GuardClassification::PredicateUse
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{GuardClassification, IdentifierRole, classify_guard, named_identifier_roles};

    fn named_identifiers(key: &str) -> Vec<&str> {
        named_identifier_roles(key)
            .into_iter()
            .map(|identifier| identifier.name)
            .collect()
    }

    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn is_domain_guard(key: &str, defined: &[&str], unique_fields: &[&str]) -> bool {
        classify_guard(key, &names(defined), &names(unique_fields)) == GuardClassification::Composed
    }

    #[test]
    fn domain_guards_compose_crate_defined_identifiers() {
        let defined = [
            "parent_agent_id",
            "is_provider_subagent",
            "needs_attention",
            "ValueRefreshKind",
        ];
        let fields = ["parent_agent_id"];
        assert!(!is_domain_guard("$0.is_empty()", &defined, &fields));
        assert!(!is_domain_guard("!$0.trim().is_empty()", &defined, &fields));
        assert!(!is_domain_guard("$0Some($1)=$2", &defined, &fields));
        assert!(!is_domain_guard(
            "$0.kind()==ErrorKind::NotFound",
            &defined,
            &fields
        ));
        assert!(is_domain_guard(
            "$0.parent_agent_id.is_some()",
            &defined,
            &fields
        ));
        assert!(!is_domain_guard(
            "$0.is_provider_subagent()",
            &defined,
            &fields
        ));
        assert!(is_domain_guard(
            "$0.is_provider_subagent()&&$1.needs_attention()",
            &defined,
            &fields
        ));
        assert!(is_domain_guard(
            "$0.kind()!=ValueRefreshKind::Cached",
            &defined,
            &fields
        ));
    }

    #[test]
    fn a_field_composes_only_when_one_struct_declares_it() {
        // `rows` is a field on many structs and a function name elsewhere;
        // `parent_agent_id` is declared once.
        let defined = ["rows", "parent_agent_id"];
        assert!(!is_domain_guard(
            "$0.rows.is_empty()",
            &defined,
            &["parent_agent_id"]
        ));
        assert!(is_domain_guard(
            "$0.rows.is_empty()",
            &defined,
            &["rows", "parent_agent_id"]
        ));
        assert_eq!(
            classify_guard("$0.rows.is_empty()", &names(&defined), &names(&[])),
            GuardClassification::Vocabulary
        );
    }

    #[test]
    fn single_crate_predicate_use_is_not_duplicated_knowledge() {
        assert_eq!(
            classify_guard(
                "$0.is_provider_subagent()",
                &names(&["is_provider_subagent"]),
                &names(&[])
            ),
            GuardClassification::PredicateUse
        );
    }

    #[test]
    fn crate_defined_std_lookalikes_do_not_make_a_guard_domain() {
        let defined = ["insert", "is_zero"];
        assert!(!is_domain_guard("!$0.insert($1)", &defined, &[]));
        assert!(!is_domain_guard("$0.is_zero()", &defined, &[]));
    }

    #[test]
    fn named_identifiers_skip_alpha_renamed_bindings() {
        assert_eq!(
            named_identifiers("$0Err($1)=child_process::spawn_detached_rimz($2,$3,_)"),
            ["Err", "child_process", "spawn_detached_rimz"]
        );
        assert_eq!(
            named_identifiers("$0.status.is_terminal()"),
            ["status", "is_terminal"]
        );
        assert_eq!(
            named_identifier_roles("$0.parent_agent_id.is_some()"),
            [
                super::NamedIdentifier {
                    name: "parent_agent_id",
                    role: IdentifierRole::Field,
                },
                super::NamedIdentifier {
                    name: "is_some",
                    role: IdentifierRole::Method,
                },
            ]
        );
        assert_eq!(
            named_identifier_roles("$0.kind()!=ValueRefreshKind::Cached"),
            [
                super::NamedIdentifier {
                    name: "kind",
                    role: IdentifierRole::Method,
                },
                super::NamedIdentifier {
                    name: "ValueRefreshKind",
                    role: IdentifierRole::Path,
                },
                super::NamedIdentifier {
                    name: "Cached",
                    role: IdentifierRole::Path,
                },
            ]
        );
    }
}
