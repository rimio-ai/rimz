use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};

use super::conform::{self, Direction};
use super::contract::{AssemblyExpectation, DeleteExpectation, EscExpectation, PassContract};
use super::facts::{Facets, Facts};
use super::inspect;
use super::modules::{
    ItemId, bounded_names, crate_module_for_path, escaping_items_for_boundary, is_declaration_only,
    path_in_scope,
};
use super::output::{self, OutputArgs};
use super::references::{Edge, EdgeKind, FunctionId};
use super::sources;
use super::syntax::Spelling;
use super::target::{self, LayerRanks, TARGET_FILE};
use super::{positive_usize, set_once, validate_scope, value};

const DEFAULT_TOP: usize = 20;

const USAGE: &str =
    "cargo xtask atlas diff (--base <ref> --path <scope> | --expect <contract.toml>) [--top N]

Proves structural movement from an indexed base revision to the indexed working
tree. --expect supplies the base and one or more paths from a v2 pass contract.

  --base <ref>       base revision for an exploratory diff
  --path <scope>     root-relative boundary for an exploratory diff
  --expect <file>    v1 or v2 executable pass contract
  --top N            detail rows shown per section (default 20)";

const SECTIONS: &[&str] = &[
    "expectations",
    "totals",
    "interface",
    "surface",
    "dependencies",
    "files",
    "evidence",
];

#[derive(Debug, PartialEq, Eq)]
struct Args {
    input: Input,
    top: usize,
    output: OutputArgs,
}

#[derive(Debug, PartialEq, Eq)]
enum Input {
    Base { base: String, path: PathBuf },
    Expect(PathBuf),
}

#[derive(Clone, Debug, Serialize)]
struct ValueDelta {
    base: u64,
    current: u64,
    delta: i64,
}

#[derive(Clone, Debug, Serialize)]
struct SurfaceItem {
    scope: PathBuf,
    id: ItemId,
    path: PathBuf,
    line: usize,
}

