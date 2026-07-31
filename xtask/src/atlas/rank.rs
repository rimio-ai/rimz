use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::source_files;

use super::api::{OccurrenceCorpus, median};
use super::history;
use super::metrics::{self, FunctionMetric};
use super::modules::module_for_path;
use super::sources;
use super::syntax;
use super::{finite_nonnegative, positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";

const USAGE: &str = "cargo xtask atlas rank [--path <prefix>] [--top N] [--window <pct>] [--since <ref>] [--verbose] [--json]

Ranks modules by churn-weighted size (code × churn%); cx breaks ties.
`cx` sums severity-weighted cognitive, cyclomatic, and source-line overruns for
the module's functions; functions below the warning thresholds contribute zero.
Flags: pin = churn% >= threshold and test/code below threshold; hot = non-noisy
pace >= threshold; shallow = wide, thin, and low-use public surface; hub = wide,
thin, and high-use public surface. Occurrence counts are whole-word identifiers
outside the defining file-module, not resolved caller counts.
Requires rust-code-analysis-cli (`cargo install rust-code-analysis-cli --locked`).

  --path <path>          root-relative subtree (default crates/rimz/src)
  --top N                module rows (default 20)
  --window <pct>         recent history window (default 25)
  --noise-lifetime N     minimum lifetime commits for pace (default 20)
  --noise-window N       minimum window commits for pace (default 5)
  --pin-churn N          pin churn-percent threshold (default 3)
  --pin-tc N             pin test/code ceiling (default 0.30)
  --hot-pace N           hot pace threshold (default 1.5)
  --shallow-pub N        shallow public-item threshold (default 20)
  --shallow-locpub N     shallow lines/public ceiling (default 30)
  --shallow-occ N        shallow occurrence/item ceiling (default 3)
  --since <ref>          add code and public-item deltas
  --verbose              list top offender functions for shown modules
  --json                 versioned JSON agent contract (v1)";

#[derive(Debug)]
struct Args {
    path: PathBuf,
    top: usize,
    window: usize,
    noise_lifetime: usize,
    noise_window: usize,
    pin_churn: f64,
    pin_tc: f64,
    hot_pace: f64,
    shallow_pub: usize,
    shallow_locpub: f64,
    shallow_occ: f64,
    since: Option<String>,
    verbose: bool,
    json: bool,
}

#[derive(Clone, Debug, Default)]
struct Size {
    code: u64,
    tests: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
struct Row {
    module: String,
    code: u64,
    tests: u64,
    pub_items: usize,
    loc_per_pub: Option<f64>,
    occurrence_median: f64,
    churn_pct: f64,
    pace: Option<f64>,
    complexity: f64,
    test_code_ratio: Option<f64>,
    flags: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_pub: Option<isize>,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    path: PathBuf,
    history_commits: usize,
    total_modules: usize,
    total_code: u64,
    total_tests: u64,
    total_pub_items: usize,
    total_complexity: f64,
    rows: Vec<Row>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    offenders: Vec<FunctionMetric>,
    parse_failures: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas rank output is a command stdout contract"
)]
pub(super) fn run(root: &Path, args: &[String]) -> Result<()> {
    let Some(args) = parse_args(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let report = build_report(root, &args)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("rendering atlas rank JSON")?
        );
    } else {
        print_report(&report, args.since.is_some());
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut path = None;
    let mut top = None;
    let mut window = None;
    let mut noise_lifetime = None;
    let mut noise_window = None;
    let mut pin_churn = None;
    let mut pin_tc = None;
    let mut hot_pace = None;
    let mut shallow_pub = None;
    let mut shallow_locpub = None;
    let mut shallow_occ = None;
    let mut since = None;
    let mut verbose = false;
    let mut json = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--path" => {
                let parsed = validate_scope(value(args, index, "rank", "--path")?, "--path")?;
                set_once(&mut path, parsed, "rank", "--path")?;
                index += 2;
            }
            "--top" => {
                let parsed = positive_usize(value(args, index, "rank", "--top")?, "rank", "--top")?;
                set_once(&mut top, parsed, "rank", "--top")?;
                index += 2;
            }
            "--window" => {
                let parsed =
                    positive_usize(value(args, index, "rank", "--window")?, "rank", "--window")?;
                if parsed > 100 {
                    bail!("atlas rank --window must not exceed 100");
                }
                set_once(&mut window, parsed, "rank", "--window")?;
                index += 2;
            }
            "--noise-lifetime" => {
                let parsed = positive_usize(
                    value(args, index, "rank", "--noise-lifetime")?,
                    "rank",
                    "--noise-lifetime",
                )?;
                set_once(&mut noise_lifetime, parsed, "rank", "--noise-lifetime")?;
                index += 2;
            }
            "--noise-window" => {
                let parsed = positive_usize(
                    value(args, index, "rank", "--noise-window")?,
                    "rank",
                    "--noise-window",
                )?;
                set_once(&mut noise_window, parsed, "rank", "--noise-window")?;
                index += 2;
            }
            "--pin-churn" => {
                let parsed = finite_nonnegative(
                    value(args, index, "rank", "--pin-churn")?,
                    "rank",
                    "--pin-churn",
                )?;
                set_once(&mut pin_churn, parsed, "rank", "--pin-churn")?;
                index += 2;
            }
            "--pin-tc" => {
                let parsed = finite_nonnegative(
                    value(args, index, "rank", "--pin-tc")?,
                    "rank",
                    "--pin-tc",
                )?;
                set_once(&mut pin_tc, parsed, "rank", "--pin-tc")?;
                index += 2;
            }
            "--hot-pace" => {
                let parsed = finite_nonnegative(
                    value(args, index, "rank", "--hot-pace")?,
                    "rank",
                    "--hot-pace",
                )?;
                set_once(&mut hot_pace, parsed, "rank", "--hot-pace")?;
                index += 2;
            }
            "--shallow-pub" => {
                let parsed = positive_usize(
                    value(args, index, "rank", "--shallow-pub")?,
                    "rank",
                    "--shallow-pub",
                )?;
                set_once(&mut shallow_pub, parsed, "rank", "--shallow-pub")?;
                index += 2;
            }
            "--shallow-locpub" => {
                let parsed = finite_nonnegative(
                    value(args, index, "rank", "--shallow-locpub")?,
                    "rank",
                    "--shallow-locpub",
                )?;
                set_once(&mut shallow_locpub, parsed, "rank", "--shallow-locpub")?;
                index += 2;
            }
            "--shallow-occ" => {
                let parsed = finite_nonnegative(
                    value(args, index, "rank", "--shallow-occ")?,
                    "rank",
                    "--shallow-occ",
                )?;
                set_once(&mut shallow_occ, parsed, "rank", "--shallow-occ")?;
                index += 2;
            }
            "--since" => {
                let reference = value(args, index, "rank", "--since")?.to_owned();
                set_once(&mut since, reference, "rank", "--since")?;
                index += 2;
            }
            "--verbose" if !verbose => {
                verbose = true;
                index += 1;
            }
            "--verbose" => bail!("atlas rank --verbose may only be passed once"),
            "--json" if !json => {
                json = true;
                index += 1;
            }
            "--json" => bail!("atlas rank --json may only be passed once"),
            _ => bail!("unknown atlas rank argument `{arg}`"),
        }
    }
    Ok(Some(Args {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        top: top.unwrap_or(20),
        window: window.unwrap_or(25),
        noise_lifetime: noise_lifetime.unwrap_or(20),
        noise_window: noise_window.unwrap_or(5),
        pin_churn: pin_churn.unwrap_or(3.0),
        pin_tc: pin_tc.unwrap_or(0.30),
        hot_pace: hot_pace.unwrap_or(1.5),
        shallow_pub: shallow_pub.unwrap_or(20),
        shallow_locpub: shallow_locpub.unwrap_or(30.0),
        shallow_occ: shallow_occ.unwrap_or(3.0),
        since,
        verbose,
        json,
    }))
}

