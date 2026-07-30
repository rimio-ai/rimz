use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{runner, source_files};

use super::modules::{module_for_path, path_in_scope};

const OUTPUT_DIR: &str = "target/atlas-complexity";

const WARN_CYCLOMATIC: f64 = 10.0;
const WARN_COGNITIVE: f64 = 15.0;
const WARN_SLOC: f64 = 60.0;
const HIGH_CYCLOMATIC: f64 = 15.0;
const HIGH_COGNITIVE: f64 = 25.0;
const HIGH_SLOC: f64 = 100.0;
const CRITICAL_CYCLOMATIC: f64 = 25.0;
const CRITICAL_COGNITIVE: f64 = 50.0;

#[derive(Clone, Debug, Serialize)]
pub(super) struct FunctionMetric {
    pub(super) module: String,
    pub(super) path: PathBuf,
    pub(super) name: String,
    pub(super) line: u64,
    pub(super) cyclomatic: f64,
    pub(super) cognitive: f64,
    pub(super) sloc: f64,
    pub(super) score: f64,
}

#[derive(Debug)]
pub(super) struct MetricsReport {
    pub(super) module_scores: BTreeMap<String, f64>,
    pub(super) functions: Vec<FunctionMetric>,
}

#[derive(Debug, Deserialize)]
struct Space {
    name: String,
    kind: String,
    start_line: u64,
    #[serde(default)]
    spaces: Vec<Space>,
    metrics: Metrics,
}

#[derive(Debug, Deserialize)]
struct Metrics {
    cyclomatic: MetricSum,
    cognitive: MetricSum,
    loc: LocMetrics,
}

#[derive(Debug, Deserialize)]
struct MetricSum {
    sum: f64,
}

#[derive(Debug, Deserialize)]
struct LocMetrics {
    sloc: f64,
}

pub(super) fn analyze(root: &Path, scope: &Path) -> Result<MetricsReport> {
    ensure_prerequisite()?;
    let files = source_files::tracked_rust_files(root)?
        .into_iter()
        .filter(|file| {
            file.strip_prefix(root)
                .is_ok_and(|path| path_in_scope(path, scope))
        })
        .collect::<Vec<_>>();
    if files.is_empty() {
        bail!("no tracked Rust files under `{}`", scope.display());
    }
    let output_dir = root.join(OUTPUT_DIR);
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir)
            .with_context(|| format!("removing {}", output_dir.display()))?;
    }
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("creating {}", output_dir.display()))?;
    let mut command_args = vec![
        OsString::from("-m"),
        OsString::from("-l"),
        OsString::from("rust"),
        OsString::from("-O"),
        OsString::from("json"),
        OsString::from("-o"),
        output_dir.as_os_str().to_owned(),
    ];
    for file in &files {
        let relative = file
            .strip_prefix(root)
            .with_context(|| format!("making {} root-relative", file.display()))?;
        command_args.push(OsString::from("-p"));
        command_args.push(relative.as_os_str().to_owned());
    }
    runner::run(root, "rust-code-analysis-cli", command_args)?;

    let mut json_files = Vec::new();
    walk_json_files(&output_dir, &mut json_files)?;
    let mut functions = Vec::new();
    for json_file in json_files {
        let raw = fs::read_to_string(&json_file)
            .with_context(|| format!("reading {}", json_file.display()))?;
        let space: Space = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", json_file.display()))?;
        let relative_json = json_file
            .strip_prefix(&output_dir)
            .with_context(|| format!("making {} report-relative", json_file.display()))?;
        let mut path = relative_json.to_path_buf();
        if path.extension().and_then(OsStr::to_str) != Some("json") {
            continue;
        }
        path.set_extension("");
        if source_files::is_test_file(&path) {
            continue;
        }
        let marker = fs::read_to_string(root.join(&path))
            .ok()
            .and_then(|source| source_files::inline_test_marker_line(&source));
        collect_functions(&space, &path, scope, marker, &mut functions);
    }
    let mut module_scores = BTreeMap::<String, f64>::new();
    for function in &functions {
        *module_scores.entry(function.module.clone()).or_default() += function.score;
    }
    functions.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    Ok(MetricsReport {
        module_scores,
        functions,
    })
}

fn ensure_prerequisite() -> Result<()> {
    let status = Command::new("rust-code-analysis-cli")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => bail!(
            "rust-code-analysis-cli is not installed\n\nInstall atlas rank prerequisite:\n  cargo install rust-code-analysis-cli --locked"
        ),
    }
}

fn walk_json_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_json_files(&path, files)?;
        } else if path.extension().and_then(OsStr::to_str) == Some("json") {
            files.push(path);
        }
    }
    Ok(())
}

fn collect_functions(
    space: &Space,
    path: &Path,
    scope: &Path,
    inline_test_marker: Option<u64>,
    output: &mut Vec<FunctionMetric>,
) {
    if matches!(space.kind.as_str(), "function" | "closure") {
        if inline_test_marker.is_some_and(|line| space.start_line >= line) {
            return;
        }
        let cyclomatic = space.metrics.cyclomatic.sum;
        let cognitive = space.metrics.cognitive.sum;
        let sloc = space.metrics.loc.sloc;
        let score = score(cyclomatic, cognitive, sloc);
        output.push(FunctionMetric {
            module: module_for_path(path, scope),
            path: path.to_path_buf(),
            name: space.name.clone(),
            line: space.start_line,
            cyclomatic,
            cognitive,
            sloc,
            score,
        });
        return;
    }
    for child in &space.spaces {
        collect_functions(child, path, scope, inline_test_marker, output);
    }
}

fn score(cyclomatic: f64, cognitive: f64, sloc: f64) -> f64 {
    let severity_multiplier = if cognitive > CRITICAL_COGNITIVE
        || (cognitive > WARN_COGNITIVE && cyclomatic > CRITICAL_CYCLOMATIC)
    {
        4.0
    } else if cognitive > HIGH_COGNITIVE
        || (cognitive > WARN_COGNITIVE && cyclomatic > HIGH_CYCLOMATIC)
        || sloc > HIGH_SLOC
    {
        2.0
    } else {
        0.0
    };
    let cyclomatic_overrun = if cognitive > WARN_COGNITIVE {
        over_threshold(cyclomatic, WARN_CYCLOMATIC)
    } else {
        0.0
    };
    severity_multiplier
        * (over_threshold(cognitive, WARN_COGNITIVE)
            + 0.5 * cyclomatic_overrun
            + 0.25 * over_threshold(sloc, WARN_SLOC))
}

fn over_threshold(value: f64, threshold: f64) -> f64 {
    (value / threshold - 1.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibrated_score_ignores_low_cognitive_cyclomatic_noise() {
        assert_eq!(score(50.0, 5.0, 20.0), 0.0);
        assert!(score(30.0, 60.0, 120.0) > 0.0);
    }
}
