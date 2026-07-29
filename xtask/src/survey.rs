//! Repository terrain map for refactor planning.
//!
//! Pace counts non-merge commits as changes. RimZ's effectively linear,
//! rebase-style history makes that a useful approximation; in a squash-merge
//! repository the same number would approximate pull requests. Git renames
//! below the similarity threshold appear as a delete plus an add and split the
//! file identity.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::source_files;

const DEFAULT_PATH: &str = "crates/rimz/src";
const JSON_VERSION: u8 = 1;
const COUPLING_EDGE_LIMIT: usize = 15;

pub(crate) const USAGE: &str = "cargo xtask survey [--path <prefix>] [--deps] [--json]

  --path <path>   root-relative subtree to survey (default crates/rimz/src)
  --deps          include module coupling (requires cargo-modules; holds Cargo's target lock)
  --json          versioned JSON agent contract (v1)
  -h, --help      this help";

#[derive(Debug, PartialEq, Eq)]
struct SurveyArgs {
    path: PathBuf,
    deps: bool,
    json: bool,
}

#[derive(Debug, Serialize)]
struct SurveyReport {
    version: u8,
    path: PathBuf,
    size: SizeReport,
    pace: PaceReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    coupling: Option<CouplingReport>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct SizeMetrics {
    code: u64,
    tests: u64,
    files: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ModuleSize {
    module: String,
    #[serde(flatten)]
    metrics: SizeMetrics,
}

#[derive(Debug, Serialize)]
struct SizeReport {
    total: SizeMetrics,
    modules: Vec<ModuleSize>,
}

#[derive(Debug, Serialize)]
struct PaceWindow {
    pct: u8,
    commits: usize,
    share: f64,
    pace: f64,
}

#[derive(Debug, Serialize)]
struct ModulePace {
    module: String,
    commits: usize,
    share: f64,
    windows: Vec<PaceWindow>,
    noisy: bool,
}

#[derive(Debug, Serialize)]
struct PaceReport {
    commits: usize,
    windows: [u8; 3],
    retired_commits: usize,
    modules: Vec<ModulePace>,
}

#[derive(Debug, Serialize)]
struct Degree {
    module: String,
    fan_in: usize,
    fan_out: usize,
}

#[derive(Debug, Serialize)]
struct CouplingReport {
    edges: Vec<(String, String, u64)>,
    mutual: Vec<(String, String, u64, u64)>,
    degree: Vec<Degree>,
}

#[derive(Debug)]
struct Commit {
    changes: Vec<Change>,
}

#[derive(Debug)]
enum Change {
    Touch(PathBuf),
    Delete(PathBuf),
    Rename { old: PathBuf, new: PathBuf },
}

#[derive(Debug, Default)]
struct Identity {
    path: Option<PathBuf>,
    commits: BTreeSet<usize>,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask survey output is a command stdout contract"
)]
pub(crate) fn survey(root: &Path, args: &[String]) -> Result<()> {
    let Some(args) = parse_survey_args(args)? else {
        println!("{USAGE}");
        return Ok(());
    };

    let size = build_size_report(root, &args.path)?;
    let pace = build_pace_report(root, &args.path)?;
    let coupling = args.deps.then(|| build_coupling_report(root)).transpose()?;
    let report = SurveyReport {
        version: JSON_VERSION,
        path: args.path,
        size,
        pace,
        coupling,
    };

    if args.json {
        let rendered = serde_json::to_string_pretty(&report).context("rendering survey JSON")?;
        println!("{rendered}");
    } else {
        print_report(&report);
    }
    Ok(())
}

fn parse_survey_args(args: &[String]) -> Result<Option<SurveyArgs>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }

    let mut path = None;
    let mut deps = false;
    let mut json = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--path" => {
                if path.is_some() {
                    bail!("survey --path may only be passed once");
                }
                let value = args
                    .get(index + 1)
                    .context("survey --path requires a value")?;
                path = Some(validate_scope(value)?);
                index += 2;
            }
            "--deps" => {
                if deps {
                    bail!("survey --deps may only be passed once");
                }
                deps = true;
                index += 1;
            }
            "--json" => {
                if json {
                    bail!("survey --json may only be passed once");
                }
                json = true;
                index += 1;
            }
            _ => bail!("unknown survey argument `{arg}`"),
        }
    }

    Ok(Some(SurveyArgs {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        deps,
        json,
    }))
}

fn validate_scope(value: &str) -> Result<PathBuf> {
    if value.is_empty() {
        bail!("survey --path requires a non-empty root-relative path");
    }
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        bail!("survey --path must be root-relative and may not contain `..`");
    }
    Ok(path)
}

