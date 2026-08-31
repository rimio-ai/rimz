use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::facts::{Facets, Facts, FileSize};
use super::history;
use super::metrics::FunctionMetric;
use super::modules::{crate_module_for_row, module_for_path, module_is_within};
use super::syntax;
use super::{REPORT_VERSION, finite_nonnegative, positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";

const USAGE: &str = "cargo xtask atlas rank [--path <prefix>] [--top N] [--window <pct>] [--since <ref>] [--verbose] [--json]

Ranks modules by churn-weighted size (code × churn%); cx breaks ties.
`cx` sums severity-weighted cognitive, cyclomatic, and source-line overruns for
the module's functions; functions below the warning thresholds contribute zero.
Flags: pin = churn% >= threshold and test/code below threshold; hot = non-noisy
pace >= threshold; shallow = wide, thin, and low-use escaping surface; hub = wide,
thin, and high-use escaping surface. Reference medians count exact outside
production modules from the rust-analyzer SCIP index.
Requires rust-code-analysis-cli (`cargo install rust-code-analysis-cli --locked`).

  --path <path>          root-relative subtree (default crates/rimz/src)
  --top N                module rows (default 20)
  --window <pct>         recent history window (default 25)
  --noise-lifetime N     minimum lifetime commits for pace (default 20)
  --noise-window N       minimum window commits for pace (default 5)
  --pin-churn N          pin churn-percent threshold (default 3)
  --pin-tc N             pin test/code ceiling (default 0.30)
  --hot-pace N           hot pace threshold (default 1.5)
  --shallow-pub N        shallow escaping-item threshold (default 20)
  --shallow-locpub N     shallow lines/escaping-item ceiling (default 120)
  --hub-refs N           shallow/hub outside-caller median boundary (default 2)
  --no-index             omit exact-reference columns and use the thin flag
  --split-above N        recursively split directory rows above N SLOC (default 8000)
  --no-split             keep the report at one level
  --since <ref>          add row deltas and complete totals deltas
  --verbose              list top offender functions for shown modules
  --json                 versioned JSON agent contract (v3)";

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
    hub_refs: f64,
    no_index: bool,
    split_above: usize,
    no_split: bool,
    since: Option<String>,
    verbose: bool,
    json: bool,
}

type Size = FileSize;

#[derive(Clone, Debug, Default, Serialize)]
struct Row {
    module: String,
    code: u64,
    tests: u64,
    pub_items: usize,
    escaping_items: usize,
    loc_per_escape: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ref_median: Option<f64>,
    churn_pct: f64,
    pace: Option<f64>,
    complexity: f64,
    test_code_ratio: Option<f64>,
    flags: Vec<&'static str>,
    children: Vec<Row>,
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
    total_escaping_items: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_tests: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_pub: Option<isize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_esc: Option<isize>,
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
    if args.no_index {
        super::note_no_index();
    }
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
    let mut hub_refs = None;
    let mut no_index = false;
    let mut split_above = None;
    let mut no_split = false;
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
            "--hub-refs" => {
                let parsed = finite_nonnegative(
                    value(args, index, "rank", "--hub-refs")?,
                    "rank",
                    "--hub-refs",
                )?;
                set_once(&mut hub_refs, parsed, "rank", "--hub-refs")?;
                index += 2;
            }
            "--no-index" if !no_index => {
                no_index = true;
                index += 1;
            }
            "--no-index" => bail!("atlas rank --no-index may only be passed once"),
            "--split-above" => {
                let parsed = positive_usize(
                    value(args, index, "rank", "--split-above")?,
                    "rank",
                    "--split-above",
                )?;
                set_once(&mut split_above, parsed, "rank", "--split-above")?;
                index += 2;
            }
            "--no-split" if !no_split => {
                no_split = true;
                index += 1;
            }
            "--no-split" => bail!("atlas rank --no-split may only be passed once"),
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
        shallow_locpub: shallow_locpub.unwrap_or(120.0),
        hub_refs: hub_refs.unwrap_or(2.0),
        no_index,
        split_above: split_above.unwrap_or(8_000),
        no_split,
        since,
        verbose,
        json,
    }))
}

