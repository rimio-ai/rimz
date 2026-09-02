use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::conform::{self, Direction};
use super::contract::{AssemblyExpectation, PassContract};
use super::facts::{Facets, Facts};
use super::inspect;
use super::modules::{
    ItemId, bounded_names, crate_module_for_path, escaping_items_for_boundary, path_in_scope,
};
use super::references::{Edge, EdgeKind, FunctionId};
use super::sources;
use super::syntax::Spelling;
use super::target::{self, LayerRanks, TARGET_FILE};
use super::{positive_usize, set_once, validate_scope, value};

const DEFAULT_TOP: usize = 20;

const USAGE: &str =
    "cargo xtask atlas diff (--base <ref> --path <scope> | --expect <contract.toml>) [--top N]

Proves structural movement from an indexed base revision to the indexed working
tree. --expect supplies the base and one or more paths from a v1 pass contract.

  --base <ref>       base revision for an exploratory diff
  --path <scope>     root-relative boundary for an exploratory diff
  --expect <file>    v1 executable pass contract
  --top N            detail rows shown per section (default 20)";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    input: Input,
    top: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum Input {
    Base { base: String, path: PathBuf },
    Expect(PathBuf),
}

#[derive(Clone, Debug)]
struct ValueDelta {
    base: u64,
    current: u64,
    delta: i64,
}

#[derive(Clone, Debug)]
struct SurfaceItem {
    scope: PathBuf,
    id: ItemId,
    path: PathBuf,
    line: usize,
}

#[derive(Debug)]
struct BoundarySurface {
    scope: PathBuf,
    items: Vec<SurfaceItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DependencyKey {
    from: String,
    to: String,
    path: PathBuf,
    item: String,
    spelling: Spelling,
}

#[derive(Clone, Debug)]
struct DependencySite {
    key: DependencyKey,
    line: usize,
    direction: &'static str,
}

#[derive(Clone, Debug, Default)]
struct EdgeData {
    items: BTreeSet<String>,
    by_fn: BTreeMap<FunctionId, BTreeSet<String>>,
}

impl EdgeData {
    fn assembly(&self) -> usize {
        self.by_fn.values().map(BTreeSet::len).max().unwrap_or(0)
    }