#[derive(Debug, Serialize)]
struct BoundarySurface {
    scope: PathBuf,
    items: Vec<SurfaceItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct DependencyKey {
    from: String,
    to: String,
    path: PathBuf,
    item: String,
    spelling: Spelling,
}

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
struct InterfaceRow {
    from: String,
    to: String,
    base: usize,
    current: usize,
    base_heaviest: Option<String>,
    current_heaviest: Option<String>,
    moved: bool,
}

#[derive(Clone, Debug)]
struct AssemblyCheck {
    expectation: AssemblyExpectation,
    base: usize,
    current: usize,
}

#[derive(Clone, Debug)]
struct EscCheck {
    expectation: EscExpectation,
    base: usize,
    current: usize,
}

#[derive(Clone, Debug)]
struct DeleteCheck {
    expectation: DeleteExpectation,
    current: Option<DefinitionSite>,
}

#[derive(Clone, Debug)]
struct DefinitionSite {
    path: PathBuf,
    line: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ExpectationRow {
    assertion: String,
    landed: bool,
    detail: String,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
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
    enforcing: bool,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas diff output is the command stdout contract"
)]
pub(super) fn run(root: &Path, raw: &[String]) -> Result<()> {
    let Some(args) = parse_args(raw)? else {
        println!("{USAGE}\n\n{}", output::USAGE);
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
    let (base, exploratory_paths, contract_path) = match &args.input {
        Input::Base { base, path } => (base.clone(), vec![path.clone()], None),
        Input::Expect(path) => (
            super::contract::read_base(path)?,
            Vec::new(),
            Some(path.as_path()),
        ),
    };
    let base_commit = resolve_base(root, &base)?;
    let base_facts = Facts::load_at(root, Path::new("."), &base_commit, true)?;
    let contract = contract_path
        .map(|path| {
            super::contract::load(root, path, &current.syntax.files, &base_facts.syntax.files)
        })
        .transpose()?;
    let paths = contract
        .as_ref()
        .map_or(exploratory_paths, |contract| contract.paths.clone());
    let report = build_report(root, base, paths, contract.as_ref(), &base_facts, &current)?;
    let rendered = if args.output.json {
        render_json(&report, &args.output)?
    } else {
        render_markdown(&report, args.top, &args.output)
    };
    args.output.emit(&rendered)?;

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
    let mut output = OutputArgs::default();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if let Some(eaten) = output.parse_flag(args, index, "diff")? {
            index += eaten;
            continue;
        }
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
            _ => bail!(
                "unknown atlas diff flag `{arg}`\n\n{USAGE}\n\n{}",
                output::USAGE
            ),
        }
    }
    output.validate_sections("diff", SECTIONS)?;
    let input = match (base, path, expect) {
        (Some(base), Some(path), None) => Input::Base { base, path },
        (None, None, Some(expect)) => Input::Expect(expect),
        (None, None, None) => bail!("atlas diff requires --base with --path, or --expect"),
        _ => bail!("atlas diff accepts either --base with --path, or --expect, not both"),
    };
    Ok(Some(Args {
        input,
        top: top.unwrap_or(DEFAULT_TOP),
        output,
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
    let esc_checks = contract.map_or_else(Vec::new, |contract| {
        contract
            .esc
            .iter()
            .map(|expectation| EscCheck {
                expectation: expectation.clone(),
                base: boundary_surface(base, &expectation.path).items.len(),
                current: boundary_surface(current, &expectation.path).items.len(),
            })
            .collect()
    });
    let delete_checks = contract.map_or_else(Vec::new, |contract| {
        contract
            .delete
            .iter()
            .map(|expectation| DeleteCheck {
                expectation: expectation.clone(),
                current: definition_site(&current.syntax.files, &expectation.item),
            })
            .collect()
    });
    let expectations = contract.map_or_else(Vec::new, |contract| {
        expectation_rows(
            contract,
            production.delta,
            &assembly_checks,
            &esc_checks,
            &delete_checks,
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
        enforcing: contract.is_some(),
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
        .map(|scope| boundary_surface(facts, scope))
        .collect()
}

fn boundary_surface(facts: &Facts, scope: &Path) -> BoundarySurface {
    let files = facts
        .syntax
        .files
        .iter()
        .filter(|file| in_boundary(&file.path, scope))
        .collect::<Vec<_>>();
    let entry = if scope.extension().is_some_and(|extension| extension == "rs") {
        scope.to_path_buf()
    } else {
        scope.join("mod.rs")
    };
    let module = crate_module_for_path(&entry);
    let items = escaping_items_for_boundary(&files, &module, &facts.mod_index)
        .into_iter()
        .map(|item| SurfaceItem {
            scope: scope.to_path_buf(),
            id: item.id,
            path: item.path,
            line: item.line,
        })
        .collect();
    BoundarySurface {
        scope: scope.to_path_buf(),
        items,
    }
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
            let base = base_data.map_or(0, EdgeData::assembly);
            let current = current_data.map_or(0, EdgeData::assembly);
            let base_heaviest = base_data.and_then(heaviest_label);
            let current_heaviest = current_data.and_then(heaviest_label);
            let moved = base != current || heaviest_changed(base_data, current_data);
            InterfaceRow {
                from,
                to,
                base,
                current,
                base_heaviest,
                current_heaviest,
                moved,
            }
        })
        .collect()
}

fn heaviest_changed(base: Option<&EdgeData>, current: Option<&EdgeData>) -> bool {
    match (
        base.and_then(EdgeData::heaviest),
        current.and_then(EdgeData::heaviest),
    ) {
        (Some((base, _)), Some((current, _))) => {
            base.path != current.path || base.label != current.label
        }
        (None, None) => false,
        _ => true,
    }
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
            if is_declaration_only(&item.kind) {
                continue;
            }
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

fn definition_site(files: &[super::syntax::FileSyntax], key: &str) -> Option<DefinitionSite> {
    let (module, name) = key.rsplit_once("::")?;
    files.iter().find_map(|file| {
        file.pub_items
            .iter()
            .find(|item| item.module == module && item.name == name)
            .map(|item| DefinitionSite {
                path: file.path.clone(),
                line: item.line,
            })
    })
}

fn expectation_rows(
    contract: &PassContract,
    production_delta: i64,
    assembly: &[AssemblyCheck],
    esc: &[EscCheck],
    delete: &[DeleteCheck],
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
    rows.extend(esc.iter().map(|check| {
        let landed = check.current <= check.expectation.max;
        let excess = check.current.saturating_sub(check.expectation.max);
        ExpectationRow {
            assertion: format!("esc `{}`", check.expectation.path.display()),
            landed,
            detail: format!(
                "base {} → current {} → max {}{}",
                check.base,
                check.current,
                check.expectation.max,
                if landed {
                    String::new()
                } else {
                    format!("; excess {excess}")
                }
            ),
        }
    }));
    rows.extend(delete.iter().map(|check| ExpectationRow {
        assertion: format!("delete `{}`", check.expectation.item),
        landed: check.current.is_none(),
        detail: check.current.as_ref().map_or_else(
            || "deleted".to_owned(),
            |site| format!("still defined at {}:{}", site.path.display(), site.line),
        ),
    }));
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

fn render_markdown(report: &Report, top: usize, output_args: &OutputArgs) -> String {
    let mut output = String::new();
    markdown_line(
        &mut output,
        format_args!("# Atlas diff — {} → working tree", report.base),
    );
    markdown_line(
        &mut output,
        format_args!(
            "\nPaths: {}",
            bounded_names(
                &report
                    .paths
                    .iter()
                    .map(|path| format!("`{}`", path.display()))
                    .collect::<Vec<_>>(),
                report.paths.len(),
            )
        ),
    );
    if output_args.wants("expectations") && !report.expectations.is_empty() {
        markdown_line(&mut output, format_args!("\n## Expectations\n"));
        markdown_line(
            &mut output,
            format_args!("| assertion | status | evidence |"),
        );
        markdown_line(&mut output, format_args!("|---|---|---|"));
        for row in &report.expectations {
            markdown_line(
                &mut output,
                format_args!(
                    "| {} | {} | {} |",
                    row.assertion,
                    if row.landed { "landed" } else { "drifted" },
                    row.detail
                ),
            );
        }
    }

    if output_args.wants("totals") {
        markdown_line(&mut output, format_args!("\n## Totals\n"));
        markdown_line(
            &mut output,
            format_args!("| measure | base | current | delta |"),
        );
        markdown_line(&mut output, format_args!("|---|---:|---:|---:|"));
        write_value_row(&mut output, "production SLOC", &report.production);
        write_value_row(&mut output, "test SLOC", &report.tests);
        for current in &report.current_surface {
            let base = report
                .base_surface
                .iter()
                .find(|base| base.scope == current.scope)
                .map_or(0, |base| base.items.len());
            markdown_line(
                &mut output,
                format_args!(
                    "| esc `{}` | {} | {} | {:+} |",
                    current.scope.display(),
                    base,
                    current.items.len(),
                    current.items.len() as i64 - base as i64
                ),
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
                markdown_line(
                    &mut output,
                    format_args!(
                        "| dependency sites ({direction}) | {base} | {current} | {:+} |",
                        current as i64 - base as i64
                    ),
                );
            }
        }
        markdown_line(
            &mut output,
            format_args!(
                "| Rust files | {} | {} | {:+} |",
                report.rust_files_base,
                report.rust_files_current,
                report.rust_files_current as i64 - report.rust_files_base as i64
            ),
        );
    }

    if output_args.wants("interface") {
        markdown_line(&mut output, format_args!("\n## Call-site interface\n"));
        let moved = moved_interface_rows(&report.interfaces);
        if moved.is_empty() {
            markdown_line(
                &mut output,
                format_args!(
                    "no call-site interface moved ({} pairs measured).",
                    report.interfaces.len()
                ),
            );
        } else {
            markdown_line(
                &mut output,
                format_args!("| caller → provider | max/fn | heaviest function |"),
            );
            markdown_line(&mut output, format_args!("|---|---:|---|"));
            for row in moved.iter().take(top) {
                markdown_line(
                    &mut output,
                    format_args!(
                        "| {} → {} | {} → {} | {} → {} |",
                        row.from,
                        row.to,
                        row.base,
                        row.current,
                        row.base_heaviest.as_deref().unwrap_or("none"),
                        row.current_heaviest.as_deref().unwrap_or("none")
                    ),
                );
            }
            write_omitted(&mut output, moved.len(), top);
            markdown_line(
                &mut output,
                format_args!(
                    "{} caller→provider pairs unchanged.",
                    report.interfaces.len() - moved.len()
                ),
            );
        }
    }

    if output_args.wants("surface") {
        let surface_added = surface_changes(&report.current_surface, &report.base_surface);
        let surface_removed = surface_changes(&report.base_surface, &report.current_surface);
        markdown_line(&mut output, format_args!("\n## Escaping surface\n"));
        markdown_line(
            &mut output,
            format_args!("| movement | boundary | item | site |"),
        );
        markdown_line(&mut output, format_args!("|---|---|---|---|"));
        for (movement, item) in surface_added
            .iter()
            .map(|item| ("added", item))
            .chain(surface_removed.iter().map(|item| ("removed", item)))
            .take(top)
        {
            markdown_line(
                &mut output,
                format_args!(
                    "| {movement} | `{}` | `{}::{}` | `{}:{}` |",
                    item.scope.display(),
                    item.id.module,
                    item.id.name,
                    item.path.display(),
                    item.line
                ),
            );
        }
        write_omitted(
            &mut output,
            surface_added.len() + surface_removed.len(),
            top,
        );
    }

    if output_args.wants("dependencies") {
        markdown_line(&mut output, format_args!("\n## Dependencies\n"));
        markdown_line(
            &mut output,
            format_args!("| movement | direction | module pair | item | site |"),
        );
        markdown_line(&mut output, format_args!("|---|---|---|---|---|"));
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
            markdown_line(
                &mut output,
                format_args!(
                    "| {movement} | {} | {} → {} | `{}{}` | `{}:{}` |",
                    site.direction,
                    site.key.from,
                    site.key.to,
                    site.key.item,
                    spelling_suffix(site.key.spelling),
                    site.key.path.display(),
                    site.line
                ),
            );
        }
        write_omitted(
            &mut output,
            report.dependencies_added.len() + report.dependencies_removed.len(),
            top,
        );
    }

    if output_args.wants("files") {
        markdown_line(&mut output, format_args!("\n## Files\n"));
        markdown_line(&mut output, format_args!("| location | group | path |"));
        markdown_line(&mut output, format_args!("|---|---|---|"));
        write_changed_paths(&mut output, "inside", &report.changed_inside, top);
        write_changed_paths(&mut output, "outside", &report.changed_outside, top);
    }

    if output_args.wants("evidence") {
        markdown_line(&mut output, format_args!("\n## Incomplete evidence\n"));
        if report.evidence.complete() {
            markdown_line(&mut output, format_args!("None."));
        } else {
            for path in &report.evidence.base_parse_failures {
                markdown_line(
                    &mut output,
                    format_args!("- base parse failure: `{}`", path.display()),
                );
            }
            for path in &report.evidence.current_parse_failures {
                markdown_line(
                    &mut output,
                    format_args!("- current parse failure: `{}`", path.display()),
                );
            }
            for item in &report.evidence.newly_unresolved {
                markdown_line(
                    &mut output,
                    format_args!(
                        "- newly unresolved: `{}::{}` at `{}:{}`",
                        item.id.module,
                        item.id.name,
                        item.path.display(),
                        item.line
                    ),
                );
            }
        }
    }

    output
}

fn moved_interface_rows(rows: &[InterfaceRow]) -> Vec<&InterfaceRow> {
    let mut moved = rows.iter().filter(|row| row.moved).collect::<Vec<_>>();
    moved.sort_by(|left, right| {
        right
            .current
            .abs_diff(right.base)
            .cmp(&left.current.abs_diff(left.base))
            .then_with(|| left.from.cmp(&right.from))
            .then_with(|| left.to.cmp(&right.to))
    });
    moved
}

fn render_json(report: &Report, output_args: &OutputArgs) -> Result<String> {
    let mut sections = Map::new();
    if output_args.wants("expectations") {
        sections.insert(
            "expectations".to_owned(),
            serde_json::to_value(&report.expectations)?,
        );
    }
    if output_args.wants("totals") {
        let escaping_surface = report
            .current_surface
            .iter()
            .map(|current| {
                let base = report
                    .base_surface
                    .iter()
                    .find(|base| base.scope == current.scope)
                    .map_or(0, |base| base.items.len());
                json!({
                    "scope": current.scope,
                    "base": base,
                    "current": current.items.len(),
                    "delta": current.items.len() as i64 - base as i64,
                })
            })
            .collect::<Vec<_>>();
        let dependency_sites = ["downward", "same", "upward", "unranked"]
            .into_iter()
            .filter_map(|direction| {
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
                (base > 0 || current > 0).then(|| {
                    (
                        direction.to_owned(),
                        json!({
                            "base": base,
                            "current": current,
                            "delta": current as i64 - base as i64,
                        }),
                    )
                })
            })
            .collect::<Map<_, _>>();
        sections.insert(
            "totals".to_owned(),
            json!({
                "production": &report.production,
                "tests": &report.tests,
                "escaping_surface": escaping_surface,
                "dependency_sites": dependency_sites,
                "rust_files": {
                    "base": report.rust_files_base,
                    "current": report.rust_files_current,
                    "delta": report.rust_files_current as i64 - report.rust_files_base as i64,
                },
            }),
        );
    }
    if output_args.wants("interface") {
        sections.insert(
            "interface".to_owned(),
            serde_json::to_value(&report.interfaces)?,
        );
    }
    if output_args.wants("surface") {
        sections.insert(
            "surface".to_owned(),
            json!({
                "base": &report.base_surface,
                "current": &report.current_surface,
                "added": surface_changes(&report.current_surface, &report.base_surface),
                "removed": surface_changes(&report.base_surface, &report.current_surface),
            }),
        );
    }
    if output_args.wants("dependencies") {
        sections.insert(
            "dependencies".to_owned(),
            json!({
                "base_counts": &report.base_dependency_counts,
                "current_counts": &report.current_dependency_counts,
                "added": &report.dependencies_added,
                "removed": &report.dependencies_removed,
            }),
        );
    }
    if output_args.wants("files") {
        sections.insert(
            "files".to_owned(),
            json!({
                "inside": &report.changed_inside,
                "outside": &report.changed_outside,
                "enforcing": report.enforcing,
            }),
        );
    }
    if output_args.wants("evidence") {
        sections.insert(
            "evidence".to_owned(),
            serde_json::to_value(&report.evidence)?,
        );
    }
    let mut rendered = serde_json::to_string_pretty(&Value::Object(sections))?;
    rendered.push('\n');
    Ok(rendered)
}

fn write_value_row(output: &mut String, name: &str, value: &ValueDelta) {
    markdown_line(
        output,
        format_args!(
            "| {name} | {} | {} | {:+} |",
            value.base, value.current, value.delta
        ),
    );
}

fn write_omitted(output: &mut String, total: usize, top: usize) {
    if total > top {
        markdown_line(
            output,
            format_args!("\n_{} more rows omitted._", total - top),
        );
    }
}

fn write_changed_paths(output: &mut String, location: &str, paths: &[PathBuf], top: usize) {
    for (group, path) in grouped_paths(paths).into_iter().take(top) {
        markdown_line(
            output,
            format_args!("| {location} | `{group}` | `{}` |", path.display()),
        );
    }
    if paths.len() > top {
        markdown_line(
            output,
            format_args!("\n_{} more omitted._", paths.len() - top),
        );
    }
}

fn grouped_paths(paths: &[PathBuf]) -> Vec<(String, &Path)> {
    let mut grouped = paths
        .iter()
        .map(|path| (changed_path_group(path), path.as_path()))
        .collect::<Vec<_>>();
    grouped.sort_by(|(left_group, left_path), (right_group, right_path)| {
        left_group
            .cmp(right_group)
            .then_with(|| left_path.cmp(right_path))
    });
    grouped
}

fn changed_path_group(path: &Path) -> String {
    let components = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(component) => Some(component.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if let [crates, rimz, src, module, ..] = components.as_slice()
        && crates == "crates"
        && rimz == "rimz"
        && src == "src"
    {
        return module.strip_suffix(".rs").unwrap_or(module).to_owned();
    }
    match components.as_slice() {
        [_] => "(root)".to_owned(),
        [first, ..] => format!("{first}/"),
        [] => "(root)".to_owned(),
    }
}

fn markdown_line(output: &mut String, arguments: fmt::Arguments<'_>) {
    // String's `fmt::Write` implementation is infallible.
    output
        .write_fmt(arguments)
        .expect("writing Markdown to a String cannot fail");
    output.push('\n');
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

    use super::super::references::{FnRef, ItemKey, ItemRefs, References};
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
                output: OutputArgs::default(),
            }
        );
    }

    #[test]
    fn diff_args_parse_json_sections_and_reject_unknown_sections() {
        let parsed = parse_args(&[
            "--base".into(),
            "HEAD~1".into(),
            "--path".into(),
            "src".into(),
            "--json".into(),
            "--section".into(),
            "totals,dependencies".into(),
        ])
        .unwrap()
        .unwrap();

        assert!(parsed.output.json);
        assert!(parsed.output.wants("totals"));
        assert!(parsed.output.wants("dependencies"));
        assert!(!parsed.output.wants("interface"));
        assert!(
            parse_args(&[
                "--base".into(),
                "HEAD~1".into(),
                "--path".into(),
                "src".into(),
                "--section".into(),
                "unknown".into(),
            ])
            .is_err()
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
    fn moved_interface_rows_are_filtered_and_sorted_by_delta_then_pair() {
        let rows = [
            interface_row("z", "target", 1, 2, false),
            interface_row("b", "target", 5, 2, false),
            interface_row("a", "target", 1, 1, true),
            interface_row("unchanged", "target", 2, 2, false),
        ];

        let moved = moved_interface_rows(&rows);

        assert_eq!(
            moved
                .iter()
                .map(|row| row.from.as_str())
                .collect::<Vec<_>>(),
            ["b", "z", "a"]
        );
    }

    #[test]
    fn interface_line_shift_does_not_move_the_heaviest_function() {
        let paths = [PathBuf::from("src/a.rs")];
        let base_edges = [reference_edge("src/a.rs", "One", Some(10))];
        let current_edges = [reference_edge("src/a.rs", "One", Some(20))];
        let base = collect_reference_edges(base_edges.iter(), &paths);
        let current = collect_reference_edges(current_edges.iter(), &paths);

        let rows = interface_rows(&base, &current);

        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0].base_heaviest, rows[0].current_heaviest);
        assert!(!rows[0].moved);
    }

    #[test]
    fn diff_expect_requires_call_site_shrink() {
        let contract = contract(-1);
        let checks = [AssemblyCheck {
            expectation: contract.assembly[0].clone(),
            base: 4,
            current: 4,
        }];
        let rows = expectation_rows(&contract, -2, &checks, &[], &[], &[], true);

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

        assert!(!expectation_rows(&contract, -2, &checks, &[], &[], &[], true)[0].landed);
        assert!(expectation_rows(&contract, -3, &checks, &[], &[], &[], true)[0].landed);
    }

    #[test]
    fn diff_expect_rejects_esc_excess() {
        let mut contract = contract(-1);
        contract.esc.push(EscExpectation {
            path: PathBuf::from("src/message"),
            max: 2,
        });
        let checks = [EscCheck {
            expectation: contract.esc[0].clone(),
            base: 1,
            current: 3,
        }];

        let rows = expectation_rows(&contract, -2, &[], &checks, &[], &[], true);
        let row = rows
            .iter()
            .find(|row| row.assertion == "esc `src/message`")
            .unwrap();

        assert!(!row.landed);
        assert_eq!(row.detail, "base 1 → current 3 → max 2; excess 1");
    }

    #[test]
    fn diff_expect_rejects_still_defined_delete_item() {
        let mut contract = contract(-1);
        contract.delete.push(DeleteExpectation {
            item: "message::OLD".to_owned(),
        });
        let checks = [DeleteCheck {
            expectation: contract.delete[0].clone(),
            current: Some(DefinitionSite {
                path: PathBuf::from("src/message.rs"),
                line: 27,
            }),
        }];

        let rows = expectation_rows(&contract, -2, &[], &[], &checks, &[], true);
        let row = rows
            .iter()
            .find(|row| row.assertion == "delete `message::OLD`")
            .unwrap();

        assert!(!row.landed);
        assert_eq!(row.detail, "still defined at src/message.rs:27");
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
        let rows = expectation_rows(&contract, -2, &checks, &[], &[], &outside, true);

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
            defined_names: super::super::facts::defined_names(&syntax),
            unique_fields: super::super::facts::unique_fields(&syntax),
            bin_modules: super::super::facts::bin_modules(&syntax),
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
    fn declaration_only_mod_never_becomes_newly_unresolved() {
        let mut base = facts_for_source("pub mod child;", References::default());
        let key = {
            let file = &base.syntax.files[0];
            let item = file
                .pub_items
                .iter()
                .find(|item| item.kind == "mod")
                .unwrap();
            ItemKey::new(file, item)
        };
        base.references
            .as_mut()
            .unwrap()
            .items
            .insert(key, ItemRefs::default());
        let current = facts_for_source("pub mod child;", References::default());
        let paths = [PathBuf::from("src/lib.rs")];

        assert!(
            boundary_surfaces(&current, &paths)[0]
                .items
                .iter()
                .any(|item| item.id.kind == "mod")
        );
        assert!(
            evidence(&base, &current, &paths)
                .newly_unresolved
                .is_empty()
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

    #[test]
    fn changed_paths_are_grouped_and_bounded_for_markdown() {
        let paths = vec![
            PathBuf::from("README.md"),
            PathBuf::from("docs/guide/start.md"),
            PathBuf::from("crates/rimz/src/message/deliver.rs"),
            PathBuf::from("xtask/src/atlas/diff.rs"),
        ];
        let mut output = String::new();

        write_changed_paths(&mut output, "outside", &paths, 3);

        assert!(output.contains("| outside | `(root)` | `README.md` |"));
        assert!(output.contains("| outside | `docs/` | `docs/guide/start.md` |"));
        assert!(output.contains("| outside | `message` | `crates/rimz/src/message/deliver.rs` |"));
        assert!(!output.contains("xtask/src/atlas/diff.rs"));
        assert!(output.contains("_1 more omitted._"));
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
            esc: Vec::new(),
            delete: Vec::new(),
        }
    }

    fn facts_for_source(source: &str, references: References) -> Facts {
        let sources = vec![Source::new("src/lib.rs", source)];
        let syntax = super::super::syntax::analyze_sources(&sources, &BTreeSet::new());
        Facts {
            root: PathBuf::from("."),
            scope: PathBuf::from("."),
            mod_index: super::super::syntax::ModIndex::new(&syntax.files),
            known_modules: syntax
                .files
                .iter()
                .map(|file| file.module_path.clone())
                .collect(),
            defined_names: super::super::facts::defined_names(&syntax),
            unique_fields: super::super::facts::unique_fields(&syntax),
            bin_modules: super::super::facts::bin_modules(&syntax),
            syntax,
            sources,
            crate_names: BTreeSet::new(),
            sizes: BTreeMap::new(),
            history: None,
            metrics: None,
            references: Some(references),
        }
    }

    fn reference_edge(path: &str, item: &str, function_line: Option<usize>) -> Edge {
        Edge {
            from_path: PathBuf::from(path),
            to_path: PathBuf::from("src/target.rs"),
            from: "caller".to_owned(),
            to: "target".to_owned(),
            to_line: 1,
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

    fn interface_row(
        from: &str,
        to: &str,
        base: usize,
        current: usize,
        heaviest_moved: bool,
    ) -> InterfaceRow {
        InterfaceRow {
            from: from.to_owned(),
            to: to.to_owned(),
            base,
            current,
            base_heaviest: Some("base".to_owned()),
            current_heaviest: Some(if heaviest_moved { "current" } else { "base" }.to_owned()),
            moved: base != current || heaviest_moved,
        }
    }
}
