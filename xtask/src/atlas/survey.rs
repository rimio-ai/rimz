use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::detect::{self, PassThrough, RepeatedGuard, VestigialItem};
use super::facts::{Facets, Facts};
use super::history::{self, CochangeEdge};
use super::index::IndexPolicy;
use super::modules::{
    crate_module_for_path, crate_module_for_row, module_for_path, module_is_within, path_in_scope,
};
use super::{REPORT_VERSION, positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";

const USAGE: &str = "cargo xtask atlas survey [--path <prefix>] [--top N] [--md|--json] [--no-index] [--split-above N|--no-split] [--guard-files N]

Builds one shared facts model and emits the architecture-review reading queue.";

#[derive(Debug)]
struct Args {
    path: PathBuf,
    top: usize,
    md: bool,
    json: bool,
    no_index: bool,
    split_above: usize,
    no_split: bool,
    guard_files: usize,
}

#[derive(Clone, Debug, Serialize)]
struct RankRow {
    module: String,
    code: u64,
    tests: u64,
    pub_items: usize,
    escaping_items: usize,
    complexity: f64,
    children: Vec<RankRow>,
}

#[derive(Clone, Debug, Serialize)]
struct Provider {
    provider: String,
    modules: usize,
    items: usize,
}

#[derive(Clone, Debug, Serialize)]
struct CochangeCluster {
    members: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct Divergence {
    pub(super) kind: &'static str,
    pub(super) left: String,
    pub(super) right: String,
    pub(super) items: usize,
    pub(super) cochanges: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    path: PathBuf,
    history_commits: usize,
    history_start: i64,
    history_end: i64,
    total_code: u64,
    total_tests: u64,
    rank: Vec<RankRow>,
    cochange_clusters: Vec<CochangeCluster>,
    cochange_edges: Vec<CochangeEdge>,
    divergence: Vec<Divergence>,
    shapes: serde_json::Value,
    external_providers: Vec<Provider>,
    passthroughs: Vec<PassThrough>,
    vestigial_items: Vec<VestigialItem>,
    repeated_guards: Vec<RepeatedGuard>,
    detector_counts: BTreeMap<String, BTreeMap<String, usize>>,
    parse_failures: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas survey output is a command stdout contract"
)]
#[expect(
    clippy::print_stderr,
    reason = "interactive atlas survey suggests capturing its long report"
)]
pub(super) fn run(root: &Path, raw: &[String]) -> Result<()> {
    let Some(args) = parse_args(raw)? else {
        println!("{USAGE}");
        return Ok(());
    };
    if args.no_index {
        super::note_no_index();
    }
    let facts = Facts::load(
        root,
        &args.path,
        Facets {
            history: true,
            metrics: true,
            references: Some(if args.no_index {
                IndexPolicy::Skip
            } else {
                IndexPolicy::Required
            }),
            blame: true,
        },
    )?;
    let report = build_report(&facts, &args)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("rendering atlas survey JSON")?
        );
    } else {
        if std::io::stdout().is_terminal() {
            eprintln!("atlas: survey is long; use --md > survey.md to keep the reading queue");
        }
        print_markdown(&report, args.top, args.md);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut path = None;
    let mut top = None;
    let mut split_above = None;
    let mut guard_files = None;
    let mut md = false;
    let mut json = false;
    let mut no_index = false;
    let mut no_split = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--path" => {
                let parsed = validate_scope(value(args, index, "survey", "--path")?, "--path")?;
                set_once(&mut path, parsed, "survey", "--path")?;
                index += 2;
            }
            "--top" => {
                let parsed =
                    positive_usize(value(args, index, "survey", "--top")?, "survey", "--top")?;
                set_once(&mut top, parsed, "survey", "--top")?;
                index += 2;
            }
            "--split-above" => {
                let parsed = positive_usize(
                    value(args, index, "survey", "--split-above")?,
                    "survey",
                    "--split-above",
                )?;
                set_once(&mut split_above, parsed, "survey", "--split-above")?;
                index += 2;
            }
            "--guard-files" => {
                let parsed = positive_usize(
                    value(args, index, "survey", "--guard-files")?,
                    "survey",
                    "--guard-files",
                )?;
                set_once(&mut guard_files, parsed, "survey", "--guard-files")?;
                index += 2;
            }
            "--md" if !md => {
                md = true;
                index += 1;
            }
            "--json" if !json => {
                json = true;
                index += 1;
            }
            "--no-index" if !no_index => {
                no_index = true;
                index += 1;
            }
            "--no-split" if !no_split => {
                no_split = true;
                index += 1;
            }
            "--md" | "--json" | "--no-index" | "--no-split" => {
                bail!("atlas survey flag `{arg}` may only be passed once")
            }
            _ => bail!("unknown atlas survey argument `{arg}`"),
        }
    }
    if md && json {
        bail!("atlas survey --md and --json are mutually exclusive");
    }
    Ok(Some(Args {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        top: top.unwrap_or(20),
        md,
        json,
        no_index,
        split_above: split_above.unwrap_or(8_000),
        no_split,
        guard_files: guard_files.unwrap_or(3),
    }))
}

