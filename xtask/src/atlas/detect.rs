use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::facts::Facts;
use super::modules::{module_for_path, path_in_scope};

#[derive(Clone, Debug, Serialize)]
pub(super) struct PassThrough {
    pub(super) module: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) name: String,
    pub(super) callee: String,
}

#[derive(Clone, Debug)]
pub(super) struct GuardSite {
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) kind: String,
}

#[derive(Clone, Debug)]
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
                    module: module_for_path(&file.path, scope),
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
            (files >= 3).then_some(GuardFamily {
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
    families
}
