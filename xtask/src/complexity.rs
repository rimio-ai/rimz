use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering as AtomicOrdering},
};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, Serializer};

use crate::runner;
use crate::source_files;
use crate::spinner::Spinner;

const COMPLEXITY_OUTPUT_DIR: &str = "target/complexity";
const DEFAULT_TOP_N: usize = 10;
const JSON_VERSION: u8 = 2;
const DIRECTORY_ROLLUP_LIMIT: usize = 10;
const SPLIT_FIRST_OFFENDER_COUNT: usize = 5;

pub(crate) const USAGE: &str =
    "cargo xtask complexity [--top N] [--code|--tests] [--path <prefix>] [--json]

  --top N        top N file groups per section (default 10)
  --code         only the source-code section
  --tests        only the tests section
  --path <path>  only analyze tracked Rust files under this root-relative path
  --json         versioned JSON agent contract (v2)
  -h, --help     this help";

// Thresholds follow McCabe/NIST cyclomatic risk bands, SonarSource's Cognitive
// Complexity guidance, and Clippy's too_many_lines default. Weights and tier
// multipliers make cognitive complexity the primary refactoring signal.
const WARN_CYCLOMATIC: f64 = 10.0;
const WARN_COGNITIVE: f64 = 15.0;
const WARN_SLOC: f64 = 60.0;
const HIGH_CYCLOMATIC: f64 = 15.0;
const HIGH_COGNITIVE: f64 = 25.0;
const HIGH_SLOC: f64 = 100.0;
const CRITICAL_CYCLOMATIC: f64 = 25.0;
const CRITICAL_COGNITIVE: f64 = 50.0;
const COGNITIVE_WEIGHT: f64 = 1.0;
const CYCLOMATIC_WEIGHT: f64 = 0.5;
const SLOC_WEIGHT: f64 = 0.25;
const HIGH_MULTIPLIER: f64 = 2.0;
const CRITICAL_MULTIPLIER: f64 = 4.0;

#[derive(Debug, PartialEq, Eq)]
struct ComplexityArgs {
    top_n: usize,
    filter: SectionFilter,
    path: Option<PathBuf>,
    json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SectionFilter {
    Both,
    Code,
    Tests,
}

impl SectionFilter {
    fn includes_code(self) -> bool {
        matches!(self, Self::Both | Self::Code)
    }

    fn includes_tests(self) -> bool {
        matches!(self, Self::Both | Self::Tests)
    }
}

#[derive(Debug)]
struct ComplexityReport {
    path: PathBuf,
    functions: Vec<FunctionMetrics>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct FunctionMetrics {
    name: String,
    start_line: u64,
    cyclomatic: f64,
    cognitive: f64,
    sloc: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Warn,
    High,
    Critical,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ScoredFunction {
    #[serde(flatten)]
    metrics: FunctionMetrics,
    severity: Severity,
    #[serde(serialize_with = "serialize_score")]
    score: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct FileGroup {
    path: PathBuf,
    #[serde(serialize_with = "serialize_score")]
    score: f64,
    split_first: bool,
    offenders: Vec<ScoredFunction>,
    near: Vec<FunctionMetrics>,
}

#[derive(Debug, Serialize)]
struct ComplexityJson<'a> {
    version: u8,
    thresholds: Thresholds,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<SectionJson<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tests: Option<SectionJson<'a>>,
}

#[derive(Debug, Serialize)]
struct SectionJson<'a> {
    total_files: usize,
    files: &'a [FileGroup],
}

#[derive(Debug, Serialize)]
struct Thresholds {
    warn: SeverityThresholds,
    high: SeverityThresholds,
    critical: CriticalThresholds,
}

#[derive(Debug, Serialize)]
struct SeverityThresholds {
    cyclomatic: f64,
    cognitive: f64,
    sloc: f64,
}

#[derive(Debug, Serialize)]
struct CriticalThresholds {
    cyclomatic: f64,
    cognitive: f64,
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

#[derive(Debug)]
struct DirectoryRollup {
    path: PathBuf,
    score: f64,
    files: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask complexity help is a command stdout contract"
)]
pub(crate) fn complexity(root: &Path, args: &[String]) -> Result<()> {
    let Some(args) = parse_complexity_args(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    ensure_complexity_prerequisites()?;
    let spinner = Arc::new(Spinner::new("complexity — listing tracked files"));
    let mut files = source_files::tracked_rust_files(root)?;
    if files.is_empty() {
        bail!("no tracked Rust files found");
    }
    if let Some(scope) = &args.path {
        files.retain(|file| path_is_in_scope(root, file, scope));
        if files.is_empty() {
            bail!("no tracked Rust files under `{}`", scope.display());
        }
    }

    let output_dir = root.join(COMPLEXITY_OUTPUT_DIR);
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
            .with_context(|| format!("making {} relative to {}", file.display(), root.display()))?;
        command_args.push(OsString::from("-p"));
        command_args.push(relative.as_os_str().to_owned());
    }
    spinner.set(format!("complexity — analyzing 0/{} files", files.len()));
    let watcher_stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&watcher_stop);
    let worker_spinner = Arc::clone(&spinner);
    let worker_output_dir = output_dir.clone();
    let total = files.len();
    let watcher = thread::spawn(move || {
        while !worker_stop.load(AtomicOrdering::Relaxed) {
            let mut json_files = Vec::new();
            if walk_json_files(&worker_output_dir, &mut json_files).is_ok() {
                worker_spinner.set(format!(
                    "complexity — analyzing {}/{total} files",
                    json_files.len()
                ));
            }
            thread::sleep(Duration::from_millis(300));
        }
    });
    let analysis = runner::run(root, "rust-code-analysis-cli", command_args);
    watcher_stop.store(true, AtomicOrdering::Relaxed);
    if watcher.join().is_err() {
        bail!("complexity progress watcher panicked");
    }
    analysis?;