fn build_size_report(root: &Path, scope: &Path) -> Result<SizeReport> {
    let mut files = source_files::tracked_rust_files(root)?;
    files.retain(|path| path_in_scope(root, path, scope));
    if files.is_empty() {
        bail!("no tracked Rust files under `{}`", scope.display());
    }

    let mut modules = BTreeMap::<String, SizeMetrics>::new();
    let mut total = SizeMetrics::default();
    for path in files {
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("making {} root-relative", path.display()))?;
        let source =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let sloc = rust_sloc(&source);
        let (code, tests) = if source_files::is_test_file(relative) {
            (0, sloc)
        } else {
            split_rust_sloc(&source)
        };
        let metrics = modules.entry(module_for_path(relative, scope)).or_default();
        metrics.code += code;
        metrics.tests += tests;
        metrics.files += 1;
        total.code += code;
        total.tests += tests;
        total.files += 1;
    }

    let mut modules = modules
        .into_iter()
        .map(|(module, metrics)| ModuleSize { module, metrics })
        .collect::<Vec<_>>();
    modules.sort_by(|left, right| {
        right
            .metrics
            .code
            .cmp(&left.metrics.code)
            .then_with(|| right.metrics.tests.cmp(&left.metrics.tests))
            .then_with(|| left.module.cmp(&right.module))
    });
    Ok(SizeReport { total, modules })
}

fn split_rust_sloc(source: &str) -> (u64, u64) {
    let total = rust_sloc(source);
    let Some(marker) = source_files::inline_test_marker_line(source) else {
        return (total, 0);
    };
    let code_end = if marker == 1 {
        0
    } else {
        source
            .match_indices('\n')
            .nth(marker as usize - 2)
            .map_or(source.len(), |(index, _)| index + 1)
    };
    let code = rust_sloc(&source[..code_end]);
    (code, total.saturating_sub(code))
}

fn rust_sloc(source: &str) -> u64 {
    let bytes = source.as_bytes();
    let mut code_lines = 0;
    let mut line_has_code = false;
    let mut block_comment_depth = 0_u32;
    let mut string_escape = false;
    let mut string = false;
    let mut raw_string_hashes = None;
    let mut character_end = None;
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            code_lines += u64::from(line_has_code);
            line_has_code = string || raw_string_hashes.is_some();
            string_escape = false;
            index += 1;
            continue;
        }
        if block_comment_depth > 0 {
            if bytes[index..].starts_with(b"/*") {
                block_comment_depth += 1;
                index += 2;
            } else if bytes[index..].starts_with(b"*/") {
                block_comment_depth -= 1;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(end) = character_end {
            if index == end {
                character_end = None;
            }
            index += 1;
            continue;
        }
        if let Some(hashes) = raw_string_hashes {
            if byte == b'"'
                && bytes
                    .get(index + 1..index + 1 + hashes)
                    .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
            {
                raw_string_hashes = None;
                index += hashes + 1;
            } else {
                index += 1;
            }
            continue;
        }
        if string {
            if string_escape {
                string_escape = false;
            } else if byte == b'\\' {
                string_escape = true;
            } else if byte == b'"' {
                string = false;
            }
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            block_comment_depth = 1;
            index += 2;
            continue;
        }
        if let Some((prefix_len, hashes)) = source_files::raw_string_start(&bytes[index..]) {
            line_has_code = true;
            raw_string_hashes = Some(hashes);
            index += prefix_len;
            continue;
        }
        if byte == b'\'' {
            line_has_code = true;
            character_end = source_files::character_literal_end(bytes, index);
            index += 1;
            continue;
        }
        if byte == b'"' {
            line_has_code = true;
            string = true;
            index += 1;
            continue;
        }
        if !byte.is_ascii_whitespace() {
            line_has_code = true;
        }
        index += 1;
    }

    code_lines + u64::from(line_has_code)
}

fn path_in_scope(root: &Path, path: &Path, scope: &Path) -> bool {
    path.strip_prefix(root)
        .is_ok_and(|relative| relative.starts_with(scope_for_matching(scope)))
}

fn scope_for_matching(scope: &Path) -> &Path {
    match scope.strip_prefix(Path::new(".")) {
        Ok(scope) if scope.as_os_str().is_empty() => Path::new(""),
        Ok(scope) => scope,
        Err(_) => scope,
    }
}