fn build_report(facts: &Facts, args: &Args) -> Result<Report> {
    let log = facts
        .history
        .as_ref()
        .context("survey history facts missing")?;
    let mut rank = rank_rows(facts, &args.path, "", args)?;
    rank.sort_by(|left, right| {
        right
            .code
            .cmp(&left.code)
            .then_with(|| left.module.cmp(&right.module))
    });
    let cochange = history::cochange(log, &facts.root, &args.path, None, 25, 10)?;
    let passthroughs = detect::passthroughs(facts, &args.path);
    let vestigial_items = detect::vestigial(facts, &args.path, 25);
    let repeated_guards = detect::guards(facts, &args.path, args.guard_files);
    let detector_counts = BTreeMap::from([
        (
            "passthroughs".to_owned(),
            detect::counts_by_module(&passthroughs, |row| &row.module),
        ),
        (
            "vestigial".to_owned(),
            detect::counts_by_module(&vestigial_items, |row| &row.module),
        ),
    ]);
    let divergence = divergence(facts, &args.path, &cochange.edges, 3);
    let cochange_clusters = cochange_clusters(&cochange.edges, 3);
    Ok(Report {
        version: REPORT_VERSION,
        verb: "survey",
        path: facts.scope.clone(),
        history_commits: cochange.commits,
        history_start: log.first_time(),
        history_end: log.last_time(),
        total_code: rank.iter().map(|row| row.code).sum(),
        total_tests: rank.iter().map(|row| row.tests).sum(),
        rank,
        cochange_clusters,
        cochange_edges: cochange.edges,
        divergence,
        shapes: super::shapes::survey_value(facts, &args.path)?,
        external_providers: providers(facts, &args.path),
        passthroughs,
        vestigial_items,
        repeated_guards,
        detector_counts,
        parse_failures: facts
            .syntax
            .parse_failures
            .iter()
            .filter(|path| path_in_scope(path, &args.path))
            .count(),
    })
}

fn cochange_clusters(edges: &[CochangeEdge], minimum: usize) -> Vec<CochangeCluster> {
    let mut graph = BTreeMap::<String, BTreeSet<String>>::new();
    for edge in edges.iter().filter(|edge| edge.commits >= minimum) {
        graph
            .entry(edge.left.clone())
            .or_default()
            .insert(edge.right.clone());
        graph
            .entry(edge.right.clone())
            .or_default()
            .insert(edge.left.clone());
    }
    let mut unseen = graph.keys().cloned().collect::<BTreeSet<_>>();
    let mut clusters = Vec::new();
    while let Some(start) = unseen.pop_first() {
        let mut pending = vec![start];
        let mut members = BTreeSet::new();
        while let Some(module) = pending.pop() {
            if !members.insert(module.clone()) {
                continue;
            }
            if let Some(neighbors) = graph.get(&module) {
                for neighbor in neighbors {
                    if unseen.remove(neighbor) {
                        pending.push(neighbor.clone());
                    }
                }
            }
        }
        clusters.push(CochangeCluster {
            members: members.into_iter().collect(),
        });
    }
    clusters.sort_by(|left, right| {
        right
            .members
            .len()
            .cmp(&left.members.len())
            .then_with(|| left.members.cmp(&right.members))
    });
    clusters
}

