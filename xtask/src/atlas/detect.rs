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
            if !is_domain_guard(&key) {
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

fn is_domain_guard(key: &str) -> bool {
    const STD_IDENTIFIERS: &[&str] = &[
        "is_empty",
        "len",
        "is_some",
        "is_none",
        "is_ok",
        "is_err",
        "contains",
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
        "is_ascii",
        "is_alphanumeric",
        "is_whitespace",
    ];

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
        if named && !STD_IDENTIFIERS.contains(&&key[start..end]) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_domain_guard;

    #[test]
    fn domain_guards_require_a_non_std_named_identifier() {
        assert!(!is_domain_guard("$0.is_empty()"));
        assert!(!is_domain_guard("!$0.trim().is_empty()"));
        assert!(!is_domain_guard("$0Some($1)=$2"));
        assert!(is_domain_guard("$0.parent_agent_id.is_some()"));
        assert!(is_domain_guard("$0.is_provider_subagent()"));
        assert!(is_domain_guard("$0.kind()==ErrorKind::NotFound"));
    }
}