    spinner.set("complexity — aggregating reports");
    let mut json_files = Vec::new();
    walk_json_files(&output_dir, &mut json_files)?;
    let reports = json_files
        .iter()
        .map(|json_file| parse_report(&output_dir, json_file))
        .collect::<Result<Vec<_>>>()?;

    let mut code_groups = Vec::new();
    let mut test_groups = Vec::new();
    for report in reports {
        if is_test_file(&report.path) {
            if let Some(group) = build_file_group(report.path, report.functions) {
                test_groups.push(group);
            }
            continue;
        }

        let source_path = root.join(&report.path);
        let source = fs::read_to_string(&source_path)
            .with_context(|| format!("reading {}", source_path.display()))?;
        let (code_group, test_group) = build_source_file_groups(
            report.path,
            report.functions,
            inline_test_marker_line(&source),
        );
        if let Some(group) = code_group {
            code_groups.push(group);
        }
        if let Some(group) = test_group {
            test_groups.push(group);
        }
    }

    code_groups.sort_by(compare_file_groups);
    test_groups.sort_by(compare_file_groups);
    drop(spinner);
    if args.json {
        print_json(&code_groups, &test_groups, args.top_n, args.filter)?;
    } else {
        print_report(args.top_n, &code_groups, &test_groups, args.filter);
    }
    Ok(())
}

fn parse_complexity_args(args: &[String]) -> Result<Option<ComplexityArgs>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }

    let mut top_n = None;
    let mut filter = SectionFilter::Both;
    let mut path = None;
    let mut json = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--top" => {
                if top_n.is_some() {
                    bail!("complexity --top may only be passed once");
                }
                let value = args
                    .get(index + 1)
                    .context("complexity --top requires a value")?;
                let parsed = value.parse::<usize>().with_context(|| {
                    format!("complexity --top requires a positive integer, got `{value}`")
                })?;
                if parsed == 0 {
                    bail!("complexity --top must be greater than zero");
                }
                top_n = Some(parsed);
                index += 2;
            }
            "--code" => {
                match filter {
                    SectionFilter::Both => filter = SectionFilter::Code,
                    SectionFilter::Code => {
                        bail!("complexity --code may only be passed once")
                    }
                    SectionFilter::Tests => {
                        bail!("complexity --code and --tests are mutually exclusive")
                    }
                }
                index += 1;
            }
            "--tests" => {
                match filter {
                    SectionFilter::Both => filter = SectionFilter::Tests,
                    SectionFilter::Tests => {
                        bail!("complexity --tests may only be passed once")
                    }
                    SectionFilter::Code => {
                        bail!("complexity --code and --tests are mutually exclusive")
                    }
                }
                index += 1;
            }
            "--path" => {
                if path.is_some() {
                    bail!("complexity --path may only be passed once");
                }
                let value = args
                    .get(index + 1)
                    .context("complexity --path requires a value")?;
                path = Some(PathBuf::from(value));
                index += 2;
            }
            "--json" => {
                if json {
                    bail!("complexity --json may only be passed once");
                }
                json = true;
                index += 1;
            }
            _ => bail!("unknown complexity argument `{arg}`"),
        }
    }
    Ok(Some(ComplexityArgs {
        top_n: top_n.unwrap_or(DEFAULT_TOP_N),
        filter,
        path,
        json,
    }))
}