pub(super) fn divergence(
    facts: &Facts,
    scope: &Path,
    cochange: &[CochangeEdge],
    minimum: usize,
) -> Vec<Divergence> {
    let scope_module = crate_module_for_path(&scope.join("mod.rs"));
    let endpoint = |module: &str| {
        let relative = module
            .strip_prefix(&scope_module)
            .and_then(|value| value.strip_prefix("::"))
            .unwrap_or(module);
        relative
            .split("::")
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or("(root)")
            .to_owned()
    };
    let mut coupling = BTreeMap::<(String, String), BTreeSet<String>>::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
    {
        let from = module_for_path(&file.path, scope);
        for import in &file.imports {
            let Some(resolved) = super::syntax::resolved_internal_import(
                import,
                &facts.known_modules,
                &facts.crate_names,
            ) else {
                continue;
            };
            let to = endpoint(&resolved);
            if from != to {
                let pair = ordered_pair(&from, &to);
                coupling
                    .entry(pair)
                    .or_default()
                    .insert(import.item.clone());
            }
        }
    }
    if let Some(references) = &facts.references {
        for edge in references.edges.iter().filter(|edge| !edge.test) {
            let from = endpoint(&edge.from);
            let to = endpoint(&edge.to);
            if from != to {
                coupling
                    .entry(ordered_pair(&from, &to))
                    .or_default()
                    .insert(edge.item.clone());
            }
        }
    }
    let changes = cochange
        .iter()
        .map(|edge| (ordered_pair(&edge.left, &edge.right), edge.commits))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    for edge in cochange.iter().filter(|edge| edge.commits >= minimum) {
        let pair = ordered_pair(&edge.left, &edge.right);
        if !coupling.contains_key(&pair) {
            rows.push(Divergence {
                kind: "cochange-without-edge",
                left: pair.0,
                right: pair.1,
                items: 0,
                cochanges: edge.commits,
            });
        }
    }
    for ((left, right), items) in coupling {
        let cochanges = changes
            .get(&(left.clone(), right.clone()))
            .copied()
            .unwrap_or(0);
        if cochanges == 0 {
            rows.push(Divergence {
                kind: "edge-without-cochange",
                left,
                right,
                items: items.len(),
                cochanges,
            });
        }
    }
    rows.sort_by(|left, right| {
        right
            .cochanges
            .cmp(&left.cochanges)
            .then_with(|| right.items.cmp(&left.items))
            .then_with(|| left.left.cmp(&right.left))
            .then_with(|| left.right.cmp(&right.right))
    });
    rows
}

fn ordered_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    }
}

fn rank_rows(facts: &Facts, scope: &Path, prefix: &str, args: &Args) -> Result<Vec<RankRow>> {
    let mut rows = BTreeMap::<String, RankRow>::new();
    for (path, size) in facts
        .sizes
        .iter()
        .filter(|(path, _)| path_in_scope(path, scope))
    {
        let module = module_for_path(path, scope);
        let display = if prefix.is_empty() {
            module.clone()
        } else {
            format!("{prefix}/{module}")
        };
        let row = rows.entry(module).or_insert(RankRow {
            module: display,
            code: 0,
            tests: 0,
            pub_items: 0,
            escaping_items: 0,
            complexity: 0.0,
            children: Vec::new(),
        });
        row.code += size.code;
        row.tests += size.tests;
    }
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
    {
        let module = module_for_path(&file.path, scope);
        let Some(row) = rows.get_mut(&module) else {
            continue;
        };
        let target = crate_module_for_row(scope, &module);
        row.pub_items += file.pub_items.len();
        row.escaping_items += file
            .pub_items
            .iter()
            .filter(|item| !module_is_within(&facts.mod_index.effective_reach(file, item), &target))
            .count();
    }
    if let Some(metrics) = &facts.metrics {
        for function in metrics
            .functions
            .iter()
            .filter(|function| path_in_scope(&function.path, scope))
        {
            if let Some(row) = rows.get_mut(&module_for_path(&function.path, scope)) {
                row.complexity += function.score;
            }
        }
    }
    for (local, row) in &mut rows {
        if args.no_split || row.code <= args.split_above as u64 || local == "(root)" {
            continue;
        }
        let child_scope = scope.join(local);
        if facts.root.join(&child_scope).is_dir() {
            row.children = rank_rows(facts, &child_scope, &row.module, args)?;
        }
    }
    Ok(rows.into_values().collect())
}