fn build_report(root: &Path, args: &Args) -> Result<Report> {
    let current_sources = sources::scope_sources(root, &args.path, None)?;
    let all_sources = sources::scope_sources(root, Path::new("."), None)?;
    let occurrence_corpus = OccurrenceCorpus::new(&all_sources);
    let syntax = syntax::analyze_sources(&current_sources);
    let current_sizes = sizes(&current_sources, &args.path);
    let current_pub = public_counts(&syntax.files, &args.path);
    let current_occurrences = occurrence_medians(&syntax.files, &args.path, &occurrence_corpus);
    let previous = args
        .since
        .as_deref()
        .map(|reference| sources::scope_sources(root, &args.path, Some(reference)))
        .transpose()?;
    let previous_sizes = previous.as_ref().map(|sources| sizes(sources, &args.path));
    let previous_pub = previous
        .as_ref()
        .map(|sources| public_counts(&syntax::analyze_sources(sources).files, &args.path));
    let pace = history::pace(
        root,
        &args.path,
        args.window,
        args.noise_lifetime,
        args.noise_window,
    )?;
    let metrics = metrics::analyze(root, &args.path, &current_sources)?;

    let mut rows = current_sizes
        .iter()
        .map(|(module, size)| {
            let pub_items = current_pub.get(module).copied().unwrap_or(0);
            let loc_per_pub = (pub_items > 0).then_some(size.code as f64 / pub_items as f64);
            let occurrence_median = current_occurrences.get(module).copied().unwrap_or(0.0);
            let history = pace.modules.get(module).cloned().unwrap_or_default();
            let churn_pct = history.share * 100.0;
            let complexity = metrics.module_scores.get(module).copied().unwrap_or(0.0);
            let test_code_ratio = (size.code > 0).then_some(size.tests as f64 / size.code as f64);
            let mut flags = Vec::new();
            if is_pinned(churn_pct, test_code_ratio, args.pin_churn, args.pin_tc) {
                flags.push("pin");
            }
            if history.pace.is_some_and(|pace| pace >= args.hot_pace) {
                flags.push("hot");
            }
            if let Some(flag) = surface_flag(
                pub_items,
                loc_per_pub,
                occurrence_median,
                args.shallow_pub,
                args.shallow_locpub,
                args.shallow_occ,
            ) {
                flags.push(flag);
            }
            Row {
                module: module.clone(),
                code: size.code,
                tests: size.tests,
                pub_items,
                loc_per_pub,
                occurrence_median,
                churn_pct,
                pace: history.pace,
                complexity,
                test_code_ratio,
                flags,
                delta_code: previous_sizes.as_ref().map(|previous| {
                    size.code as i64 - previous.get(module).map_or(0, |size| size.code) as i64
                }),
                delta_pub: previous_pub.as_ref().map(|previous| {
                    pub_items as isize - previous.get(module).copied().unwrap_or(0) as isize
                }),
            }
        })
        .collect::<Vec<_>>();
    sort_rows(&mut rows);
    let total_modules = rows.len();
    let total_code = rows.iter().map(|row| row.code).sum();
    let total_tests = rows.iter().map(|row| row.tests).sum();
    let total_pub_items = rows.iter().map(|row| row.pub_items).sum();
    let total_complexity = rows.iter().map(|row| row.complexity).sum();
    rows.truncate(args.top);
    let shown = rows
        .iter()
        .map(|row| row.module.as_str())
        .collect::<Vec<_>>();
    let offenders = if args.verbose {
        metrics
            .functions
            .into_iter()
            .filter(|function| function.score > 0.0 && shown.contains(&function.module.as_str()))
            .take(args.top.saturating_mul(3).min(60))
            .collect()
    } else {
        Vec::new()
    };
    Ok(Report {
        version: 1,
        verb: "rank",
        path: args.path.clone(),
        history_commits: pace.commits,
        total_modules,
        total_code,
        total_tests,
        total_pub_items,
        total_complexity,
        rows,
        offenders,
        parse_failures: syntax.parse_failures.len(),
    })
}