fn build_report(root: &Path, args: &Args) -> Result<Report> {
    let facts = Facts::load(
        root,
        &args.path,
        Facets {
            history: true,
            metrics: true,
            references: Some(if args.no_index {
                super::index::IndexPolicy::Skip
            } else {
                super::index::IndexPolicy::Required
            }),
            ..Facets::default()
        },
    )?;
    let current_files = facts
        .syntax
        .files
        .iter()
        .filter(|file| super::modules::path_in_scope(&file.path, &args.path))
        .cloned()
        .collect::<Vec<_>>();
    let current_sizes = sizes(&facts, &args.path);
    let current_pub = public_counts(&current_files, &args.path);
    let current_escaping = escaping_counts(&current_files, &args.path, &facts.mod_index);
    let current_refs = reference_medians(&current_files, &args.path, &facts);
    let previous = args
        .since
        .as_deref()
        .map(|reference| Facts::load_at(root, &args.path, reference))
        .transpose()?;
    let previous_sizes = previous.as_ref().map(|facts| sizes(facts, &args.path));
    let previous_pub = previous.as_ref().map(|facts| {
        let files = facts
            .syntax
            .files
            .iter()
            .filter(|file| super::modules::path_in_scope(&file.path, &args.path))
            .cloned()
            .collect::<Vec<_>>();
        public_counts(&files, &args.path)
    });
    let previous_escaping = previous.as_ref().map(|facts| {
        let files = facts
            .syntax
            .files
            .iter()
            .filter(|file| super::modules::path_in_scope(&file.path, &args.path))
            .cloned()
            .collect::<Vec<_>>();
        escaping_counts(&files, &args.path, &facts.mod_index)
    });
    let log = facts
        .history
        .as_ref()
        .context("rank history facts missing")?;
    let pace = history::pace(
        log,
        root,
        &args.path,
        args.window,
        args.noise_lifetime,
        args.noise_window,
    )?;
    let metrics = facts
        .metrics
        .as_ref()
        .context("rank metric facts missing")?;

    let mut rows = current_sizes
        .iter()
        .map(|(module, size)| {
            let pub_items = current_pub.get(module).copied().unwrap_or(0);
            let escaping_items = current_escaping.get(module).copied().unwrap_or(0);
            let loc_per_escape =
                (escaping_items > 0).then_some(size.code as f64 / escaping_items as f64);
            let ref_median = facts
                .references
                .as_ref()
                .map(|_| current_refs.get(module).copied().unwrap_or(0.0));
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
                escaping_items,
                loc_per_escape,
                ref_median,
                args.shallow_pub,
                args.shallow_locpub,
                args.hub_refs,
            ) {
                flags.push(flag);
            }
            Row {
                module: module.clone(),
                code: size.code,
                tests: size.tests,
                pub_items,
                escaping_items,
                loc_per_escape,
                ref_median,
                churn_pct,
                pace: history.pace,
                complexity,
                test_code_ratio,
                flags,
                children: Vec::new(),
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
    let total_escaping_items = rows.iter().map(|row| row.escaping_items).sum();
    let total_complexity = rows.iter().map(|row| row.complexity).sum();
    let delta_code = previous_sizes.as_ref().map(|previous| {
        total_code as i64 - previous.values().map(|size| size.code).sum::<u64>() as i64
    });
    let delta_tests = previous_sizes.as_ref().map(|previous| {
        total_tests as i64 - previous.values().map(|size| size.tests).sum::<u64>() as i64
    });
    let delta_pub = previous_pub
        .as_ref()
        .map(|previous| total_pub_items as isize - previous.values().sum::<usize>() as isize);
    let delta_esc = previous_escaping
        .as_ref()
        .map(|previous| total_escaping_items as isize - previous.values().sum::<usize>() as isize);
    rows.truncate(args.top);
    if !args.no_split {
        split_rows(&mut rows, &facts, args, &args.path, "")?;
    }
    let shown = rows
        .iter()
        .map(|row| row.module.as_str())
        .collect::<Vec<_>>();
    let offenders = if args.verbose {
        metrics
            .functions
            .iter()
            .filter(|function| function.score > 0.0 && shown.contains(&function.module.as_str()))
            .take(args.top.saturating_mul(3).min(60))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    Ok(Report {
        version: REPORT_VERSION,
        verb: "rank",
        path: args.path.clone(),
        history_commits: pace.commits,
        total_modules,
        total_code,
        total_tests,
        total_pub_items,
        total_escaping_items,
        delta_code,
        delta_tests,
        delta_pub,
        delta_esc,
        total_complexity,
        rows,
        offenders,
        parse_failures: facts
            .syntax
            .parse_failures
            .iter()
            .filter(|path| super::modules::path_in_scope(path, &args.path))
            .count(),
    })
}

fn is_pinned(churn_pct: f64, test_code_ratio: Option<f64>, pin_churn: f64, pin_tc: f64) -> bool {
    churn_pct >= pin_churn && test_code_ratio.is_some_and(|ratio| ratio < pin_tc)
}

fn surface_flag(
    escaping_items: usize,
    loc_per_escape: Option<f64>,
    ref_median: Option<f64>,
    shallow_pub: usize,
    shallow_locpub: f64,
    hub_refs: f64,
) -> Option<&'static str> {
    if escaping_items < shallow_pub || !loc_per_escape.is_some_and(|ratio| ratio < shallow_locpub) {
        return None;
    }
    Some(match ref_median {
        Some(median) if median < hub_refs => "shallow",
        Some(_) => "hub",
        None => "thin",
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

fn split_rows(
    rows: &mut [Row],
    facts: &Facts,
    args: &Args,
    scope: &Path,
    prefix: &str,
) -> Result<()> {
    for row in rows {
        let local = row
            .module
            .strip_prefix(prefix)
            .and_then(|value| value.strip_prefix('/'))
            .unwrap_or(&row.module);
        if local == "(root)" || row.code <= args.split_above as u64 {
            continue;
        }
        let child_scope = scope.join(local);
        if !facts.root.join(&child_scope).is_dir() {
            continue;
        }
        let mut children = child_rows(facts, args, &child_scope, &row.module)?;
        split_rows(&mut children, facts, args, &child_scope, &row.module)?;
        row.children = children;
    }
    Ok(())
}

fn child_rows(facts: &Facts, args: &Args, scope: &Path, prefix: &str) -> Result<Vec<Row>> {
    let files = facts
        .syntax
        .files
        .iter()
        .filter(|file| super::modules::path_in_scope(&file.path, scope))
        .cloned()
        .collect::<Vec<_>>();
    let sizes = sizes(facts, scope);
    let public = public_counts(&files, scope);
    let escaping = escaping_counts(&files, scope, &facts.mod_index);
    let refs = reference_medians(&files, scope, facts);
    let pace = history::pace(
        facts
            .history
            .as_ref()
            .context("rank history facts missing")?,
        &facts.root,
        scope,
        args.window,
        args.noise_lifetime,
        args.noise_window,
    )?;
    let mut complexity = BTreeMap::<String, f64>::new();
    if let Some(metrics) = &facts.metrics {
        for function in metrics
            .functions
            .iter()
            .filter(|function| super::modules::path_in_scope(&function.path, scope))
        {
            *complexity
                .entry(module_for_path(&function.path, scope))
                .or_default() += function.score;
        }
    }
    let mut rows = sizes
        .into_iter()
        .map(|(module, size)| {
            let pub_items = public.get(&module).copied().unwrap_or(0);
            let escaping_items = escaping.get(&module).copied().unwrap_or(0);
            let loc_per_escape =
                (escaping_items > 0).then_some(size.code as f64 / escaping_items as f64);
            let ref_median = facts
                .references
                .as_ref()
                .map(|_| refs.get(&module).copied().unwrap_or(0.0));
            let history = pace.modules.get(&module).cloned().unwrap_or_default();
            let churn_pct = history.share * 100.0;
            let test_code_ratio = (size.code > 0).then_some(size.tests as f64 / size.code as f64);
            let mut flags = Vec::new();
            if is_pinned(churn_pct, test_code_ratio, args.pin_churn, args.pin_tc) {
                flags.push("pin");
            }
            if history.pace.is_some_and(|pace| pace >= args.hot_pace) {
                flags.push("hot");
            }
            if let Some(flag) = surface_flag(
                escaping_items,
                loc_per_escape,
                ref_median,
                args.shallow_pub,
                args.shallow_locpub,
                args.hub_refs,
            ) {
                flags.push(flag);
            }
            Row {
                module: format!("{prefix}/{module}"),
                code: size.code,
                tests: size.tests,
                pub_items,
                escaping_items,
                loc_per_escape,
                ref_median,
                churn_pct,
                pace: history.pace,
                complexity: complexity.get(&module).copied().unwrap_or(0.0),
                test_code_ratio,
                flags,
                children: Vec::new(),
                delta_code: None,
                delta_pub: None,
            }
        })
        .collect::<Vec<_>>();
    sort_rows(&mut rows);
    Ok(rows)
}

fn sizes(facts: &Facts, scope: &Path) -> BTreeMap<String, Size> {
    let mut sizes = BTreeMap::<String, Size>::new();
    for (path, file_size) in &facts.sizes {
        if !super::modules::path_in_scope(path, scope) {
            continue;
        }
        let module = module_for_path(path, scope);
        let size = sizes.entry(module).or_default();
        size.code += file_size.code;
        size.tests += file_size.tests;
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

fn escaping_counts(
    files: &[super::syntax::FileSyntax],
    scope: &Path,
    mod_index: &syntax::ModIndex,
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for file in files {
        let row = module_for_path(&file.path, scope);
        let target_module = crate_module_for_row(scope, &row);
        let escaping = file
            .pub_items
            .iter()
            .filter(|item| {
                let reach = mod_index.effective_reach(file, item);
                !module_is_within(&reach, &target_module)
            })
            .count();
        *counts.entry(row).or_default() += escaping;
    }
    counts
}

fn reference_medians(
    files: &[super::syntax::FileSyntax],
    scope: &Path,
    facts: &Facts,
) -> BTreeMap<String, f64> {
    let Some(references) = &facts.references else {
        return BTreeMap::new();
    };
    let mut counts = BTreeMap::<String, Vec<usize>>::new();
    for file in files {
        let row = module_for_path(&file.path, scope);
        let target_module = crate_module_for_row(scope, &row);
        for item in &file.pub_items {
            let reach = facts.mod_index.effective_reach(file, item);
            if module_is_within(&reach, &target_module) {
                continue;
            }
            let Some(item_refs) = references.get(file, item) else {
                continue;
            };
            counts.entry(row.clone()).or_default().push(
                item_refs
                    .production
                    .iter()
                    .filter(|module| !module_is_within(module, &item.module))
                    .count(),
            );
        }
    }
    counts
        .into_iter()
        .map(|(module, values)| (module, super::api::median(values)))
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
            "module                 code   pub   esc  loc/esc  churn%  pace      cx    t/c  flags          Δcode Δpub"
        );
    } else {
        println!(
            "module                 code   pub   esc  loc/esc  churn%  pace      cx    t/c  flags"
        );
    }
    for row in &report.rows {
        print_row(row, show_delta, 0);
    }
    if report.total_modules > report.rows.len() {
        println!(
            "… and {} more modules; {}",
            report.total_modules - report.rows.len(),
            totals_line(report, show_delta)
        );
    } else {
        println!("{}", totals_line(report, show_delta));
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

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas rank report is the command's stdout contract"
)]
fn print_row(row: &Row, show_delta: bool, indent: usize) {
    let loc_per_escape = row
        .loc_per_escape
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
    let module = format!("{}{name}", " ".repeat(indent), name = row.module);
    if show_delta {
        println!(
            "{:<35} {:>6} {:>5} {:>5} {:>8} {:>7.1} {:>5} {:>7.1} {:>6}  {:<12} {:+6} {:+4}",
            module,
            row.code,
            row.pub_items,
            row.escaping_items,
            loc_per_escape,
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
            "{:<35} {:>6} {:>5} {:>5} {:>8} {:>7.1} {:>5} {:>7.1} {:>6}  {}",
            module,
            row.code,
            row.pub_items,
            row.escaping_items,
            loc_per_escape,
            row.churn_pct,
            pace,
            row.complexity,
            test_code,
            flags
        );
    }
    for child in &row.children {
        print_row(child, show_delta, indent + 2);
    }
}

fn totals_line(report: &Report, show_delta: bool) -> String {
    let base = format!(
        "overall: code {}, tests {}, pub {}, esc {}, cx {:.1}",
        report.total_code,
        report.total_tests,
        report.total_pub_items,
        report.total_escaping_items,
        report.total_complexity
    );
    if show_delta {
        format!(
            "{base}; Δcode {:+}, Δtests {:+}, Δpub {:+}, Δesc {:+}",
            report.delta_code.unwrap_or(0),
            report.delta_tests.unwrap_or(0),
            report.delta_pub.unwrap_or(0),
            report.delta_esc.unwrap_or(0),
        )
    } else {
        base
    }
}

#[cfg(test)]
mod tests;
