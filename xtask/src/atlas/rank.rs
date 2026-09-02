use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use super::facts::{Facts, FileSize};
use super::history;
use super::modules::{
    crate_module_for_row, escaping_items, escaping_items_for_boundary, module_for_path,
    path_in_scope,
};

const SPLIT_ABOVE: u64 = 8_000;
const PACE_WINDOW: usize = 25;
const NOISE_LIFETIME: usize = 20;
const NOISE_WINDOW: usize = 5;
const PIN_CHURN: f64 = 3.0;
const PIN_TEST_CODE: f64 = 0.30;
const HOT_PACE: f64 = 1.5;

type Size = FileSize;

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct Row {
    pub(super) module: String,
    pub(super) code: u64,
    pub(super) tests: u64,
    pub(super) esc: usize,
    pub(super) churn: f64,
    pub(super) pace: Option<f64>,
    pub(super) cx: f64,
    pub(super) flags: Vec<&'static str>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(super) struct Totals {
    pub(super) code: u64,
    pub(super) tests: u64,
    pub(super) esc: usize,
    pub(super) cx: f64,
}

pub(super) fn rows(facts: &Facts, scope: &Path) -> Result<Vec<Row>> {
    let mut rows = split_rows(facts, scope, "")?;
    sort_rows(&mut rows);
    Ok(rows)
}

pub(super) fn totals(rows: &[Row]) -> Totals {
    rows.iter().fold(Totals::default(), |mut totals, row| {
        totals.code += row.code;
        totals.tests += row.tests;
        totals.esc += row.esc;
        totals.cx += row.cx;
        totals
    })
}

fn split_rows(facts: &Facts, scope: &Path, prefix: &str) -> Result<Vec<Row>> {
    let rows = level_rows(facts, scope, prefix, !prefix.is_empty())?;
    let mut leaves = Vec::new();
    for row in rows {
        let local = row
            .module
            .strip_prefix(prefix)
            .and_then(|module| module.strip_prefix('/'))
            .unwrap_or(&row.module);
        let child_scope = scope.join(local);
        if row.code <= SPLIT_ABOVE || local == "(root)" || !facts.root.join(&child_scope).is_dir() {
            leaves.push(row);
            continue;
        }
        let children = split_rows(facts, &child_scope, &row.module)?;
        if children.is_empty() {
            leaves.push(row);
        } else {
            leaves.extend(children);
        }
    }
    Ok(leaves)
}

fn level_rows(
    facts: &Facts,
    scope: &Path,
    prefix: &str,
    include_module_file: bool,
) -> Result<Vec<Row>> {
    let sizes = sizes(facts, scope, include_module_file);
    let files = facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
        .collect::<Vec<_>>();
    let mut escaping = escaping_items(&files, scope, &facts.mod_index);
    if include_module_file
        && let Some(file) = facts
            .syntax
            .files
            .iter()
            .find(|file| file.path == scope.with_extension("rs"))
    {
        let target = crate_module_for_row(scope, "(root)");
        escaping
            .entry("(root)".to_owned())
            .or_default()
            .extend(escaping_items_for_boundary(
                &[file],
                &target,
                &facts.mod_index,
            ));
    }
    let history = facts
        .history
        .as_ref()
        .context("rank history facts missing")?;
    let mut pace = history::pace(
        history,
        &facts.root,
        scope,
        PACE_WINDOW,
        NOISE_LIFETIME,
        NOISE_WINDOW,
    )?;
    if include_module_file {
        let module_file_pace = history::pace(
            history,
            &facts.root,
            &scope.with_extension("rs"),
            PACE_WINDOW,
            NOISE_LIFETIME,
            NOISE_WINDOW,
        )?;
        if let Some(metrics) = module_file_pace.modules.into_values().next() {
            pace.modules.insert("(root)".to_owned(), metrics);
        }
    }
    let metrics = facts
        .metrics
        .as_ref()
        .context("rank metric facts missing")?;
    let mut complexity = if scope == facts.scope {
        metrics.module_scores.clone()
    } else {
        BTreeMap::new()
    };
    if scope != facts.scope {
        for function in metrics
            .functions
            .iter()
            .filter(|function| path_in_level(&function.path, scope, include_module_file))
        {
            *complexity
                .entry(module_for_level(&function.path, scope, include_module_file))
                .or_default() += function.score;
        }
    }

    Ok(sizes
        .into_iter()
        .map(|(module, size)| {
            let history = pace.modules.get(&module).cloned().unwrap_or_default();
            let churn = history.share * 100.0;
            let ratio = (size.code > 0).then_some(size.tests as f64 / size.code as f64);
            let display = if prefix.is_empty() {
                module.clone()
            } else {
                format!("{prefix}/{module}")
            };
            let mut flags = Vec::new();
            if is_pinned(churn, ratio) {
                flags.push("pin");
            }
            if history.pace.is_some_and(|pace| pace >= HOT_PACE) {
                flags.push("hot");
            }
            if is_binary_module(facts, &display) {
                flags.push("bin");
            }
            Row {
                module: display,
                code: size.code,
                tests: size.tests,
                esc: escaping.get(&module).map_or(0, Vec::len),
                churn,
                pace: history.pace,
                cx: complexity.get(&module).copied().unwrap_or(0.0),
                flags,
            }
        })
        .collect())
}

fn is_pinned(churn: f64, test_code_ratio: Option<f64>) -> bool {
    churn >= PIN_CHURN && test_code_ratio.is_some_and(|ratio| ratio < PIN_TEST_CODE)
}

fn is_binary_module(facts: &Facts, module: &str) -> bool {
    let top = module.split('/').next().unwrap_or(module);
    let declares =
        |file: &super::syntax::FileSyntax| file.mod_decls.iter().any(|(module, _)| module == top);
    facts
        .syntax
        .files
        .iter()
        .any(|file| file.path.file_name().is_some_and(|name| name == "main.rs") && declares(file))
        && !facts.syntax.files.iter().any(|file| {
            file.path.file_name().is_some_and(|name| name == "lib.rs") && declares(file)
        })
}

fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|left, right| {
        let left_value = left.code as f64 * left.churn;
        let right_value = right.code as f64 * right.churn;
        right_value
            .total_cmp(&left_value)
            .then_with(|| right.cx.total_cmp(&left.cx))
            .then_with(|| right.code.cmp(&left.code))
            .then_with(|| left.module.cmp(&right.module))
    });
}

fn path_in_level(path: &Path, scope: &Path, include_module_file: bool) -> bool {
    path_in_scope(path, scope) || (include_module_file && path == scope.with_extension("rs"))
}

fn module_for_level(path: &Path, scope: &Path, include_module_file: bool) -> String {
    if include_module_file && path == scope.with_extension("rs") {
        "(root)".to_owned()
    } else {
        module_for_path(path, scope)
    }
}

fn sizes(facts: &Facts, scope: &Path, include_module_file: bool) -> BTreeMap<String, Size> {
    let mut sizes = BTreeMap::<String, Size>::new();
    let mut sources = facts.sources_in(scope);
    if include_module_file
        && let Some(source) = facts
            .sources
            .iter()
            .find(|source| source.path == scope.with_extension("rs"))
    {
        sources.push(source.clone());
    }
    for source in sources {
        let Some(file_size) = facts.sizes.get(&source.path) else {
            continue;
        };
        let size = sizes
            .entry(module_for_level(&source.path, scope, include_module_file))
            .or_default();
        size.code += file_size.code;
        size.tests += file_size.tests;
    }
    sizes
}

#[cfg(test)]
mod tests;