fn is_pinned(churn_pct: f64, test_code_ratio: Option<f64>, pin_churn: f64, pin_tc: f64) -> bool {
    churn_pct >= pin_churn && test_code_ratio.is_some_and(|ratio| ratio < pin_tc)
}

fn surface_flag(
    pub_items: usize,
    loc_per_pub: Option<f64>,
    occurrence_median: f64,
    shallow_pub: usize,
    shallow_locpub: f64,
    shallow_occ: f64,
) -> Option<&'static str> {
    if pub_items < shallow_pub || !loc_per_pub.is_some_and(|ratio| ratio < shallow_locpub) {
        return None;
    }
    Some(if occurrence_median < shallow_occ {
        "shallow"
    } else {
        "hub"
    })
}

fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|left, right| {
        let left_value = left.code as f64 * left.churn_pct;
        let right_value = right.code as f64 * right.churn_pct;
        right_value
            .total_cmp(&left_value)
            .then_with(|| right.complexity.total_cmp(&left.complexity))
            .then_with(|| right.code.cmp(&left.code))
            .then_with(|| left.module.cmp(&right.module))
    });
}

fn sizes(source_list: &[sources::Source], scope: &Path) -> BTreeMap<String, Size> {
    let mut sizes = BTreeMap::<String, Size>::new();
    for source in source_list {
        let module = module_for_path(&source.path, scope);
        let (code, tests) = if source.is_test() {
            (0, source_files::rust_sloc(&source.text))
        } else if !source.is_production() {
            (0, 0)
        } else {
            source_files::split_rust_sloc(&source.text)
        };
        let size = sizes.entry(module).or_default();
        size.code += code;
        size.tests += tests;
    }
    sizes
}

