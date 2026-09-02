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
) -> (Vec<GuardFamily>, usize) {
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
    let mut dropped = 0;
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
            if !is_domain_guard(&key, &facts.defined_names) {
                dropped += 1;
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
/// vocabulary at a guard site; a guard needs another crate-defined name to
/// count as domain knowledge.
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

/// Identifiers a normalized guard key names as a call, field, or path
/// segment: `$0.parent_agent_id.is_some()` names `parent_agent_id` and
/// `is_some`; alpha-renamed bindings (`$0`) are not names.
pub(super) fn named_identifiers(key: &str) -> Vec<&str> {
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
        let named = before.ends_with('.')
            || after.starts_with('(')
            || before.ends_with("::")
            || after.starts_with("::");
        if named {
            names.push(&key[start..end]);
        }
    }
    names
}

pub(super) fn is_std_identifier(name: &str) -> bool {
    STD_IDENTIFIERS.contains(&name)
}

/// A guard is domain knowledge when it names something the crate defines;
/// a guard built only from std and external vocabulary is idiom.
fn is_domain_guard(key: &str, defined_names: &BTreeSet<String>) -> bool {
    named_identifiers(key)
        .into_iter()
        .any(|name| !is_std_identifier(name) && defined_names.contains(name))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{is_domain_guard, named_identifiers};

    #[test]
    fn domain_guards_require_a_crate_defined_named_identifier() {
        let defined = [
            "parent_agent_id",
            "is_provider_subagent",
            "ValueRefreshKind",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert!(!is_domain_guard("$0.is_empty()", &defined));
        assert!(!is_domain_guard("!$0.trim().is_empty()", &defined));
        assert!(!is_domain_guard("$0Some($1)=$2", &defined));
        assert!(!is_domain_guard("$0.kind()==ErrorKind::NotFound", &defined));
        assert!(is_domain_guard("$0.parent_agent_id.is_some()", &defined));
        assert!(is_domain_guard("$0.is_provider_subagent()", &defined));
        assert!(is_domain_guard(
            "$0.kind()!=ValueRefreshKind::Cached",
            &defined
        ));
    }

    #[test]
    fn crate_defined_std_lookalikes_do_not_make_a_guard_domain() {
        let defined = ["insert", "is_zero"]
            .map(str::to_owned)
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert!(!is_domain_guard("!$0.insert($1)", &defined));
        assert!(!is_domain_guard("$0.is_zero()", &defined));
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
    }
}