fn path_is_in_scope(root: &Path, file: &Path, scope: &Path) -> bool {
    let scope = scope.strip_prefix(".").unwrap_or(scope);
    file.strip_prefix(root)
        .is_ok_and(|relative| relative.starts_with(scope))
}

fn ensure_complexity_prerequisites() -> Result<()> {
    let status = Command::new("rust-code-analysis-cli")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => bail!(
            "rust-code-analysis-cli is not installed\n\nInstall complexity prerequisite:\n  cargo install rust-code-analysis-cli --locked"
        ),
    }
}

fn parse_report(output_dir: &Path, json_file: &Path) -> Result<ComplexityReport> {
    let raw = fs::read_to_string(json_file)
        .with_context(|| format!("reading {}", json_file.display()))?;
    let space: Space =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", json_file.display()))?;
    let relative_json = json_file.strip_prefix(output_dir).with_context(|| {
        format!(
            "making {} relative to {}",
            json_file.display(),
            output_dir.display()
        )
    })?;
    let path = source_path_from_report_path(relative_json)?;
    let mut functions = Vec::new();
    collect_functions(&space, &mut functions);
    Ok(ComplexityReport { path, functions })
}

fn source_path_from_report_path(path: &Path) -> Result<PathBuf> {
    if path.extension().and_then(OsStr::to_str) != Some("json") {
        bail!("complexity report is not JSON: {}", path.display());
    }
    let mut source = path.to_path_buf();
    source.set_extension("");
    Ok(source)
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

fn is_test_file(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new("tests.rs"))
        || path
            .components()
            .any(|component| component.as_os_str() == OsStr::new("tests"))
}

fn collect_functions(space: &Space, functions: &mut Vec<FunctionMetrics>) {
    if matches!(space.kind.as_str(), "function" | "closure") {
        functions.push(FunctionMetrics {
            name: space.name.clone(),
            start_line: space.start_line,
            cyclomatic: space.metrics.cyclomatic.sum,
            cognitive: space.metrics.cognitive.sum,
            sloc: space.metrics.loc.sloc,
        });
        return;
    }
    for child in &space.spaces {
        collect_functions(child, functions);
    }
}

fn inline_test_marker_line(source: &str) -> Option<u64> {
    let mut marker = None;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(marker_line) = marker {
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("mod tests") {
                return Some(marker_line);
            }
            marker = None;
        }
        if trimmed == "#[cfg(test)]" {
            marker = Some(index as u64 + 1);
        }
    }
    None
}

fn classify(metrics: &FunctionMetrics) -> Option<Severity> {
    if metrics.cognitive > CRITICAL_COGNITIVE
        || (cyclomatic_counts(metrics) && metrics.cyclomatic > CRITICAL_CYCLOMATIC)
    {
        Some(Severity::Critical)
    } else if metrics.cognitive > HIGH_COGNITIVE
        || (cyclomatic_counts(metrics) && metrics.cyclomatic > HIGH_CYCLOMATIC)
        || metrics.sloc > HIGH_SLOC
    {
        Some(Severity::High)
    } else if metrics.cognitive > WARN_COGNITIVE
        || metrics.cyclomatic > WARN_CYCLOMATIC
        || metrics.sloc > WARN_SLOC
    {
        Some(Severity::Warn)
    } else {
        None
    }
}