    fn heaviest(&self) -> Option<(&FunctionId, usize)> {
        self.by_fn
            .iter()
            .map(|(function, items)| (function, items.len()))
            .max_by(|(left_fn, left), (right_fn, right)| {
                left.cmp(right).then_with(|| right_fn.cmp(left_fn))
            })
    }
}

type EdgeMap = BTreeMap<(String, String), EdgeData>;

#[derive(Clone, Debug)]
struct InterfaceRow {
    from: String,
    to: String,
    base: usize,
    current: usize,
    base_heaviest: Option<String>,
    current_heaviest: Option<String>,
}

#[derive(Clone, Debug)]
struct AssemblyCheck {
    expectation: AssemblyExpectation,
    base: usize,
    current: usize,
}

#[derive(Clone, Debug)]
struct ExpectationRow {
    assertion: String,
    landed: bool,
    detail: String,
}

#[derive(Debug)]
struct Evidence {
    base_parse_failures: Vec<PathBuf>,
    current_parse_failures: Vec<PathBuf>,
    newly_unresolved: Vec<SurfaceItem>,
}

impl Evidence {
    fn complete(&self) -> bool {
        self.base_parse_failures.is_empty()
            && self.current_parse_failures.is_empty()
            && self.newly_unresolved.is_empty()
    }
}

#[derive(Debug)]
struct Report {
    base: String,
    paths: Vec<PathBuf>,
    production: ValueDelta,
    tests: ValueDelta,
    base_surface: Vec<BoundarySurface>,
    current_surface: Vec<BoundarySurface>,
    dependencies_added: Vec<DependencySite>,
    dependencies_removed: Vec<DependencySite>,
    base_dependency_counts: BTreeMap<&'static str, usize>,
    current_dependency_counts: BTreeMap<&'static str, usize>,
    interfaces: Vec<InterfaceRow>,
    rust_files_base: usize,
    rust_files_current: usize,
    changed_inside: Vec<PathBuf>,
    changed_outside: Vec<PathBuf>,
    evidence: Evidence,
    expectations: Vec<ExpectationRow>,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas diff output is the command stdout contract"
)]
pub(super) fn run(root: &Path, raw: &[String]) -> Result<()> {
    let Some(args) = parse_args(raw)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let current = Facts::load(
        root,
        Path::new("."),
        Facets {
            references: true,
            ..Facets::default()
        },
    )?;
    let (base, paths, contract) = match &args.input {
        Input::Base { base, path } => (base.clone(), vec![path.clone()], None),
        Input::Expect(path) => {
            let contract = super::contract::load(root, path, &current.syntax.files)?;
            (
                contract.base.clone(),
                contract.paths.clone(),
                Some(contract),
            )
        }
    };
    let base_commit = resolve_base(root, &base)?;
    let base_facts = Facts::load_at(root, Path::new("."), &base_commit, true)?;
    let report = build_report(root, base, paths, contract.as_ref(), &base_facts, &current)?;
    print_report(&report, args.top);

    let drifted = report
        .expectations
        .iter()
        .filter(|row| !row.landed)
        .map(|row| format!("{}: {}", row.assertion, row.detail))
        .collect::<Vec<_>>();
    if !drifted.is_empty() {
        bail!(
            "atlas diff expectations drifted:\n- {}",
            drifted.join("\n- ")
        );
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut base = None;
    let mut path = None;
    let mut expect = None;
    let mut top = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--base" => {
                let parsed = value(args, index, "diff", "--base")?;
                if parsed.is_empty() {
                    bail!("atlas diff --base requires a non-empty revision");
                }
                set_once(&mut base, parsed.to_owned(), "diff", "--base")?;
                index += 2;
            }
            "--path" => {
                let parsed = validate_scope(value(args, index, "diff", "--path")?, "--path")?;
                set_once(&mut path, parsed, "diff", "--path")?;
                index += 2;
            }
            "--expect" => {
                let parsed = value(args, index, "diff", "--expect")?;
                if parsed.is_empty() {
                    bail!("atlas diff --expect requires a non-empty path");
                }
                set_once(&mut expect, PathBuf::from(parsed), "diff", "--expect")?;
                index += 2;
            }
            "--top" => {
                let parsed = positive_usize(value(args, index, "diff", "--top")?, "diff", "--top")?;
                set_once(&mut top, parsed, "diff", "--top")?;
                index += 2;
            }
            _ => bail!("unknown atlas diff flag `{arg}`\n\n{USAGE}"),
        }
    }
    let input = match (base, path, expect) {
        (Some(base), Some(path), None) => Input::Base { base, path },
        (None, None, Some(expect)) => Input::Expect(expect),
        (None, None, None) => bail!("atlas diff requires --base with --path, or --expect"),
        _ => bail!("atlas diff accepts either --base with --path, or --expect, not both"),
    };
    Ok(Some(Args {
        input,
        top: top.unwrap_or(DEFAULT_TOP),
    }))
}

fn resolve_base(root: &Path, reference: &str) -> Result<String> {
    let verify = format!("{reference}^{{commit}}");
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &verify])
        .current_dir(root)
        .output()
        .with_context(|| format!("resolving atlas diff base `{reference}`"))?;
    if !output.status.success() {
        bail!(
            "atlas diff base `{reference}` is not a commit: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .context("git rev-parse returned a non-UTF-8 commit")?
        .trim()
        .to_owned())
}