fn module_for_path(path: &Path, scope: &Path) -> String {
    let relative = path.strip_prefix(scope_for_matching(scope)).unwrap_or(path);
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return file_module(path);
    };
    if components.next().is_some() {
        first.as_os_str().to_string_lossy().into_owned()
    } else {
        file_module(relative)
    }
}

fn file_module(path: &Path) -> String {
    let Some(name) = path.file_name() else {
        return "(root)".to_owned();
    };
    let name = name.to_string_lossy();
    match name.as_ref() {
        "lib.rs" | "main.rs" => "(root)".to_owned(),
        _ => name.strip_suffix(".rs").unwrap_or(&name).to_owned(),
    }
}

fn build_pace_report(root: &Path, scope: &Path) -> Result<PaceReport> {
    let history = git_history(root, scope)?;
    let commits = parse_history(&history)?;
    if commits.is_empty() {
        bail!("no non-merge commits touch `{}`", scope.display());
    }
    Ok(fold_history(root, scope, &commits))
}

fn git_history(root: &Path, scope: &Path) -> Result<String> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.quotePath=false",
            "log",
            "--reverse",
            "--topo-order",
            "--no-merges",
            "--format=@%H",
            "--name-status",
            "-M",
            "--",
        ])
        .arg(scope)
        .current_dir(root)
        .output()
        .context("running `git log` for survey")?;
    if !output.status.success() {
        bail!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("git log returned non-UTF-8 paths")
}

fn parse_history(input: &str) -> Result<Vec<Commit>> {
    let mut commits = Vec::<Commit>::new();
    for line in input.lines().filter(|line| !line.is_empty()) {
        if line.starts_with('@') && !line.contains('\t') {
            commits.push(Commit {
                changes: Vec::new(),
            });
            continue;
        }
        let commit = commits
            .last_mut()
            .context("git history contained a change before its commit header")?;
        let fields = line.split('\t').collect::<Vec<_>>();
        let status = fields.first().copied().unwrap_or_default();
        let change = match status.as_bytes().first() {
            Some(b'R') => {
                if fields.len() != 3 {
                    bail!("malformed git rename record `{line}`");
                }
                Change::Rename {
                    old: PathBuf::from(fields[1]),
                    new: PathBuf::from(fields[2]),
                }
            }
            Some(b'D') => {
                let path = fields
                    .get(1)
                    .with_context(|| format!("malformed git delete record `{line}`"))?;
                Change::Delete(PathBuf::from(path))
            }
            Some(_) => {
                let path = fields
                    .last()
                    .filter(|_| fields.len() >= 2)
                    .with_context(|| format!("malformed git change record `{line}`"))?;
                Change::Touch(PathBuf::from(path))
            }
            None => bail!("malformed git status record `{line}`"),
        };
        commit.changes.push(change);
    }
    Ok(commits)
}

fn fold_history(root: &Path, scope: &Path, commits: &[Commit]) -> PaceReport {
    let mut paths = HashMap::<PathBuf, usize>::new();
    let mut identities = Vec::<Identity>::new();
    for (commit_index, commit) in commits.iter().enumerate() {
        for change in &commit.changes {
            match change {
                Change::Touch(path) => {
                    let identity = identity_for_path(path, &mut paths, &mut identities);
                    identities[identity].commits.insert(commit_index);
                }
                Change::Delete(path) => {
                    let identity = identity_for_path(path, &mut paths, &mut identities);
                    identities[identity].commits.insert(commit_index);
                    paths.remove(path);
                    identities[identity].path = None;
                }
                Change::Rename { old, new } => {
                    let identity = paths
                        .remove(old)
                        .unwrap_or_else(|| new_identity(old, &mut identities));
                    if let Some(replaced) = paths.remove(new) {
                        identities[replaced].path = None;
                    }
                    paths.insert(new.clone(), identity);
                    identities[identity].path = Some(new.clone());
                    identities[identity].commits.insert(commit_index);
                }
            }
        }
    }

    let mut module_commits = BTreeMap::<String, BTreeSet<usize>>::new();
    let mut retired_commits = BTreeSet::new();
    for identity in identities {
        let Some(path) = identity.path.filter(|path| {
            path.starts_with(scope_for_matching(scope)) && root.join(path).is_file()
        }) else {
            retired_commits.extend(identity.commits);
            continue;
        };
        module_commits
            .entry(module_for_path(&path, scope))
            .or_default()
            .extend(identity.commits);
    }

    let total = commits.len();
    let windows = [
        (25_u8, window_size(total, 25)),
        (10, window_size(total, 10)),
    ];
    let mut modules = module_commits
        .into_iter()
        .map(|(module, module_commits)| {
            let lifetime_commits = module_commits.len();
            let lifetime_share = lifetime_commits as f64 / total as f64;
            let windows = windows
                .iter()
                .map(|(pct, size)| {
                    let first = total - size;
                    let commits = module_commits.range(first..).count();
                    let share = commits as f64 / *size as f64;
                    PaceWindow {
                        pct: *pct,
                        commits,
                        share: round(share, 3),
                        pace: round(share / lifetime_share, 2),
                    }
                })
                .collect::<Vec<_>>();
            let noisy = windows
                .iter()
                .any(|window| pace_is_noisy(lifetime_commits, window.commits));
            ModulePace {
                module,
                commits: lifetime_commits,
                share: round(lifetime_share, 3),
                windows,
                noisy,
            }
        })
        .collect::<Vec<_>>();
    modules.sort_by(|left, right| {
        right
            .commits
            .cmp(&left.commits)
            .then_with(|| left.module.cmp(&right.module))
    });

    PaceReport {
        commits: total,
        windows: [100, 25, 10],
        retired_commits: retired_commits.len(),
        modules,
    }
}