fn offender_score(metrics: &FunctionMetrics, severity: Severity) -> f64 {
    let multiplier = match severity {
        Severity::Warn => 0.0,
        Severity::High => HIGH_MULTIPLIER,
        Severity::Critical => CRITICAL_MULTIPLIER,
    };
    let cyclomatic_overrun = if cyclomatic_counts(metrics) {
        over_threshold(metrics.cyclomatic, WARN_CYCLOMATIC)
    } else {
        0.0
    };
    multiplier
        * (COGNITIVE_WEIGHT * over_threshold(metrics.cognitive, WARN_COGNITIVE)
            + CYCLOMATIC_WEIGHT * cyclomatic_overrun
            + SLOC_WEIGHT * over_threshold(metrics.sloc, WARN_SLOC))
}

fn cyclomatic_counts(metrics: &FunctionMetrics) -> bool {
    metrics.cognitive > WARN_COGNITIVE
}

fn over_threshold(value: f64, threshold: f64) -> f64 {
    (value / threshold - 1.0).max(0.0)
}

fn build_file_group(path: PathBuf, functions: Vec<FunctionMetrics>) -> Option<FileGroup> {
    let mut offenders = Vec::new();
    let mut near = Vec::new();
    for metrics in functions {
        match classify(&metrics) {
            Some(Severity::Warn) => near.push(metrics),
            Some(severity) => offenders.push(ScoredFunction {
                score: offender_score(&metrics, severity),
                metrics,
                severity,
            }),
            None => {}
        }
    }
    if offenders.is_empty() {
        return None;
    }

    offenders.sort_by(compare_scored_functions);
    near.sort_by(|left, right| {
        left.start_line
            .cmp(&right.start_line)
            .then_with(|| left.name.cmp(&right.name))
    });
    let score = offenders.iter().map(|function| function.score).sum();
    let split_first = offenders.len() > SPLIT_FIRST_OFFENDER_COUNT;
    Some(FileGroup {
        path,
        score,
        split_first,
        offenders,
        near,
    })
}

fn build_source_file_groups(
    path: PathBuf,
    functions: Vec<FunctionMetrics>,
    inline_test_marker: Option<u64>,
) -> (Option<FileGroup>, Option<FileGroup>) {
    let (code_functions, test_functions) = functions
        .into_iter()
        .partition(|metrics| inline_test_marker.is_none_or(|line| metrics.start_line < line));
    (
        build_file_group(path.clone(), code_functions),
        build_file_group(path, test_functions),
    )
}

fn compare_scored_functions(left: &ScoredFunction, right: &ScoredFunction) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.metrics.start_line.cmp(&right.metrics.start_line))
        .then_with(|| left.metrics.name.cmp(&right.metrics.name))
}

fn compare_file_groups(left: &FileGroup, right: &FileGroup) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.path.cmp(&right.path))
}

fn directory_rollups(groups: &[FileGroup]) -> Vec<DirectoryRollup> {
    let mut rollups = BTreeMap::<PathBuf, (f64, usize)>::new();
    for group in groups {
        let directory = group
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let entry = rollups.entry(directory.to_path_buf()).or_default();
        entry.0 += group.score;
        entry.1 += 1;
    }
    let mut rollups = rollups
        .into_iter()
        .map(|(path, (score, files))| DirectoryRollup { path, score, files })
        .collect::<Vec<_>>();
    rollups.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
    });
    rollups.truncate(DIRECTORY_ROLLUP_LIMIT);
    rollups
}

fn thresholds() -> Thresholds {
    Thresholds {
        warn: SeverityThresholds {
            cyclomatic: WARN_CYCLOMATIC,
            cognitive: WARN_COGNITIVE,
            sloc: WARN_SLOC,
        },
        high: SeverityThresholds {
            cyclomatic: HIGH_CYCLOMATIC,
            cognitive: HIGH_COGNITIVE,
            sloc: HIGH_SLOC,
        },
        critical: CriticalThresholds {
            cyclomatic: CRITICAL_CYCLOMATIC,
            cognitive: CRITICAL_COGNITIVE,
        },
    }
}

fn section_json(groups: &[FileGroup], top_n: usize) -> SectionJson<'_> {
    SectionJson {
        total_files: groups.len(),
        files: &groups[..groups.len().min(top_n)],
    }
}