fn build_report(
    root: &Path,
    base_name: String,
    paths: Vec<PathBuf>,
    contract: Option<&PassContract>,
    base: &Facts,
    current: &Facts,
) -> Result<Report> {
    if !base
        .sources
        .iter()
        .chain(&current.sources)
        .any(|source| in_paths(&source.path, &paths))
    {
        bail!("no Rust files under the requested Atlas diff paths");
    }
    let target = target::load(&root.join(TARGET_FILE))?;
    let ranks = target.as_ref().map(|target| target.layer_ranks());
    let base_dependencies = dependencies(base, &paths, ranks.as_ref());
    let current_dependencies = dependencies(current, &paths, ranks.as_ref());
    let dependencies_added = difference(&current_dependencies, &base_dependencies);
    let dependencies_removed = difference(&base_dependencies, &current_dependencies);
    let base_edges = interface_edges(base, &paths);
    let current_edges = interface_edges(current, &paths);
    let interfaces = interface_rows(&base_edges, &current_edges);
    let base_surface = boundary_surfaces(base, &paths);
    let current_surface = boundary_surfaces(current, &paths);
    let evidence = evidence(base, current, &paths);
    let changed = sources::changed_paths(root, &base_name)?;
    let (changed_inside, changed_outside) = split_changed_paths(&changed, &paths);
    let production = size_delta(base, current, &paths, false);
    let tests = size_delta(base, current, &paths, true);
    let assembly_checks = if let Some(contract) = contract {
        contract
            .assembly
            .iter()
            .map(|expectation| {
                Ok(AssemblyCheck {
                    expectation: expectation.clone(),
                    base: contract_assembly(root, base, expectation, true)?,
                    current: contract_assembly(root, current, expectation, false)?,
                })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    let expectations = contract.map_or_else(Vec::new, |contract| {
        expectation_rows(
            contract,
            production.delta,
            &assembly_checks,
            &changed_outside,
            evidence.complete(),
        )
    });
    let rust_files_base = rust_file_count(base, &paths);
    let rust_files_current = rust_file_count(current, &paths);

    Ok(Report {
        base: base_name,
        paths,
        production,
        tests,
        base_surface,
        current_surface,
        dependencies_added,
        dependencies_removed,
        base_dependency_counts: dependency_counts(&base_dependencies),
        current_dependency_counts: dependency_counts(&current_dependencies),
        interfaces,
        rust_files_base,
        rust_files_current,
        changed_inside,
        changed_outside,
        evidence,
        expectations,
    })
}

fn in_paths(path: &Path, paths: &[PathBuf]) -> bool {
    paths.iter().any(|scope| in_boundary(path, scope))
}

fn in_boundary(path: &Path, scope: &Path) -> bool {
    path_in_scope(path, scope)
        || (!scope.extension().is_some_and(|extension| extension == "rs")
            && path == scope.with_extension("rs"))
}

fn size_delta(base: &Facts, current: &Facts, paths: &[PathBuf], tests: bool) -> ValueDelta {
    let total = |facts: &Facts| {
        facts
            .sizes
            .iter()
            .filter(|(path, _)| in_paths(path, paths))
            .map(|(_, size)| if tests { size.tests } else { size.code })
            .sum::<u64>()
    };
    let base = total(base);
    let current = total(current);
    ValueDelta {
        base,
        current,
        delta: current as i64 - base as i64,
    }
}

fn boundary_surfaces(facts: &Facts, paths: &[PathBuf]) -> Vec<BoundarySurface> {
    paths
        .iter()
        .map(|scope| {
            let files = facts
                .syntax
                .files
                .iter()
                .filter(|file| in_boundary(&file.path, scope))
                .collect::<Vec<_>>();
            let entry = if scope.extension().is_some_and(|extension| extension == "rs") {
                scope.clone()
            } else {
                scope.join("mod.rs")
            };
            let module = crate_module_for_path(&entry);
            let items = escaping_items_for_boundary(&files, &module, &facts.mod_index)
                .into_iter()
                .map(|item| SurfaceItem {
                    scope: scope.clone(),
                    id: item.id,
                    path: item.path,
                    line: item.line,
                })
                .collect();
            BoundarySurface {
                scope: scope.clone(),
                items,
            }
        })
        .collect()
}

fn surface_changes(left: &[BoundarySurface], right: &[BoundarySurface]) -> Vec<SurfaceItem> {
    left.iter()
        .flat_map(|surface| {
            let other_ids = right
                .iter()
                .find(|candidate| candidate.scope == surface.scope)
                .into_iter()
                .flat_map(|surface| surface.items.iter().map(|item| &item.id))
                .collect::<BTreeSet<_>>();
            surface
                .items
                .iter()
                .filter(move |item| !other_ids.contains(&item.id))
                .cloned()
        })
        .collect()
}

fn dependencies(
    facts: &Facts,
    paths: &[PathBuf],
    ranks: Option<&LayerRanks>,
) -> BTreeMap<DependencyKey, DependencySite> {
    let mut sites = BTreeMap::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| in_paths(&file.path, paths))
    {
        for dependency in &file.dependencies {
            let Some(to) = super::syntax::resolved_internal_import(
                dependency,
                &facts.known_modules,
                &facts.crate_names,
            ) else {
                continue;
            };
            if file.module_path == to {
                continue;
            }
            let direction = ranks
                .and_then(|ranks| conform::layer_direction(ranks, &file.module_path, &to))
                .map_or("unranked", direction_label);
            let key = DependencyKey {
                from: file.module_path.clone(),
                to,
                path: file.path.clone(),
                item: dependency.item.clone(),
                spelling: dependency.spelling,
            };
            sites.entry(key.clone()).or_insert(DependencySite {
                key,
                line: dependency.line,
                direction,
            });
        }
    }
    sites
}

fn direction_label(direction: Direction) -> &'static str {
    match direction {
        Direction::Downward => "downward",
        Direction::Same => "same",
        Direction::Upward => "upward",
    }
}

fn difference(
    left: &BTreeMap<DependencyKey, DependencySite>,
    right: &BTreeMap<DependencyKey, DependencySite>,
) -> Vec<DependencySite> {
    left.iter()
        .filter(|(key, _)| !right.contains_key(*key))
        .map(|(_, site)| site.clone())
        .collect()
}

fn dependency_counts(
    sites: &BTreeMap<DependencyKey, DependencySite>,
) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for site in sites.values() {
        *counts.entry(site.direction).or_default() += 1;
    }
    counts
}