fn identity_for_path(
    path: &Path,
    paths: &mut HashMap<PathBuf, usize>,
    identities: &mut Vec<Identity>,
) -> usize {
    if let Some(identity) = paths.get(path) {
        return *identity;
    }
    let identity = new_identity(path, identities);
    paths.insert(path.to_path_buf(), identity);
    identity
}

fn new_identity(path: &Path, identities: &mut Vec<Identity>) -> usize {
    let identity = identities.len();
    identities.push(Identity {
        path: Some(path.to_path_buf()),
        commits: BTreeSet::new(),
    });
    identity
}

fn window_size(commits: usize, pct: usize) -> usize {
    commits.saturating_mul(pct).div_ceil(100)
}

fn pace_is_noisy(lifetime_commits: usize, window_commits: usize) -> bool {
    lifetime_commits < 20 || window_commits < 5
}

fn round(value: f64, decimal_places: i32) -> f64 {
    let factor = 10_f64.powi(decimal_places);
    (value * factor).round() / factor
}

fn build_coupling_report(root: &Path) -> Result<CouplingReport> {
    ensure_coupling_prerequisite()?;
    let output = Command::new("cargo")
        .args([
            "modules",
            "dependencies",
            "-p",
            "rimz",
            "--lib",
            "--no-externs",
            "--no-sysroot",
            "--no-owns",
        ])
        .current_dir(root)
        .output()
        .context("running `cargo modules dependencies`")?;
    if !output.status.success() {
        bail!(
            "cargo modules dependencies failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let dot =
        String::from_utf8(output.stdout).context("cargo modules returned non-UTF-8 output")?;
    Ok(aggregate_coupling(&dot))
}

fn ensure_coupling_prerequisite() -> Result<()> {
    let status = Command::new("cargo-modules")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => bail!(
            "cargo-modules is not installed\n\nInstall survey prerequisite:\n  cargo install cargo-modules --locked"
        ),
    }
}

fn aggregate_coupling(dot: &str) -> CouplingReport {
    let mut counts = BTreeMap::<(String, String), u64>::new();
    for line in dot.lines() {
        let Some((left, right)) = parse_dot_edge(line) else {
            continue;
        };
        let (Some(left), Some(right)) = (module_endpoint(left), module_endpoint(right)) else {
            continue;
        };
        if left != right {
            *counts.entry((left, right)).or_default() += 1;
        }
    }

    let mut edges = counts
        .iter()
        .map(|((left, right), count)| (left.clone(), right.clone(), *count))
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.1.cmp(&right.1))
    });

    let mut mutual = Vec::new();
    for ((left, right), count) in &counts {
        if left < right
            && let Some(reverse) = counts.get(&(right.clone(), left.clone()))
        {
            mutual.push((left.clone(), right.clone(), *count, *reverse));
        }
    }
    mutual.sort_by(|left, right| {
        (right.2 + right.3)
            .cmp(&(left.2 + left.3))
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.1.cmp(&right.1))
    });

    let mut degrees = BTreeMap::<String, (usize, usize)>::new();
    for (left, right) in counts.keys() {
        degrees.entry(left.clone()).or_default().1 += 1;
        degrees.entry(right.clone()).or_default().0 += 1;
    }
    let degree = degrees
        .into_iter()
        .map(|(module, (fan_in, fan_out))| Degree {
            module,
            fan_in,
            fan_out,
        })
        .collect();

    CouplingReport {
        edges,
        mutual,
        degree,
    }
}