fn complexity_json<'a>(
    code_groups: &'a [FileGroup],
    test_groups: &'a [FileGroup],
    top_n: usize,
    filter: SectionFilter,
) -> ComplexityJson<'a> {
    ComplexityJson {
        version: JSON_VERSION,
        thresholds: thresholds(),
        code: filter
            .includes_code()
            .then(|| section_json(code_groups, top_n)),
        tests: filter
            .includes_tests()
            .then(|| section_json(test_groups, top_n)),
    }
}

fn serialize_score<S>(score: &f64, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_f64(round_score(*score))
}

fn round_score(score: f64) -> f64 {
    (score * 10.0).round() / 10.0
}

#[expect(
    clippy::print_stdout,
    reason = "xtask complexity report is a command stdout contract"
)]
fn print_json(
    code_groups: &[FileGroup],
    test_groups: &[FileGroup],
    top_n: usize,
    filter: SectionFilter,
) -> Result<()> {
    let rendered =
        serde_json::to_string_pretty(&complexity_json(code_groups, test_groups, top_n, filter))
            .context("rendering complexity JSON")?;
    println!("{rendered}");
    Ok(())
}

#[expect(
    clippy::print_stdout,
    reason = "xtask complexity report is a command stdout contract"
)]
fn print_report(
    top_n: usize,
    code_groups: &[FileGroup],
    test_groups: &[FileGroup],
    filter: SectionFilter,
) {
    if filter.includes_code() {
        print_report_section("Source", "source", top_n, code_groups);
    }
    if filter.includes_code() && filter.includes_tests() {
        println!();
    }
    if filter.includes_tests() {
        print_report_section("Test", "test", top_n, test_groups);
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask complexity report is a command stdout contract"
)]
fn print_report_section(title: &str, directory_kind: &str, top_n: usize, groups: &[FileGroup]) {
    let displayed = groups.len().min(top_n);
    println!(
        "{title} refactor targets (top {displayed} of {} files; severity: critical/high, near = warn-level context)",
        groups.len()
    );
    if groups.is_empty() {
        println!("  (none)");
    }
    for (index, group) in groups.iter().take(top_n).enumerate() {
        println!();
        let offender_label = if group.offenders.len() == 1 {
            "offender"
        } else {
            "offenders"
        };
        if group.split_first {
            println!(
                "#{}   {}    score {:.1}   split-first ({} {offender_label})",
                index + 1,
                group.path.display(),
                round_score(group.score),
                group.offenders.len()
            );
        } else {
            println!(
                "#{}   {}    score {:.1}   ({} {offender_label})",
                index + 1,
                group.path.display(),
                round_score(group.score),
                group.offenders.len()
            );
        }
        let function_width = group
            .offenders
            .iter()
            .map(|function| function_label(&function.metrics).len())
            .chain(
                group
                    .near
                    .iter()
                    .map(|function| function_label(function).len()),
            )
            .max()
            .unwrap_or(0);
        for function in &group.offenders {
            print_function(
                severity_label(function.severity),
                &function.metrics,
                function_width,
            );
        }
        for function in &group.near {
            print_function("near", function, function_width);
        }
    }

    println!();
    println!("Hot {directory_kind} directories");
    let rollups = directory_rollups(groups);
    if rollups.is_empty() {
        println!("  (none)");
    } else {
        let path_width = rollups
            .iter()
            .map(|rollup| rollup.path.display().to_string().len())
            .max()
            .unwrap_or(0);
        for rollup in rollups {
            let file_label = if rollup.files == 1 { "file" } else { "files" };
            println!(
                "     {:<path_width$}   {} {file_label}   score {:.1}",
                rollup.path.display(),
                rollup.files,
                round_score(rollup.score)
            );
        }
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask complexity report is a command stdout contract"
)]
fn print_function(severity: &str, function: &FunctionMetrics, width: usize) {
    println!(
        "     {severity:<8}  {:<width$}   cyc {:>4}   cog {:>4}   sloc {:>4}",
        function_label(function),
        metric_label(function.cyclomatic),
        metric_label(function.cognitive),
        metric_label(function.sloc)
    );
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Warn => "warn",
        Severity::High => "high",
        Severity::Critical => "critical",
    }
}

fn function_label(function: &FunctionMetrics) -> String {
    format!("{}:{}", function.name, function.start_line)
}

fn metric_label(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests;