fn public_counts(files: &[super::syntax::FileSyntax], scope: &Path) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for file in files {
        *counts
            .entry(module_for_path(&file.path, scope))
            .or_default() += file.pub_items.len();
    }
    counts
}

fn occurrence_medians(
    files: &[super::syntax::FileSyntax],
    scope: &Path,
    corpus: &OccurrenceCorpus,
) -> BTreeMap<String, f64> {
    let mut occurrences = BTreeMap::<String, Vec<usize>>::new();
    for file in files {
        let module = module_for_path(&file.path, scope);
        occurrences.entry(module).or_default().extend(
            file.pub_items
                .iter()
                .map(|item| corpus.count_from_module(&file.module_path, &item.name).0),
        );
    }
    occurrences
        .into_iter()
        .map(|(module, values)| (module, median(values)))
        .collect()
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas rank report is the command's stdout contract"
)]
fn print_report(report: &Report, show_delta: bool) {
    println!("Atlas rank — {}", report.path.display());
    if show_delta {
        println!(
            "module                 code   pub  loc/pub  churn%  pace      cx    t/c  flags          Δcode Δpub"
        );
    } else {
        println!("module                 code   pub  loc/pub  churn%  pace      cx    t/c  flags");
    }
    for row in &report.rows {
        let loc_per_pub = row
            .loc_per_pub
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}"));
        let pace = row
            .pace
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.2}"));
        let test_code = row
            .test_code_ratio
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.2}"));
        let flags = if row.flags.is_empty() {
            "—".to_owned()
        } else {
            row.flags.join(",")
        };
        if show_delta {
            println!(
                "{:<22} {:>6} {:>5} {:>8} {:>7.1} {:>5} {:>7.1} {:>6}  {:<12} {:+6} {:+4}",
                row.module,
                row.code,
                row.pub_items,
                loc_per_pub,
                row.churn_pct,
                pace,
                row.complexity,
                test_code,
                flags,
                row.delta_code.unwrap_or(0),
                row.delta_pub.unwrap_or(0)
            );
        } else {
            println!(
                "{:<22} {:>6} {:>5} {:>8} {:>7.1} {:>5} {:>7.1} {:>6}  {}",
                row.module,
                row.code,
                row.pub_items,
                loc_per_pub,
                row.churn_pct,
                pace,
                row.complexity,
                test_code,
                flags
            );
        }
    }
    if report.total_modules > report.rows.len() {
        println!(
            "… and {} more modules; overall: code {}, tests {}, pub {}, cx {:.1}",
            report.total_modules - report.rows.len(),
            report.total_code,
            report.total_tests,
            report.total_pub_items,
            report.total_complexity
        );
    } else {
        println!(
            "overall: code {}, tests {}, pub {}, cx {:.1}",
            report.total_code, report.total_tests, report.total_pub_items, report.total_complexity
        );
    }
    if !report.offenders.is_empty() {
        println!();
        println!("Offender functions");
        for function in &report.offenders {
            println!(
                "{} {}:{} {} cx {:.0}/{:.0} sloc {:.0} score {:.1}",
                function.module,
                function.path.display(),
                function.line,
                function.name,
                function.cyclomatic,
                function.cognitive,
                function.sloc,
                function.score
            );
        }
    }
    println!(
        "history: {} commits; parse failures: {}",
        report.history_commits, report.parse_failures
    );
}

#[cfg(test)]
mod tests;