fn parse_dot_edge(line: &str) -> Option<(&str, &str)> {
    let (left, right) = line.trim().split_once(" -> ")?;
    let left = left.strip_prefix('"')?.strip_suffix('"')?;
    let right = right.strip_prefix('"')?;
    let quote = right.find('"')?;
    Some((left, &right[..quote]))
}

fn module_endpoint(path: &str) -> Option<String> {
    if path == "rimz" {
        return Some("(root)".to_owned());
    }
    path.strip_prefix("rimz::")
        .and_then(|path| path.split("::").next())
        .filter(|module| !module.is_empty())
        .map(str::to_owned)
}

#[expect(
    clippy::print_stdout,
    reason = "xtask survey report is a command stdout contract"
)]
fn print_report(report: &SurveyReport) {
    println!("Repository survey — {}", report.path.display());
    println!();
    print_size(&report.size);
    println!();
    print_pace(&report.pace);
    if let Some(coupling) = &report.coupling {
        println!();
        print_coupling(coupling);
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask survey report is a command stdout contract"
)]
fn print_size(size: &SizeReport) {
    println!("Size");
    println!(
        "  {:<24} {:>10} {:>10} {:>7} {:>7}",
        "module", "code", "tests", "t/c", "files"
    );
    for module in &size.modules {
        println!(
            "  {:<24} {:>10} {:>10} {:>7} {:>7}",
            module.module,
            module.metrics.code,
            module.metrics.tests,
            ratio_label(module.metrics.code, module.metrics.tests),
            module.metrics.files
        );
    }
    println!(
        "  {:<24} {:>10} {:>10} {:>7} {:>7}",
        "total",
        size.total.code,
        size.total.tests,
        ratio_label(size.total.code, size.total.tests),
        size.total.files
    );
}

fn ratio_label(code: u64, tests: u64) -> String {
    if code == 0 {
        "—".to_owned()
    } else {
        format!("{:.2}", tests as f64 / code as f64)
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask survey report is a command stdout contract"
)]
fn print_pace(pace: &PaceReport) {
    let window_25 = window_size(pace.commits, 25);
    let window_10 = window_size(pace.commits, 10);
    println!(
        "Pace ({} commits; 25% = {window_25}, 10% = {window_10})",
        pace.commits
    );
    println!(
        "  {:<24} {:>8} {:>8} {:>9} {:>8} {:>9} {:>8}",
        "module", "commits", "share", "25% share", "pace", "10% share", "pace"
    );
    for module in &pace.modules {
        let window_25 = &module.windows[0];
        let window_10 = &module.windows[1];
        println!(
            "  {:<24} {:>8} {:>7.1}% {:>8.1}% {:>8} {:>8.1}% {:>8}",
            module.module,
            module.commits,
            module.share * 100.0,
            window_25.share * 100.0,
            pace_label(module.commits, window_25),
            window_10.share * 100.0,
            pace_label(module.commits, window_10)
        );
    }
    println!(
        "  retired: {} commits touch paths absent at HEAD",
        pace.retired_commits
    );
    println!("  * noisy: fewer than 5 window commits or 20 lifetime commits");
}

fn pace_label(lifetime_commits: usize, window: &PaceWindow) -> String {
    let suffix = if pace_is_noisy(lifetime_commits, window.commits) {
        "*"
    } else {
        ""
    };
    format!("{:.2}x{suffix}", window.pace)
}

#[expect(
    clippy::print_stdout,
    reason = "xtask survey report is a command stdout contract"
)]
fn print_coupling(coupling: &CouplingReport) {
    let total = coupling.edges.iter().map(|edge| edge.2).sum::<u64>();
    println!(
        "Coupling ({} distinct cross-module edges, {total} total)",
        coupling.edges.len()
    );
    println!("  Top weighted edges");
    for (left, right, count) in coupling.edges.iter().take(COUPLING_EDGE_LIMIT) {
        println!("    {left} -> {right}: {count}");
    }
    println!("  Mutual pairs");
    if coupling.mutual.is_empty() {
        println!("    (none)");
    } else {
        for (left, right, forward, reverse) in &coupling.mutual {
            println!("    {left} <-> {right}: {forward}/{reverse}");
        }
    }
    println!("  Degree");
    for degree in &coupling.degree {
        println!(
            "    {:<24} in {:>3}  out {:>3}",
            degree.module, degree.fan_in, degree.fan_out
        );
    }
}

#[cfg(test)]
mod tests;
