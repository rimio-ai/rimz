use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::runner;
use crate::source_files;

const COMPLEXITY_OUTPUT_DIR: &str = "target/complexity";
const DEFAULT_TOP_N: usize = 20;

#[derive(Clone, Debug, PartialEq)]
struct FileComplexity {
    path: PathBuf,
    sloc: f64,
    cyclomatic: f64,
    cognitive: f64,
    worst: Option<WorstSpace>,
}

#[derive(Clone, Debug, PartialEq)]
struct WorstSpace {
    name: String,
    start_line: u64,
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

pub(crate) fn complexity(root: &Path, args: &[String]) -> Result<()> {
    let top_n = parse_top_n(args)?;
    ensure_complexity_prerequisites()?;
    let files = source_files::tracked_rust_files(root)?;
    if files.is_empty() {
        bail!("no tracked Rust files found");
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
    runner::run(root, "rust-code-analysis-cli", command_args)?;

    let mut reports = Vec::new();
    let mut json_files = Vec::new();
    walk_json_files(&output_dir, &mut json_files)?;
    for json_file in json_files {
        reports.push(parse_report(&output_dir, &json_file)?);
    }

    let mut source_files = Vec::new();
    let mut test_files = Vec::new();
    for report in reports {
        if is_test_file(&report.path) {
            test_files.push(report);
        } else {
            source_files.push(report);
        }
    }

    let source_files = top_complexity(source_files, top_n);
    let test_files = top_complexity(test_files, top_n);
    print_report(top_n, &source_files, &test_files);
    Ok(())
}

fn parse_top_n(args: &[String]) -> Result<usize> {
    match args {
        [] => Ok(DEFAULT_TOP_N),
        [value] => {
            let top_n = value
                .parse::<usize>()
                .with_context(|| format!("parsing complexity top-N `{value}`"))?;
            if top_n == 0 {
                bail!("complexity top-N must be greater than zero");
            }
            Ok(top_n)
        }
        _ => bail!("complexity takes at most one top-N argument"),
    }
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

fn parse_report(output_dir: &Path, json_file: &Path) -> Result<FileComplexity> {
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
    Ok(FileComplexity {
        path,
        sloc: space.metrics.loc.sloc,
        cyclomatic: space.metrics.cyclomatic.sum,
        cognitive: space.metrics.cognitive.sum,
        worst: worst_space(&space),
    })
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

fn worst_space(space: &Space) -> Option<WorstSpace> {
    let mut worst = if matches!(space.kind.as_str(), "function" | "closure") {
        Some(WorstSpace {
            name: space.name.clone(),
            start_line: space.start_line,
            cyclomatic: space.metrics.cyclomatic.sum,
            cognitive: space.metrics.cognitive.sum,
        })
    } else {
        None
    };

    for child in &space.spaces {
        worst = better_worst(worst, worst_space(child));
    }
    worst
}

fn better_worst(left: Option<WorstSpace>, right: Option<WorstSpace>) -> Option<WorstSpace> {
    match (left, right) {
        (None, None) => None,
        (Some(worst), None) | (None, Some(worst)) => Some(worst),
        (Some(left), Some(right)) => {
            if compare_worst(&right, &left).is_lt() {
                Some(right)
            } else {
                Some(left)
            }
        }
    }
}

fn compare_worst(left: &WorstSpace, right: &WorstSpace) -> Ordering {
    right
        .cyclomatic
        .total_cmp(&left.cyclomatic)
        .then_with(|| right.cognitive.total_cmp(&left.cognitive))
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.start_line.cmp(&right.start_line))
}

fn top_complexity(mut files: Vec<FileComplexity>, top_n: usize) -> Vec<FileComplexity> {
    files.sort_by(compare_file_complexity);
    files.truncate(top_n);
    files
}

fn compare_file_complexity(left: &FileComplexity, right: &FileComplexity) -> Ordering {
    right
        .cyclomatic
        .total_cmp(&left.cyclomatic)
        .then_with(|| right.cognitive.total_cmp(&left.cognitive))
        .then_with(|| right.sloc.total_cmp(&left.sloc))
        .then_with(|| left.path.cmp(&right.path))
}

#[expect(
    clippy::print_stdout,
    reason = "xtask complexity report is a command stdout contract"
)]
fn print_report(top_n: usize, source_files: &[FileComplexity], test_files: &[FileComplexity]) {
    print_table("Source files", top_n, source_files);
    println!();
    print_table("Test files", top_n, test_files);
}

#[expect(
    clippy::print_stdout,
    reason = "xtask complexity report is a command stdout contract"
)]
fn print_table(title: &str, top_n: usize, files: &[FileComplexity]) {
    println!("{title} by complexity (top {top_n})");
    if files.is_empty() {
        println!("  (none)");
        return;
    }

    let file_width = files
        .iter()
        .map(|file| file.path.display().to_string().len())
        .max()
        .unwrap_or("File".len())
        .max("File".len());
    println!(
        "{:<4}  {:<file_width$}  {:>6}  {:>10}  {:>9}  Worst fn",
        "Rank", "File", "SLOC", "Cyclomatic", "Cognitive"
    );
    for (index, file) in files.iter().enumerate() {
        println!(
            "{:<4}  {:<file_width$}  {:>6}  {:>10}  {:>9}  {}",
            index + 1,
            file.path.display(),
            metric_label(file.sloc),
            metric_label(file.cyclomatic),
            metric_label(file.cognitive),
            worst_space_label(file.worst.as_ref())
        );
    }
}