fn providers(facts: &Facts, scope: &Path) -> Vec<Provider> {
    let scope_module = crate_module_for_path(&scope.join("mod.rs"));
    let mut rows = BTreeMap::<String, (BTreeSet<String>, BTreeSet<String>)>::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
    {
        let from = module_for_path(&file.path, scope);
        for import in &file.imports {
            let Some(resolved) = super::syntax::resolved_internal_import(
                import,
                &facts.known_modules,
                &facts.crate_names,
            ) else {
                continue;
            };
            if module_is_within(&resolved, &scope_module) {
                continue;
            }
            let provider = resolved.split("::").next().unwrap_or("(crate)").to_owned();
            let row = rows.entry(provider).or_default();
            row.0.insert(from.clone());
            row.1.insert(import.item.clone());
        }
    }
    let mut rows = rows
        .into_iter()
        .map(|(provider, (modules, items))| Provider {
            provider,
            modules: modules.len(),
            items: items.len(),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| left.provider.cmp(&right.provider))
    });
    rows
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas survey output is a command stdout contract"
)]
fn print_markdown(report: &Report, top: usize, markdown: bool) {
    let fence = if markdown { "```" } else { "" };
    println!("# Atlas survey — {}", report.path.display());
    println!(
        "history: {} commits; code: {}; tests: {}",
        report.history_commits, report.total_code, report.total_tests
    );
    section("Rank", fence);
    println!("module                              code  tests   pub   esc      cx");
    for row in report.rank.iter().take(top) {
        print_rank(row, 0);
    }
    close(fence);
    section("Co-change reading assignments", fence);
    for cluster in report.cochange_clusters.iter().take(top) {
        println!("{}", cluster.members.join(" <> "));
    }
    close(fence);
    section("Divergence", fence);
    for row in report.divergence.iter().take(top) {
        println!(
            "{:<24} {:<24} {:>5} {:>5} {}",
            row.left, row.right, row.items, row.cochanges, row.kind
        );
    }
    close(fence);
    section("External providers", fence);
    for row in report.external_providers.iter().take(top) {
        println!(
            "{:<28} modules {:>3} items {:>4}",
            row.provider, row.modules, row.items
        );
    }
    close(fence);
    section("Shapes", fence);
    if let Some(clusters) = report
        .shapes
        .get("clusters")
        .and_then(serde_json::Value::as_array)
    {
        for cluster in clusters.iter().take(top) {
            println!("{}", serde_json::to_string(cluster).unwrap_or_default());
        }
    }
    close(fence);
    section("Pass-throughs", fence);
    for row in report.passthroughs.iter().take(top) {
        println!(
            "{}:{} {} -> {}",
            row.path.display(),
            row.line,
            row.name,
            row.callee
        );
    }
    close(fence);
    section("Vestigial items", fence);
    for row in report.vestigial_items.iter().take(top) {
        println!(
            "{}:{} {} ({}d)",
            row.path.display(),
            row.line,
            row.name,
            row.age_days
        );
    }
    close(fence);
    section("Repeated guards", fence);
    for row in report.repeated_guards.iter().take(top) {
        println!("{} files  {}", row.files, row.predicate);
    }
    close(fence);
}

#[expect(clippy::print_stdout, reason = "atlas report helper")]
fn section(name: &str, fence: &str) {
    println!("\n## {name}");
    if !fence.is_empty() {
        println!("{fence}");
    }
}

#[expect(clippy::print_stdout, reason = "atlas report helper")]
fn close(fence: &str) {
    if !fence.is_empty() {
        println!("{fence}");
    }
}

#[expect(clippy::print_stdout, reason = "atlas report helper")]
fn print_rank(row: &RankRow, indent: usize) {
    println!(
        "{:<35} {:>6} {:>6} {:>5} {:>5} {:>7.1}",
        format!("{}{}", " ".repeat(indent), row.module),
        row.code,
        row.tests,
        row.pub_items,
        row.escaping_items,
        row.complexity
    );
    for child in &row.children {
        print_rank(child, indent + 2);
    }
}