fn interface_edges(facts: &Facts, paths: &[PathBuf]) -> EdgeMap {
    collect_reference_edges(
        facts
            .references
            .as_ref()
            .expect("diff loads required reference evidence")
            .edges
            .iter(),
        paths,
    )
}

fn collect_reference_edges<'a>(
    edges: impl Iterator<Item = &'a Edge>,
    paths: &[PathBuf],
) -> EdgeMap {
    let mut rows = EdgeMap::new();
    for edge in edges.filter(|edge| {
        edge.kind == EdgeKind::Reference
            && !edge.test
            && (in_paths(&edge.from_path, paths) ^ in_paths(&edge.to_path, paths))
    }) {
        let data = rows
            .entry((edge.from.clone(), edge.to.clone()))
            .or_default();
        data.items.insert(edge.item.clone());
        if let Some(function) = &edge.from_fn {
            data.by_fn
                .entry(FunctionId::new(&edge.from_path, function))
                .or_default()
                .insert(edge.item.clone());
        }
    }
    rows
}

fn interface_rows(base: &EdgeMap, current: &EdgeMap) -> Vec<InterfaceRow> {
    base.keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|(from, to)| {
            let base_data = base.get(&(from.clone(), to.clone()));
            let current_data = current.get(&(from.clone(), to.clone()));
            InterfaceRow {
                from,
                to,
                base: base_data.map_or(0, EdgeData::assembly),
                current: current_data.map_or(0, EdgeData::assembly),
                base_heaviest: base_data.and_then(heaviest_label),
                current_heaviest: current_data.and_then(heaviest_label),
            }
        })
        .collect()
}

fn heaviest_label(data: &EdgeData) -> Option<String> {
    data.heaviest().map(|(function, _)| {
        format!(
            "{} ({}:{})",
            function.label,
            function.path.display(),
            function.line
        )
    })
}

fn contract_assembly(
    root: &Path,
    facts: &Facts,
    expectation: &AssemblyExpectation,
    absent_as_zero: bool,
) -> Result<usize> {
    let from = inspect::resolve_module(
        root,
        &facts.syntax.files,
        &expectation.from,
        "diff",
        "contract assembly.from",
    );
    let to = inspect::resolve_module(
        root,
        &facts.syntax.files,
        &expectation.to,
        "diff",
        "contract assembly.to",
    );
    let (from, to) = match (from, to) {
        (Ok(from), Ok(to)) => (from, to),
        _ if absent_as_zero => return Ok(0),
        (Err(error), _) | (_, Err(error)) => return Err(error),
    };
    let mut functions = BTreeMap::<FunctionId, BTreeSet<String>>::new();
    for edge in facts
        .references
        .as_ref()
        .expect("diff loads required reference evidence")
        .edges
        .iter()
        .filter(|edge| {
            edge.kind == EdgeKind::Reference
                && !edge.test
                && from.matches(&edge.from, &edge.from_path)
                && to.matches(&edge.to, &edge.to_path)
        })
    {
        if let Some(function) = &edge.from_fn {
            functions
                .entry(FunctionId::new(&edge.from_path, function))
                .or_default()
                .insert(edge.item.clone());
        }
    }
    Ok(functions.values().map(BTreeSet::len).max().unwrap_or(0))
}