fn metric_label(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn worst_space_label(worst: Option<&WorstSpace>) -> String {
    match worst {
        Some(worst) => format!(
            "{}:{} (cyc {}, cog {})",
            worst.name,
            worst.start_line,
            metric_label(worst.cyclomatic),
            metric_label(worst.cognitive)
        ),
        None => "-".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric_sum(sum: u64) -> MetricSum {
        MetricSum { sum: sum as f64 }
    }

    fn metrics(cyclomatic: u64, cognitive: u64, sloc: u64) -> Metrics {
        Metrics {
            cyclomatic: metric_sum(cyclomatic),
            cognitive: metric_sum(cognitive),
            loc: LocMetrics { sloc: sloc as f64 },
        }
    }

    fn space(
        name: &str,
        kind: &str,
        start_line: u64,
        cyclomatic: u64,
        cognitive: u64,
        children: Vec<Space>,
    ) -> Space {
        Space {
            name: name.to_owned(),
            kind: kind.to_owned(),
            start_line,
            spaces: children,
            metrics: metrics(cyclomatic, cognitive, 1),
        }
    }

    #[test]
    fn is_test_file_classifies_conventional_paths() {
        assert!(is_test_file(Path::new("xtask/src/pricing/tests.rs")));
        assert!(is_test_file(Path::new(
            "crates/rimz/tests/integration/backend/zellij.rs"
        )));
        assert!(!is_test_file(Path::new("crates/rimz/src/worktree.rs")));
    }

    #[test]
    fn worst_space_selects_function_or_closure_descendant() {
        let root = space(
            "crate",
            "unit",
            1,
            40,
            30,
            vec![
                space("module", "mod", 2, 30, 25, vec![]),
                space("small", "function", 4, 4, 3, vec![]),
                space(
                    "outer",
                    "function",
                    10,
                    8,
                    4,
                    vec![space("inner", "closure", 12, 12, 9, vec![])],
                ),
            ],
        );

        assert_eq!(
            worst_space(&root),
            Some(WorstSpace {
                name: "inner".to_owned(),
                start_line: 12,
                cyclomatic: 12.0,
                cognitive: 9.0,
            })
        );
    }

    #[test]
    fn complexity_json_deserializes_trimmed_rust_code_analysis_report() {
        let raw = r#"{
            "name": "example.rs",
            "start_line": 1,
            "end_line": 20,
            "kind": "unit",
            "spaces": [
                {
                    "name": "parse",
                    "start_line": 5,
                    "end_line": 12,
                    "kind": "function",
                    "spaces": [],
                    "metrics": {
                        "cyclomatic": { "sum": 7, "average": 7.0, "min": 7, "max": 7 },
                        "cognitive": { "sum": 4, "average": 4.0, "min": 4, "max": 4 },
                        "loc": { "sloc": 8, "ploc": 6, "lloc": 5, "cloc": 1, "blank": 1 }
                    }
                }
            ],
            "metrics": {
                "cyclomatic": { "sum": 9, "average": 9.0, "min": 9, "max": 9 },
                "cognitive": { "sum": 5, "average": 5.0, "min": 5, "max": 5 },
                "loc": { "sloc": 20, "ploc": 16, "lloc": 12, "cloc": 2, "blank": 2 }
            }
        }"#;

        let report: Space = serde_json::from_str(raw).unwrap();

        assert_eq!(report.metrics.cyclomatic.sum, 9.0);
        assert_eq!(report.metrics.cognitive.sum, 5.0);
        assert_eq!(report.metrics.loc.sloc, 20.0);
        assert_eq!(
            worst_space(&report),
            Some(WorstSpace {
                name: "parse".to_owned(),
                start_line: 5,
                cyclomatic: 7.0,
                cognitive: 4.0,
            })
        );
    }

    #[test]
    fn top_complexity_sorts_and_truncates_by_cyclomatic_then_cognitive() {
        let files = vec![
            FileComplexity {
                path: PathBuf::from("a.rs"),
                sloc: 10.0,
                cyclomatic: 5.0,
                cognitive: 3.0,
                worst: None,
            },
            FileComplexity {
                path: PathBuf::from("b.rs"),
                sloc: 10.0,
                cyclomatic: 8.0,
                cognitive: 2.0,
                worst: None,
            },
            FileComplexity {
                path: PathBuf::from("c.rs"),
                sloc: 10.0,
                cyclomatic: 8.0,
                cognitive: 6.0,
                worst: None,
            },
        ];

        let paths: Vec<_> = top_complexity(files, 2)
            .into_iter()
            .map(|file| file.path)
            .collect();

        assert_eq!(paths, vec![PathBuf::from("c.rs"), PathBuf::from("b.rs")]);
    }
}
