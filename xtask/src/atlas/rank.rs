use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use super::facts::{Facts, FileSize};
use super::history;
use super::metrics::MetricsReport;
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
const THIN_TEST_CODE: f64 = 0.30;
const THIN_MIN_CODE: u64 = 200;
const CX_DECILE: usize = 10;

type Size = FileSize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RankBy {
    #[default]
    Accretion,
    Code,
    Esc,
    Churn,
    Pace,
    Cx,
    TestCode,
}

impl RankBy {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "code" => Some(Self::Code),
            "esc" => Some(Self::Esc),
            "churn" => Some(Self::Churn),
            "pace" => Some(Self::Pace),
            "cx" => Some(Self::Cx),
            "tc" => Some(Self::TestCode),
            _ => None,
        }
    }
}

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

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Hotspot {
    pub(super) function: String,
    pub(super) path: PathBuf,
    pub(super) line: u64,
    pub(super) cx: f64,
    pub(super) churn: f64,
    pub(super) hot: f64,
}

pub(super) fn rows_by(facts: &Facts, scope: &Path, by: RankBy) -> Result<Vec<Row>> {
    let mut rows = split_rows(facts, scope, "")?;
    add_outlier_flags(&mut rows);
    sort_rows(&mut rows, by);
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

pub(super) fn hotspots(
    metrics: &MetricsReport,
    file_shares: &BTreeMap<PathBuf, f64>,
) -> Vec<Hotspot> {
    let mut rows = metrics
        .functions
        .iter()
        .filter(|function| function.score > 0.0)
        .map(|function| {
            let churn = file_shares.get(&function.path).copied().unwrap_or(0.0) * 100.0;
            Hotspot {
                function: function.name.clone(),
                path: function.path.clone(),
                line: function.line,
                cx: function.score,
                churn,
                hot: function.score * churn,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .hot
            .total_cmp(&left.hot)
            .then_with(|| right.cx.total_cmp(&left.cx))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.function.cmp(&right.function))
    });
    rows
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

fn add_outlier_flags(rows: &mut [Row]) {
    let mut positive_cx = rows
        .iter()
        .map(|row| row.cx)
        .filter(|cx| *cx > 0.0)
        .collect::<Vec<_>>();
    positive_cx.sort_by(|left, right| right.total_cmp(left));
    let cx_threshold = positive_cx
        .len()
        .checked_sub(1)
        .map(|_| positive_cx.len().div_ceil(CX_DECILE))
        .and_then(|count| positive_cx.get(count.saturating_sub(1)))
        .copied();
    for row in rows {
        if cx_threshold.is_some_and(|threshold| row.cx >= threshold) {
            row.flags.push("cx");
        }
        let ratio = (row.code > 0).then_some(row.tests as f64 / row.code as f64);
        if row.code >= THIN_MIN_CODE && ratio.is_some_and(|ratio| ratio < THIN_TEST_CODE) {
            row.flags.push("thin");
        }
    }
}

fn is_binary_module(facts: &Facts, module: &str) -> bool {
    let top = module.split('/').next().unwrap_or(module);
    facts.bin_modules.contains(top)
}

fn sort_rows(rows: &mut [Row], by: RankBy) {
    rows.sort_by(|left, right| {
        let primary = match by {
            RankBy::Accretion => {
                (right.code as f64 * right.churn).total_cmp(&(left.code as f64 * left.churn))
            }
            RankBy::Code => right.code.cmp(&left.code),
            RankBy::Esc => right.esc.cmp(&left.esc),
            RankBy::Churn => right.churn.total_cmp(&left.churn),
            RankBy::Pace => right
                .pace
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&left.pace.unwrap_or(f64::NEG_INFINITY)),
            RankBy::Cx => right.cx.total_cmp(&left.cx),
            RankBy::TestCode => test_code_ratio(left).total_cmp(&test_code_ratio(right)),
        };
        primary
            .then_with(|| right.cx.total_cmp(&left.cx))
            .then_with(|| right.code.cmp(&left.code))
            .then_with(|| left.module.cmp(&right.module))
    });
}

fn test_code_ratio(row: &Row) -> f64 {
    if row.code == 0 {
        f64::INFINITY
    } else {
        row.tests as f64 / row.code as f64
    }
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