fn resolution(facts: &Facts) -> BTreeMap<ItemId, bool> {
    let references = facts
        .references
        .as_ref()
        .expect("diff loads required reference evidence");
    let mut resolution = BTreeMap::<ItemId, bool>::new();
    for file in &facts.syntax.files {
        for item in &file.pub_items {
            let id = ItemId {
                module: item.module.clone(),
                kind: item.kind.clone(),
                name: item.name.clone(),
            };
            let resolved = references.get(file, item).is_some();
            resolution
                .entry(id)
                .and_modify(|existing| *existing |= resolved)
                .or_insert(resolved);
        }
    }
    resolution
}

fn evidence(base: &Facts, current: &Facts, paths: &[PathBuf]) -> Evidence {
    let parse_failures = |facts: &Facts| {
        facts
            .syntax
            .parse_failures
            .iter()
            .filter(|path| in_paths(path, paths))
            .cloned()
            .collect::<Vec<_>>()
    };
    let base_resolution = resolution(base);
    let current_resolution = resolution(current);
    let newly_unresolved = current
        .syntax
        .files
        .iter()
        .filter(|file| in_paths(&file.path, paths))
        .flat_map(|file| {
            let base_resolution = &base_resolution;
            let current_resolution = &current_resolution;
            file.pub_items.iter().filter_map(move |item| {
                let id = ItemId {
                    module: item.module.clone(),
                    kind: item.kind.clone(),
                    name: item.name.clone(),
                };
                (base_resolution.get(&id) == Some(&true)
                    && current_resolution.get(&id) == Some(&false))
                .then(|| SurfaceItem {
                    scope: paths
                        .iter()
                        .find(|scope| in_boundary(&file.path, scope))
                        .cloned()
                        .unwrap_or_default(),
                    id,
                    path: file.path.clone(),
                    line: item.line,
                })
            })
        })
        .collect();
    Evidence {
        base_parse_failures: parse_failures(base),
        current_parse_failures: parse_failures(current),
        newly_unresolved,
    }
}

fn rust_file_count(facts: &Facts, paths: &[PathBuf]) -> usize {
    facts
        .sources
        .iter()
        .filter(|source| in_paths(&source.path, paths))
        .count()
}

fn split_changed_paths(
    changed: &BTreeSet<PathBuf>,
    paths: &[PathBuf],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    changed
        .iter()
        .cloned()
        .partition(|path| in_paths(path, paths))
}

fn expectation_rows(
    contract: &PassContract,
    production_delta: i64,
    assembly: &[AssemblyCheck],
    changed_outside: &[PathBuf],
    evidence_complete: bool,
) -> Vec<ExpectationRow> {
    let mut rows = vec![ExpectationRow {
        assertion: "production SLOC".to_owned(),
        landed: production_delta <= contract.max_production_sloc_delta,
        detail: format!(
            "delta {production_delta:+}, ceiling {:+}",
            contract.max_production_sloc_delta
        ),
    }];
    rows.extend(assembly.iter().map(|check| ExpectationRow {
        assertion: format!(
            "assembly {} → {}",
            check.expectation.from, check.expectation.to
        ),
        landed: check.current < check.base && check.current <= check.expectation.max_items,
        detail: format!(
            "max/fn {} → {}, ceiling {}",
            check.base, check.current, check.expectation.max_items
        ),
    }));
    rows.push(ExpectationRow {
        assertion: "changed paths".to_owned(),
        landed: changed_outside.is_empty(),
        detail: if changed_outside.is_empty() {
            "all changes are inside contract paths".to_owned()
        } else {
            format!("{} outside contract paths", changed_outside.len())
        },
    });
    rows.push(ExpectationRow {
        assertion: "evidence complete".to_owned(),
        landed: evidence_complete,
        detail: if evidence_complete {
            "no parse failures or newly unresolved definitions".to_owned()
        } else {
            "parse failures or newly unresolved definitions remain".to_owned()
        },
    });
    rows
}

