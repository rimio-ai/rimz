use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::facts::Facts;
use super::modules::{module_for_path, module_is_within, path_in_scope};

#[derive(Clone, Debug, Serialize)]
pub(super) struct PassThrough {
    pub(super) module: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) name: String,
    pub(super) callee: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct VestigialItem {
    pub(super) module: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) name: String,
    pub(super) age_days: i64,
    pub(super) production_ref_modules: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct GuardSite {
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) kind: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct RepeatedGuard {
    pub(super) predicate: String,
    pub(super) files: usize,
    pub(super) sites: Vec<GuardSite>,
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

pub(super) fn vestigial(facts: &Facts, scope: &Path, window_pct: usize) -> Vec<VestigialItem> {
    let (Some(references), Some(blame), Some(history)) =
        (&facts.references, &facts.blame, &facts.history)
    else {
        return Vec::new();
    };
    let window_start = history.window_start(window_pct);
    let now = history.last_time();
    let mut rows = Vec::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
    {
        let Some(lines) = blame.lines.get(&file.path) else {
            continue;
        };
        for item in &file.pub_items {
            let reach = facts.mod_index.effective_reach(file, item);
            if module_is_within(&reach, &item.module) {
                continue;
            }
            let Some(item_refs) = references.get(file, item) else {
                continue;
            };
            let production_ref_modules = item_refs
                .production
                .iter()
                .filter(|module| !module_is_within(module, &item.module))
                .cloned()
                .collect::<Vec<_>>();
            if production_ref_modules.len() > 1 {
                continue;
            }
            let span = item.line.saturating_sub(1)..item.end_line.min(lines.len());
            let blamed = lines.get(span).unwrap_or_default();
            let commits = blamed
                .iter()
                .map(|(commit, _)| commit)
                .collect::<BTreeSet<_>>();
            let Some((_, time)) = blamed.first() else {
                continue;
            };
            if commits.len() != 1 || *time >= window_start {
                continue;
            }
            rows.push(VestigialItem {
                module: module_for_path(&file.path, scope),
                path: file.path.clone(),
                line: item.line,
                name: item.name.clone(),
                age_days: now.saturating_sub(*time) / 86_400,
                production_ref_modules,
            });
        }
    }
    rows.sort_by(|left, right| {
        right
            .age_days
            .cmp(&left.age_days)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    rows
}

pub(super) fn guards(facts: &Facts, scope: &Path, min_files: usize) -> Vec<RepeatedGuard> {
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
    let mut rows = groups
        .into_iter()
        .filter_map(|(predicate, mut sites)| {
            sites.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| left.line.cmp(&right.line))
            });
            let files = sites
                .iter()
                .map(|site| &site.path)
                .collect::<BTreeSet<_>>()
                .len();
            (files >= min_files).then_some(RepeatedGuard {
                predicate,
                files,
                sites,
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .files
            .cmp(&left.files)
            .then_with(|| right.sites.len().cmp(&left.sites.len()))
            .then_with(|| left.predicate.cmp(&right.predicate))
    });
    rows
}

pub(super) fn counts_by_module<T>(
    rows: &[T],
    module: impl Fn(&T) -> &str,
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for row in rows {
        *counts.entry(module(row).to_owned()).or_default() += 1;
    }
    counts
}