#[expect(clippy::print_stdout, reason = "atlas diff report helpers")]
fn print_report(report: &Report, top: usize) {
    println!("# Atlas diff — {} → working tree", report.base);
    println!(
        "\nPaths: {}",
        bounded_names(
            &report
                .paths
                .iter()
                .map(|path| format!("`{}`", path.display()))
                .collect::<Vec<_>>(),
            report.paths.len(),
        )
    );
    if !report.expectations.is_empty() {
        println!("\n## Expectations\n");
        println!("| assertion | status | evidence |");
        println!("|---|---|---|");
        for row in &report.expectations {
            println!(
                "| {} | {} | {} |",
                row.assertion,
                if row.landed { "landed" } else { "drifted" },
                row.detail
            );
        }
    }

    println!("\n## Totals\n");
    println!("| measure | base | current | delta |");
    println!("|---|---:|---:|---:|");
    print_value_row("production SLOC", &report.production);
    print_value_row("test SLOC", &report.tests);
    for current in &report.current_surface {
        let base = report
            .base_surface
            .iter()
            .find(|base| base.scope == current.scope)
            .map_or(0, |base| base.items.len());
        println!(
            "| esc `{}` | {} | {} | {:+} |",
            current.scope.display(),
            base,
            current.items.len(),
            current.items.len() as i64 - base as i64
        );
    }
    for direction in ["downward", "same", "upward", "unranked"] {
        let base = report
            .base_dependency_counts
            .get(direction)
            .copied()
            .unwrap_or(0);
        let current = report
            .current_dependency_counts
            .get(direction)
            .copied()
            .unwrap_or(0);
        if base > 0 || current > 0 {
            println!(
                "| dependency sites ({direction}) | {base} | {current} | {:+} |",
                current as i64 - base as i64
            );
        }
    }
    println!(
        "| Rust files | {} | {} | {:+} |",
        report.rust_files_base,
        report.rust_files_current,
        report.rust_files_current as i64 - report.rust_files_base as i64
    );

    println!("\n## Call-site interface\n");
    println!("| caller → provider | max/fn | heaviest function |");
    println!("|---|---:|---|");
    for row in report.interfaces.iter().take(top) {
        println!(
            "| {} → {} | {} → {} | {} → {} |",
            row.from,
            row.to,
            row.base,
            row.current,
            row.base_heaviest.as_deref().unwrap_or("none"),
            row.current_heaviest.as_deref().unwrap_or("none")
        );
    }
    print_omitted(report.interfaces.len(), top);

    let surface_added = surface_changes(&report.current_surface, &report.base_surface);
    let surface_removed = surface_changes(&report.base_surface, &report.current_surface);
    println!("\n## Escaping surface\n");
    println!("| movement | boundary | item | site |");
    println!("|---|---|---|---|");
    for (movement, item) in surface_added
        .iter()
        .map(|item| ("added", item))
        .chain(surface_removed.iter().map(|item| ("removed", item)))
        .take(top)
    {
        println!(
            "| {movement} | `{}` | `{}::{}` | `{}:{}` |",
            item.scope.display(),
            item.id.module,
            item.id.name,
            item.path.display(),
            item.line
        );
    }
    print_omitted(surface_added.len() + surface_removed.len(), top);

    println!("\n## Dependencies\n");
    println!("| movement | direction | module pair | item | site |");
    println!("|---|---|---|---|---|");
    for (movement, site) in report
        .dependencies_added
        .iter()
        .map(|site| ("added", site))
        .chain(
            report
                .dependencies_removed
                .iter()
                .map(|site| ("removed", site)),
        )
        .take(top)
    {
        println!(
            "| {movement} | {} | {} → {} | `{}{}` | `{}:{}` |",
            site.direction,
            site.key.from,
            site.key.to,
            site.key.item,
            spelling_suffix(site.key.spelling),
            site.key.path.display(),
            site.line
        );
    }
    print_omitted(
        report.dependencies_added.len() + report.dependencies_removed.len(),
        top,
    );

    println!("\n## Files\n");
    println!("| location | path |");
    println!("|---|---|");
    for (location, path) in report
        .changed_inside
        .iter()
        .map(|path| ("inside", path))
        .chain(report.changed_outside.iter().map(|path| ("outside", path)))
        .take(top)
    {
        println!("| {location} | `{}` |", path.display());
    }
    print_omitted(
        report.changed_inside.len() + report.changed_outside.len(),
        top,
    );

    println!("\n## Incomplete evidence\n");
    if report.evidence.complete() {
        println!("None.");
        return;
    }
    for path in &report.evidence.base_parse_failures {
        println!("- base parse failure: `{}`", path.display());
    }
    for path in &report.evidence.current_parse_failures {
        println!("- current parse failure: `{}`", path.display());
    }
    for item in &report.evidence.newly_unresolved {
        println!(
            "- newly unresolved: `{}::{}` at `{}:{}`",
            item.id.module,
            item.id.name,
            item.path.display(),
            item.line
        );
    }
}

#[expect(clippy::print_stdout, reason = "atlas diff report helpers")]
fn print_value_row(name: &str, value: &ValueDelta) {
    println!(
        "| {name} | {} | {} | {:+} |",
        value.base, value.current, value.delta
    );
}

#[expect(clippy::print_stdout, reason = "atlas diff report helpers")]
fn print_omitted(total: usize, top: usize) {
    if total > top {
        println!("\n_{} more rows omitted._", total - top);
    }
}

fn spelling_suffix(spelling: Spelling) -> &'static str {
    match spelling {
        Spelling::Use => "",
        Spelling::Qualified => " (qualified)",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use super::super::references::{FnRef, References};
    use super::super::sources::Source;
    use super::*;

    #[test]
    fn diff_args_require_base_and_path_or_expect() {
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["--base".into(), "HEAD".into()]).is_err());
        assert!(
            parse_args(&[
                "--base".into(),
                "HEAD".into(),
                "--path".into(),
                "src".into(),
                "--expect".into(),
                "pass.toml".into(),
            ])
            .is_err()
        );
        assert_eq!(
            parse_args(&[
                "--base".into(),
                "HEAD~1".into(),
                "--path".into(),
                "src".into(),
            ])
            .unwrap()
            .unwrap(),
            Args {
                input: Input::Base {
                    base: "HEAD~1".to_owned(),
                    path: PathBuf::from("src"),
                },
                top: 20,
            }
        );
    }

    #[test]
    fn assembly_delta_uses_the_per_function_maximum() {
        let base_edges = [
            reference_edge("src/a.rs", "One", Some(10)),
            reference_edge("src/a.rs", "Two", Some(10)),
            reference_edge("src/a.rs", "Three", Some(30)),
            reference_edge("src/a.rs", "Four", Some(30)),
            reference_edge("src/a.rs", "Outside", None),
        ];
        let current_edges = [
            reference_edge("src/a.rs", "One", Some(10)),
            reference_edge("src/a.rs", "Two", Some(10)),
            reference_edge("src/a.rs", "Three", Some(10)),
            reference_edge("src/a.rs", "Four", Some(10)),
            reference_edge("src/a.rs", "Outside", None),
        ];
        let paths = [PathBuf::from("src/a.rs")];
        let base = collect_reference_edges(base_edges.iter(), &paths);
        let current = collect_reference_edges(current_edges.iter(), &paths);
        let pair = ("caller".to_owned(), "target".to_owned());

        assert_eq!(base[&pair].items, current[&pair].items);
        assert_eq!(base[&pair].assembly(), 2);
        assert_eq!(current[&pair].assembly(), 4);
    }

    #[test]
    fn diff_expect_requires_call_site_shrink() {
        let contract = contract(-1);
        let checks = [AssemblyCheck {
            expectation: contract.assembly[0].clone(),
            base: 4,
            current: 4,
        }];
        let rows = expectation_rows(&contract, -2, &checks, &[], true);

        assert!(!rows[1].landed);
        assert!(rows[1].detail.contains("4 → 4"));
    }

    #[test]
    fn diff_expect_enforces_negative_sloc_budget() {
        let contract = contract(-3);
        let checks = [AssemblyCheck {
            expectation: contract.assembly[0].clone(),
            base: 4,
            current: 2,
        }];

        assert!(!expectation_rows(&contract, -2, &checks, &[], true)[0].landed);
        assert!(expectation_rows(&contract, -3, &checks, &[], true)[0].landed);
    }

    #[test]
    fn diff_expect_rejects_changes_outside_paths() {
        let changed = BTreeSet::from([
            PathBuf::from("src/store/mod.rs"),
            PathBuf::from("README.md"),
        ]);
        let (_, outside) = split_changed_paths(&changed, &[PathBuf::from("src/store")]);
        let contract = contract(-1);
        let checks = [AssemblyCheck {
            expectation: contract.assembly[0].clone(),
            base: 4,
            current: 2,
        }];
        let rows = expectation_rows(&contract, -2, &checks, &outside, true);

        assert_eq!(outside, [PathBuf::from("README.md")]);
        assert!(
            !rows
                .iter()
                .find(|row| row.assertion == "changed paths")
                .unwrap()
                .landed
        );
    }

    #[test]
    fn contract_assembly_treats_a_base_only_missing_endpoint_as_zero() {
        let root = tempfile::tempdir().unwrap();
        let sources = vec![Source::new("src/caller.rs", "fn call() {}")];
        let syntax = super::super::syntax::analyze_sources(&sources, &BTreeSet::new());
        let facts = Facts {
            root: root.path().to_path_buf(),
            scope: PathBuf::from("."),
            mod_index: super::super::syntax::ModIndex::new(&syntax.files),
            known_modules: syntax
                .files
                .iter()
                .map(|file| file.module_path.clone())
                .collect(),
            syntax,
            sources,
            crate_names: BTreeSet::new(),
            sizes: BTreeMap::new(),
            history: None,
            metrics: None,
            references: Some(References::default()),
        };
        let expectation = AssemblyExpectation {
            from: "caller".to_owned(),
            to: "newmod".to_owned(),
            max_items: 1,
        };

        assert_eq!(
            contract_assembly(root.path(), &facts, &expectation, true).unwrap(),
            0
        );
        assert!(
            contract_assembly(root.path(), &facts, &expectation, false)
                .unwrap_err()
                .to_string()
                .contains("atlas diff contract assembly.to")
        );
    }

    #[test]
    fn diff_reports_boundary_esc_not_leaf_sums() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/domain")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"atlas-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(root.path().join("src/lib.rs"), "pub mod domain;\n").unwrap();
        fs::write(root.path().join("src/domain/mod.rs"), "mod detail;\n").unwrap();
        fs::write(
            root.path().join("src/domain/detail.rs"),
            "pub fn helper() {}\n",
        )
        .unwrap();
        let facts = Facts::load(root.path(), Path::new("."), Facets::default()).unwrap();

        let boundary = boundary_surfaces(&facts, &[PathBuf::from("src/domain")]);
        let leaf = escaping_items_for_boundary(
            &[facts
                .syntax
                .files
                .iter()
                .find(|file| file.path == Path::new("src/domain/detail.rs"))
                .unwrap()],
            "domain::detail",
            &facts.mod_index,
        );

        assert!(boundary[0].items.is_empty());
        assert_eq!(leaf.len(), 1);
    }

    fn contract(max_production_sloc_delta: i64) -> PassContract {
        PassContract {
            version: 1,
            base: "HEAD".to_owned(),
            paths: vec![PathBuf::from("src")],
            max_production_sloc_delta,
            assembly: vec![AssemblyExpectation {
                from: "caller".to_owned(),
                to: "target".to_owned(),
                max_items: 3,
            }],
        }
    }

    fn reference_edge(path: &str, item: &str, function_line: Option<usize>) -> Edge {
        Edge {
            from_path: PathBuf::from(path),
            to_path: PathBuf::from("src/target.rs"),
            from: "caller".to_owned(),
            to: "target".to_owned(),
            item: item.to_owned(),
            kind: EdgeKind::Reference,
            test: false,
            from_line: function_line.unwrap_or(1),
            from_fn: function_line.map(|line| FnRef {
                label: "run".to_owned(),
                line,
            }),
        }
    }
}
