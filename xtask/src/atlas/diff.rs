use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::conform::{self, Direction};
use super::facts::{Facets, Facts};
use super::index::IndexPolicy;
use super::modules::{
    ItemId, bounded_names, crate_module_for_row, module_endpoint, module_for_path, path_in_scope,
};
use super::references::{Edge, EdgeKind, FunctionId};
use super::target::{self, LayerRanks, TARGET_FILE, Target};
use super::{REPORT_VERSION, positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = ".";
const DEFAULT_TOP: usize = 20;

const USAGE: &str = "cargo xtask atlas diff --base <ref> [--path <prefix>] [--file <target.toml>] [--top N] [--md|--json] [--no-index]

Compares a base commit with the working tree. The report shows structural
movement in escaping surface, layer direction, use/reference edges, stranglers,
and Rust files. --no-index omits reference edges, per-function assembly, and
unresolved items.

  --base <ref>          required base revision
  --path <path>         root-relative subtree (default .)
  --file <path>         target file (default refactor-target.toml)
  --top N               names shown per list (default 20)
  --md                  markdown output
  --json                versioned JSON agent contract (v4)
  --no-index            syntax-only report";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    base: String,
    path: PathBuf,
    file: Option<PathBuf>,
    top: usize,
    markdown: bool,
    json: bool,
    no_index: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ValueDelta {
    base: u64,
    current: u64,
    delta: i64,
}

#[derive(Clone, Debug, Serialize)]
struct Movement {
    added: usize,
    removed: usize,
}

#[derive(Clone, Debug, Serialize)]
struct SurfaceDelta {
    base: usize,
    current: usize,
    added: usize,
    removed: usize,
}

#[derive(Clone, Debug, Serialize)]
struct StranglerSummary {
    rules: usize,
    regressed: usize,
}

#[derive(Clone, Debug, Serialize)]
struct UpwardSummary {
    sites: ValueDelta,
    pairs_opened: usize,
    pairs_closed: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ParseFailures {
    base: Vec<PathBuf>,
    current: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
struct Totals {
    code: ValueDelta,
    tests: ValueDelta,
    escaping: SurfaceDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    upward_imports: Option<UpwardSummary>,
    use_edges: Movement,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference_edges: Option<Movement>,
    files: Movement,
    #[serde(skip_serializing_if = "Option::is_none")]
    newly_unresolved: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stranglers: Option<StranglerSummary>,
}

#[derive(Clone, Debug, Serialize)]
struct ModuleRow {
    module: String,
    code: u64,
    delta_code: i64,
    tests: u64,
    delta_tests: i64,
    escaping: usize,
    escaping_added: usize,
    escaping_removed: usize,
    upward_sites_delta: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    references_added: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    references_removed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assembly_max_delta: Option<i64>,
}

impl ModuleRow {
    fn unchanged(&self) -> bool {
        self.delta_code == 0
            && self.delta_tests == 0
            && self.escaping_added == 0
            && self.escaping_removed == 0
            && self.upward_sites_delta == 0
            && self.references_added.unwrap_or_default() == 0
            && self.references_removed.unwrap_or_default() == 0
            && self.assembly_max_delta.unwrap_or_default() == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Site {
    path: PathBuf,
    line: usize,
}

#[derive(Clone, Debug, Serialize)]
struct SurfaceItem {
    #[serde(flatten)]
    id: ItemId,
    row: String,
    path: PathBuf,
    line: usize,
}

#[derive(Debug)]
struct SurfaceSnapshot {
    items: BTreeMap<ItemId, SurfaceItem>,
    counts: BTreeMap<String, usize>,
    total: usize,
}

struct SizeSnapshots<'a> {
    current: &'a BTreeMap<String, (u64, u64)>,
    base: &'a BTreeMap<String, (u64, u64)>,
}

#[derive(Clone, Debug, Serialize)]
struct ImportChange {
    from: String,
    to: String,
    base: usize,
    current: usize,
    delta: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'static str>,
    sites: Vec<Site>,
    #[serde(skip)]
    row: String,
}

#[derive(Clone, Debug, Serialize)]
struct EdgeChange {
    from: String,
    to: String,
    items: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ReferenceChange {
    from: String,
    to: String,
    items: Vec<String>,
    base_assembly: usize,
    current_assembly: usize,
}

#[derive(Clone, Debug, Serialize)]
struct StranglerChange {
    symbol: String,
    path: PathBuf,
    base: usize,
    current: usize,
    regressed: bool,
}

#[derive(Clone, Debug, Serialize)]
struct Changed<T> {
    added: Vec<T>,
    removed: Vec<T>,
}

#[derive(Clone, Debug, Serialize)]
struct FilesChanged {
    created: Vec<PathBuf>,
    deleted: Vec<PathBuf>,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    base: String,
    base_commit: String,
    path: PathBuf,
    no_index: bool,
    parse_failures: ParseFailures,
    totals: Totals,
    modules: Vec<ModuleRow>,
    modules_unchanged: usize,
    escaping: Changed<SurfaceItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upward: Option<Vec<ImportChange>>,
    use_edges: Changed<EdgeChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference_edges: Option<Changed<ReferenceChange>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stranglers: Option<Vec<StranglerChange>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unresolved_added: Option<Vec<SurfaceItem>>,
    files: FilesChanged,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas diff output is a command stdout contract"
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
            serde_json::to_string_pretty(&report).context("rendering atlas diff JSON")?
        );
    } else {
        print_report(&report, args.top, args.markdown);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut base = None;
    let mut path = None;
    let mut file = None;
    let mut top = None;
    let mut markdown = false;
    let mut json = false;
    let mut no_index = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--base" => {
                let parsed = value(args, index, "diff", "--base")?.to_owned();
                if parsed.is_empty() {
                    bail!("atlas diff --base requires a non-empty revision");
                }
                set_once(&mut base, parsed, "diff", "--base")?;
                index += 2;
            }
            "--path" => {
                let parsed = validate_scope(value(args, index, "diff", "--path")?, "--path")?;
                set_once(&mut path, parsed, "diff", "--path")?;
                index += 2;
            }
            "--file" => {
                let parsed = value(args, index, "diff", "--file")?;
                if parsed.is_empty() {
                    bail!("atlas diff --file requires a non-empty path");
                }
                set_once(&mut file, PathBuf::from(parsed), "diff", "--file")?;
                index += 2;
            }
            "--top" => {
                let parsed = positive_usize(value(args, index, "diff", "--top")?, "diff", "--top")?;
                set_once(&mut top, parsed, "diff", "--top")?;
                index += 2;
            }
            "--md" if !markdown => {
                markdown = true;
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
            "--md" | "--json" | "--no-index" => {
                bail!("atlas diff {arg} may only be passed once")
            }
            _ => bail!("unknown atlas diff flag `{arg}`\n\n{USAGE}"),
        }
    }
    if markdown && json {
        bail!("atlas diff --md and --json are mutually exclusive");
    }
    let base = base.context("atlas diff requires --base <ref>")?;
    Ok(Some(Args {
        base,
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        file,
        top: top.unwrap_or(DEFAULT_TOP),
        markdown,
        json,
        no_index,
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

fn build_report(root: &Path, args: &Args) -> Result<Report> {
    let base_commit = resolve_base(root, &args.base)?;
    let references = (!args.no_index).then_some(IndexPolicy::Required);
    let base = Facts::load_at(root, Path::new("."), &base_commit, references)?;
    let current = Facts::load(
        root,
        Path::new("."),
        Facets {
            references,
            ..Facets::default()
        },
    )?;
    if !base
        .sources
        .iter()
        .chain(&current.sources)
        .any(|source| path_in_scope(&source.path, &args.path))
    {
        bail!(
            "no Rust files under `{}` at the base or in the working tree",
            args.path.display()
        );
    }
    let target_path = args
        .file
        .clone()
        .unwrap_or_else(|| PathBuf::from(TARGET_FILE));
    let target = target::load(&root.join(&target_path))?;
    if target.is_none() && args.file.is_some() {
        bail!(
            "atlas diff target file `{}` does not exist",
            target_path.display()
        );
    }

    let current_sizes = sizes(&current, &args.path);
    let base_sizes = sizes(&base, &args.path);
    let current_surface = surface_items(&current, &args.path);
    let base_surface = surface_items(&base, &args.path);
    let escaping_added = values_for_difference(&current_surface.items, &base_surface.items);
    let escaping_removed = values_for_difference(&base_surface.items, &current_surface.items);

    let (upward, upward_sites) = if let Some(target) = &target {
        let ranks = target.layer_ranks();
        let current_edges = upward_edges(&current, &args.path, &ranks);
        let base_edges = upward_edges(&base, &args.path, &ranks);
        (
            Some(import_changes(&base_edges, &current_edges, &args.path)),
            Some((
                base_edges.values().map(BTreeSet::len).sum::<usize>(),
                current_edges.values().map(BTreeSet::len).sum::<usize>(),
            )),
        )
    } else {
        (None, None)
    };

    let current_use = use_edges(&current, &args.path);
    let base_use = use_edges(&base, &args.path);
    let use_added = edge_changes(&current_use, &base_use);
    let use_removed = edge_changes(&base_use, &current_use);

    let (reference_edges, reference_added, reference_removed, assembly) = if args.no_index {
        (None, 0, 0, BTreeMap::new())
    } else {
        let current_refs = reference_edges(&current, &args.path);
        let base_refs = reference_edges(&base, &args.path);
        let added = reference_changes(&current_refs, &base_refs, &base_refs, &current_refs);
        let removed = reference_changes(&base_refs, &current_refs, &base_refs, &current_refs);
        let added_count = added.len();
        let removed_count = removed.len();
        let assembly = assembly_deltas(&base_refs, &current_refs);
        (
            Some(Changed { added, removed }),
            added_count,
            removed_count,
            assembly,
        )
    };

    let current_files = rust_files(&current, &args.path);
    let base_files = rust_files(&base, &args.path);
    let created = current_files
        .difference(&base_files)
        .cloned()
        .collect::<Vec<_>>();
    let deleted = base_files
        .difference(&current_files)
        .cloned()
        .collect::<Vec<_>>();

    let unresolved_added = if args.no_index {
        None
    } else {
        Some(newly_unresolved(
            &base,
            &current,
            &base_surface.items,
            &current_surface.items,
        ))
    };
    let stranglers = target
        .as_ref()
        .map(|target| strangler_changes(target, &base, &current));

    let modules = module_rows(
        SizeSnapshots {
            current: &current_sizes,
            base: &base_sizes,
        },
        &current_surface,
        &base_surface,
        upward.as_ref(),
        reference_edges.as_ref(),
        &assembly,
        &endpoint_rows(&base, &current, &args.path),
    );
    let modules_unchanged = modules.iter().filter(|row| row.unchanged()).count();
    let mut modules = modules
        .into_iter()
        .filter(|row| !row.unchanged())
        .collect::<Vec<_>>();
    modules.sort_by(|left, right| {
        let left_escape = left.escaping_added + left.escaping_removed;
        let right_escape = right.escaping_added + right.escaping_removed;
        right_escape
            .cmp(&left_escape)
            .then_with(|| {
                right
                    .delta_code
                    .unsigned_abs()
                    .cmp(&left.delta_code.unsigned_abs())
            })
            .then_with(|| left.module.cmp(&right.module))
    });

    let code = value_delta(
        total_size(&base_sizes, false),
        total_size(&current_sizes, false),
    );
    let tests = value_delta(
        total_size(&base_sizes, true),
        total_size(&current_sizes, true),
    );
    let strangler_summary = stranglers.as_ref().map(|rows| StranglerSummary {
        rules: rows.len(),
        regressed: rows.iter().filter(|row| row.regressed).count(),
    });
    let totals = Totals {
        code,
        tests,
        escaping: SurfaceDelta {
            base: base_surface.total,
            current: current_surface.total,
            added: escaping_added.len(),
            removed: escaping_removed.len(),
        },
        upward_imports: upward.as_ref().map(|changes| {
            let (base_sites, current_sites) = upward_sites.unwrap_or_default();
            UpwardSummary {
                sites: value_delta(base_sites as u64, current_sites as u64),
                pairs_opened: changes
                    .iter()
                    .filter(|change| change.status == Some("opened"))
                    .count(),
                pairs_closed: changes
                    .iter()
                    .filter(|change| change.status == Some("closed"))
                    .count(),
            }
        }),
        use_edges: Movement {
            added: use_added.len(),
            removed: use_removed.len(),
        },
        reference_edges: (!args.no_index).then_some(Movement {
            added: reference_added,
            removed: reference_removed,
        }),
        files: Movement {
            added: created.len(),
            removed: deleted.len(),
        },
        newly_unresolved: unresolved_added.as_ref().map(Vec::len),
        stranglers: strangler_summary,
    };

    Ok(Report {
        version: REPORT_VERSION,
        verb: "diff",
        base: args.base.clone(),
        base_commit,
        path: args.path.clone(),
        no_index: args.no_index,
        parse_failures: ParseFailures {
            base: base
                .syntax
                .parse_failures
                .iter()
                .filter(|path| path_in_scope(path, &args.path))
                .cloned()
                .collect(),
            current: current
                .syntax
                .parse_failures
                .iter()
                .filter(|path| path_in_scope(path, &args.path))
                .cloned()
                .collect(),
        },
        totals,
        modules,
        modules_unchanged,
        escaping: Changed {
            added: escaping_added,
            removed: escaping_removed,
        },
        upward,
        use_edges: Changed {
            added: use_added,
            removed: use_removed,
        },
        reference_edges,
        stranglers,
        unresolved_added,
        files: FilesChanged { created, deleted },
    })
}

fn sizes(facts: &Facts, scope: &Path) -> BTreeMap<String, (u64, u64)> {
    let mut sizes = BTreeMap::<String, (u64, u64)>::new();
    for (path, size) in &facts.sizes {
        if !path_in_scope(path, scope) {
            continue;
        }
        let row = sizes.entry(module_for_path(path, scope)).or_default();
        row.0 += size.code;
        row.1 += size.tests;
    }
    sizes
}

fn total_size(sizes: &BTreeMap<String, (u64, u64)>, tests: bool) -> u64 {
    sizes
        .values()
        .map(|size| if tests { size.1 } else { size.0 })
        .sum()
}

fn value_delta(base: u64, current: u64) -> ValueDelta {
    ValueDelta {
        base,
        current,
        delta: current as i64 - base as i64,
    }
}

fn surface_items(facts: &Facts, scope: &Path) -> SurfaceSnapshot {
    let files = facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
        .collect::<Vec<_>>();
    let escaping = super::modules::escaping_items(&files, scope, &facts.mod_index);
    let counts = escaping
        .iter()
        .map(|(row, items)| (row.clone(), items.len()))
        .collect::<BTreeMap<_, _>>();
    let total = counts.values().sum();
    let mut items = BTreeMap::new();
    for (row, located) in escaping {
        for item in located {
            items.entry(item.id.clone()).or_insert_with(|| SurfaceItem {
                id: item.id,
                row: row.clone(),
                path: item.path,
                line: item.line,
            });
        }
    }
    SurfaceSnapshot {
        items,
        counts,
        total,
    }
}

fn values_for_difference(
    left: &BTreeMap<ItemId, SurfaceItem>,
    right: &BTreeMap<ItemId, SurfaceItem>,
) -> Vec<SurfaceItem> {
    left.iter()
        .filter(|(id, _)| !right.contains_key(*id))
        .map(|(_, item)| item.clone())
        .collect()
}

fn upward_edges(
    facts: &Facts,
    scope: &Path,
    ranks: &LayerRanks,
) -> BTreeMap<(String, String), BTreeSet<Site>> {
    let mut edges = BTreeMap::<(String, String), BTreeSet<Site>>::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
    {
        for import in &file.dependencies {
            let Some(resolved) = super::syntax::resolved_internal_import(
                import,
                &facts.known_modules,
                &facts.crate_names,
            ) else {
                continue;
            };
            if conform::layer_direction(ranks, &file.module_path, &resolved)
                != Some(Direction::Upward)
            {
                continue;
            }
            let from = conform::top_module(&file.module_path).to_owned();
            let to = conform::top_module(&resolved).to_owned();
            edges.entry((from, to)).or_default().insert(Site {
                path: file.path.clone(),
                line: import.line,
            });
        }
    }
    edges
}

fn import_changes(
    base: &BTreeMap<(String, String), BTreeSet<Site>>,
    current: &BTreeMap<(String, String), BTreeSet<Site>>,
    scope: &Path,
) -> Vec<ImportChange> {
    base.keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|(from, to)| {
            let base_sites = base.get(&(from.clone(), to.clone()));
            let current_sites = current.get(&(from.clone(), to.clone()));
            let base_count = base_sites.map_or(0, BTreeSet::len);
            let current_count = current_sites.map_or(0, BTreeSet::len);
            if base_count == current_count {
                return None;
            }
            let sites = current_sites
                .filter(|sites| !sites.is_empty())
                .or(base_sites)
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            let row = sites.first().map_or_else(
                || "(root)".to_owned(),
                |site| module_for_path(&site.path, scope),
            );
            Some(ImportChange {
                from: from.clone(),
                to: to.clone(),
                base: base_count,
                current: current_count,
                delta: current_count as i64 - base_count as i64,
                status: if base_count == 0 {
                    Some("opened")
                } else if current_count == 0 {
                    Some("closed")
                } else {
                    None
                },
                sites,
                row,
            })
        })
        .collect()
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
}

type EdgeMap = BTreeMap<(String, String), EdgeData>;

fn use_edges(facts: &Facts, scope: &Path) -> EdgeMap {
    let scope_module = crate_module_for_row(scope, "(root)");
    let mut edges = EdgeMap::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
    {
        let from = module_endpoint(&file.module_path, &scope_module);
        for import in &file.dependencies {
            let Some(resolved) = super::syntax::resolved_internal_import(
                import,
                &facts.known_modules,
                &facts.crate_names,
            ) else {
                continue;
            };
            let to = module_endpoint(&resolved, &scope_module);
            if from != to {
                edges
                    .entry((from.clone(), to))
                    .or_default()
                    .items
                    .insert(import.item.clone());
            }
        }
    }
    edges
}

fn reference_edges(facts: &Facts, scope: &Path) -> EdgeMap {
    let scope_module = crate_module_for_row(scope, "(root)");
    collect_reference_edges(
        facts
            .references
            .as_ref()
            .into_iter()
            .flat_map(|references| &references.edges),
        scope,
        &scope_module,
    )
}

fn collect_reference_edges<'a>(
    source: impl Iterator<Item = &'a Edge>,
    scope: &Path,
    scope_module: &str,
) -> EdgeMap {
    source
        .filter(|edge| {
            edge.kind == EdgeKind::Reference && !edge.test && path_in_scope(&edge.from_path, scope)
        })
        .fold(EdgeMap::new(), |mut edges, edge| {
            let from = module_endpoint(&edge.from, scope_module);
            let to = module_endpoint(&edge.to, scope_module);
            if from == to {
                return edges;
            }
            let data = edges.entry((from, to)).or_default();
            data.items.insert(edge.item.clone());
            if let Some(function) = &edge.from_fn {
                data.by_fn
                    .entry(FunctionId::new(&edge.from_path, function))
                    .or_default()
                    .insert(edge.item.clone());
            }
            edges
        })
}

fn edge_changes(left: &EdgeMap, right: &EdgeMap) -> Vec<EdgeChange> {
    left.iter()
        .filter_map(|((from, to), data)| {
            let other = right
                .get(&(from.clone(), to.clone()))
                .map(|data| &data.items);
            let changed = data
                .items
                .iter()
                .filter(|item| other.is_none_or(|other| !other.contains(*item)))
                .cloned()
                .collect::<Vec<_>>();
            (!changed.is_empty()).then(|| EdgeChange {
                from: from.clone(),
                to: to.clone(),
                items: changed,
            })
        })
        .collect()
}

fn reference_changes(
    left: &EdgeMap,
    right: &EdgeMap,
    base: &EdgeMap,
    current: &EdgeMap,
) -> Vec<ReferenceChange> {
    edge_changes(left, right)
        .into_iter()
        .map(|change| {
            let key = (change.from.clone(), change.to.clone());
            ReferenceChange {
                base_assembly: base.get(&key).map_or(0, EdgeData::assembly),
                current_assembly: current.get(&key).map_or(0, EdgeData::assembly),
                from: change.from,
                to: change.to,
                items: change.items,
            }
        })
        .collect()
}

fn assembly_deltas(base: &EdgeMap, current: &EdgeMap) -> BTreeMap<String, i64> {
    let pairs = base
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut deltas = BTreeMap::<String, i64>::new();
    for pair in pairs {
        let delta = current.get(&pair).map_or(0, EdgeData::assembly) as i64
            - base.get(&pair).map_or(0, EdgeData::assembly) as i64;
        let entry = deltas.entry(pair.0).or_default();
        if delta.unsigned_abs() > entry.unsigned_abs() {
            *entry = delta;
        }
    }
    deltas
}

fn rust_files(facts: &Facts, scope: &Path) -> BTreeSet<PathBuf> {
    facts
        .sources
        .iter()
        .filter(|source| path_in_scope(&source.path, scope))
        .map(|source| source.path.clone())
        .collect()
}

fn newly_unresolved(
    base: &Facts,
    current: &Facts,
    base_surface: &BTreeMap<ItemId, SurfaceItem>,
    current_surface: &BTreeMap<ItemId, SurfaceItem>,
) -> Vec<SurfaceItem> {
    let base_resolution = resolution(base);
    let current_resolution = resolution(current);
    base_surface
        .keys()
        .filter(|id| current_surface.contains_key(*id))
        .filter(|id| base_resolution.get(*id) == Some(&true))
        .filter(|id| current_resolution.get(*id) == Some(&false))
        .filter_map(|id| current_surface.get(id).cloned())
        .collect()
}

fn resolution(facts: &Facts) -> BTreeMap<ItemId, bool> {
    let Some(references) = &facts.references else {
        return BTreeMap::new();
    };
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

fn strangler_changes(target: &Target, base: &Facts, current: &Facts) -> Vec<StranglerChange> {
    target
        .strangler
        .iter()
        .map(|rule| {
            let base_sources = conform::sources_for_path(
                &base.sources,
                &rule.path,
                base.sources.iter().any(|source| source.path == rule.path),
            );
            let current_sources = conform::sources_for_path(
                &current.sources,
                &rule.path,
                current
                    .sources
                    .iter()
                    .any(|source| source.path == rule.path),
            );
            let base_count =
                conform::count_in_sources(&base_sources, &base.syntax.files, &rule.symbol);
            let current_count =
                conform::count_in_sources(&current_sources, &current.syntax.files, &rule.symbol);
            StranglerChange {
                symbol: rule.symbol.clone(),
                path: rule.path.clone(),
                base: base_count,
                current: current_count,
                regressed: current_count > rule.baseline,
            }
        })
        .collect()
}

fn endpoint_rows(base: &Facts, current: &Facts, scope: &Path) -> BTreeMap<String, String> {
    let scope_module = crate_module_for_row(scope, "(root)");
    base.syntax
        .files
        .iter()
        .chain(&current.syntax.files)
        .filter(|file| path_in_scope(&file.path, scope))
        .map(|file| {
            (
                module_endpoint(&file.module_path, &scope_module),
                module_for_path(&file.path, scope),
            )
        })
        .collect()
}

fn module_rows(
    sizes: SizeSnapshots<'_>,
    current_surface: &SurfaceSnapshot,
    base_surface: &SurfaceSnapshot,
    upward: Option<&Vec<ImportChange>>,
    references: Option<&Changed<ReferenceChange>>,
    assembly: &BTreeMap<String, i64>,
    endpoint_rows: &BTreeMap<String, String>,
) -> Vec<ModuleRow> {
    let modules = sizes
        .current
        .keys()
        .chain(sizes.base.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    modules
        .into_iter()
        .map(|module| {
            let current_size = sizes.current.get(&module).copied().unwrap_or_default();
            let base_size = sizes.base.get(&module).copied().unwrap_or_default();
            let current_ids = current_surface
                .items
                .iter()
                .filter(|(_, item)| item.row == module)
                .map(|(id, _)| id)
                .collect::<BTreeSet<_>>();
            let base_ids = base_surface
                .items
                .iter()
                .filter(|(_, item)| item.row == module)
                .map(|(id, _)| id)
                .collect::<BTreeSet<_>>();
            let upward_sites_delta = upward.map_or(0, |changes| {
                changes
                    .iter()
                    .filter(|change| change.row == module)
                    .map(|change| change.delta)
                    .sum()
            });
            let references_added = references.map(|changed| {
                changed
                    .added
                    .iter()
                    .filter(|edge| endpoint_rows.get(&edge.from) == Some(&module))
                    .count()
            });
            let references_removed = references.map(|changed| {
                changed
                    .removed
                    .iter()
                    .filter(|edge| endpoint_rows.get(&edge.from) == Some(&module))
                    .count()
            });
            ModuleRow {
                module: module.clone(),
                code: current_size.0,
                delta_code: current_size.0 as i64 - base_size.0 as i64,
                tests: current_size.1,
                delta_tests: current_size.1 as i64 - base_size.1 as i64,
                escaping: current_surface
                    .counts
                    .get(&module)
                    .copied()
                    .unwrap_or_default(),
                escaping_added: current_ids.difference(&base_ids).count(),
                escaping_removed: base_ids.difference(&current_ids).count(),
                upward_sites_delta,
                references_added,
                references_removed,
                assembly_max_delta: references.is_some().then(|| {
                    assembly
                        .iter()
                        .filter(|(endpoint, _)| endpoint_rows.get(*endpoint) == Some(&module))
                        .map(|(_, delta)| *delta)
                        .max_by_key(|delta| delta.unsigned_abs())
                        .unwrap_or_default()
                }),
            }
        })
        .collect()
}

#[expect(clippy::print_stdout, reason = "atlas diff report helper")]
fn print_report(report: &Report, top: usize, markdown: bool) {
    let title = if markdown { "# " } else { "" };
    println!(
        "{title}Atlas diff — {} → working tree ({})",
        report.base,
        report.path.display()
    );
    section("Totals", markdown);
    println!(
        "code              {:>8} → {:>8} ({:+})",
        report.totals.code.base, report.totals.code.current, report.totals.code.delta
    );
    println!(
        "tests             {:>8} → {:>8} ({:+})",
        report.totals.tests.base, report.totals.tests.current, report.totals.tests.delta
    );
    println!(
        "esc               {:>8} → {:>8} (+{} -{})",
        report.totals.escaping.base,
        report.totals.escaping.current,
        report.totals.escaping.added,
        report.totals.escaping.removed
    );
    if let Some(upward) = &report.totals.upward_imports {
        println!(
            "upward dependencies {:>7} → {:>8} ({:+}); pairs +{} -{}",
            upward.sites.base,
            upward.sites.current,
            upward.sites.delta,
            upward.pairs_opened,
            upward.pairs_closed
        );
    } else {
        println!("upward dependencies     no target configured");
    }
    println!(
        "dependency sites        +{} -{}",
        report.totals.use_edges.added, report.totals.use_edges.removed
    );
    if let Some(references) = &report.totals.reference_edges {
        println!(
            "reference edges         +{} -{}",
            references.added, references.removed
        );
    }
    println!(
        "files                   +{} -{}",
        report.totals.files.added, report.totals.files.removed
    );
    if let Some(unresolved) = report.totals.newly_unresolved {
        println!("unresolved              +{unresolved}");
    }
    if let Some(stranglers) = &report.totals.stranglers {
        println!(
            "stranglers              {} rules, {} regressed",
            stranglers.rules, stranglers.regressed
        );
    }
    println!(
        "parse failures           base {} current {}",
        report.parse_failures.base.len(),
        report.parse_failures.current.len()
    );
    for path in &report.parse_failures.base {
        println!("base parse failure: {}", path.display());
    }
    for path in &report.parse_failures.current {
        println!("current parse failure: {}", path.display());
    }
    close(markdown);

    section("Modules", markdown);
    println!(
        "{:<24} {:>7} {:>7} {:>7} {:>7} {:>5} {:>9} {:>8} {:>9} {:>8}",
        "module", "code", "Δcode", "tests", "Δtests", "esc", "Δesc", "up Δ", "refs", "asm Δ"
    );
    for row in report.modules.iter().take(top) {
        let refs = row
            .references_added
            .zip(row.references_removed)
            .map_or_else(
                || "-".to_owned(),
                |(added, removed)| format!("+{added}/-{removed}"),
            );
        let assembly = row
            .assembly_max_delta
            .map_or_else(|| "-".to_owned(), |delta| format!("{delta:+}"));
        println!(
            "{:<24} {:>7} {:+7} {:>7} {:+7} {:>5} {:>4}/-{:<3} {:+8} {:>9} {:>8}",
            row.module,
            row.code,
            row.delta_code,
            row.tests,
            row.delta_tests,
            row.escaping,
            row.escaping_added,
            row.escaping_removed,
            row.upward_sites_delta,
            refs,
            assembly
        );
    }
    if report.modules.len() > top {
        println!("… {} more changed modules", report.modules.len() - top);
    }
    if report.modules_unchanged > 0 {
        println!("{} modules unchanged", report.modules_unchanged);
    }
    close(markdown);

    print_surface(
        "Escaping items added",
        &report.escaping.added,
        top,
        markdown,
    );
    print_surface(
        "Escaping items removed",
        &report.escaping.removed,
        top,
        markdown,
    );
    if let Some(upward) = &report.upward {
        print_imports("Upward dependencies", upward, top, markdown);
    }
    print_edges(
        "Dependency sites added",
        &report.use_edges.added,
        top,
        markdown,
    );
    print_edges(
        "Dependency sites removed",
        &report.use_edges.removed,
        top,
        markdown,
    );
    if let Some(references) = &report.reference_edges {
        print_references("Reference edges added", &references.added, top, markdown);
        print_references(
            "Reference edges removed",
            &references.removed,
            top,
            markdown,
        );
    }
    if let Some(stranglers) = &report.stranglers {
        section("Stranglers", markdown);
        for row in stranglers.iter().take(top) {
            println!(
                "{} {}: {} → {}{}",
                row.symbol,
                row.path.display(),
                row.base,
                row.current,
                if row.regressed { " regressed" } else { "" }
            );
        }
        close(markdown);
    }
    if let Some(unresolved) = &report.unresolved_added {
        print_surface("Newly unresolved", unresolved, top, markdown);
    }
    section(
        &format!(
            "Files (+{} -{})",
            report.files.created.len(),
            report.files.deleted.len()
        ),
        markdown,
    );
    for path in report.files.created.iter().take(top) {
        println!("+{}", path.display());
    }
    for path in report.files.deleted.iter().take(top) {
        println!("-{}", path.display());
    }
    close(markdown);
}

#[expect(clippy::print_stdout, reason = "atlas diff report helper")]
fn section(name: &str, markdown: bool) {
    println!("\n{}{name}", if markdown { "## " } else { "" });
    if markdown {
        println!("```");
    }
}

#[expect(clippy::print_stdout, reason = "atlas diff report helper")]
fn close(markdown: bool) {
    if markdown {
        println!("```");
    }
}

#[expect(clippy::print_stdout, reason = "atlas diff report helper")]
fn print_surface(name: &str, items: &[SurfaceItem], top: usize, markdown: bool) {
    section(&format!("{name} ({})", items.len()), markdown);
    for item in items.iter().take(top) {
        println!(
            "{}::{} ({}) {}:{}",
            item.id.module,
            item.id.name,
            item.id.kind,
            item.path.display(),
            item.line
        );
    }
    close(markdown);
}

#[expect(clippy::print_stdout, reason = "atlas diff report helper")]
fn print_imports(name: &str, edges: &[ImportChange], top: usize, markdown: bool) {
    section(&format!("{name} ({})", edges.len()), markdown);
    for edge in edges.iter().take(top) {
        let sites = edge
            .sites
            .iter()
            .take(5)
            .map(|site| format!("{}:{}", site.path.display(), site.line))
            .collect::<Vec<_>>()
            .join(", ");
        let omitted = edge.sites.len().saturating_sub(5);
        let omitted = if omitted == 0 {
            String::new()
        } else {
            format!(" … {omitted} more")
        };
        let status = edge
            .status
            .map_or(String::new(), |status| format!(" {status}"));
        println!(
            "{} → {}: sites {} → {} ({:+}){}  {sites}{omitted}",
            edge.from, edge.to, edge.base, edge.current, edge.delta, status
        );
    }
    close(markdown);
}

#[expect(clippy::print_stdout, reason = "atlas diff report helper")]
fn print_edges(name: &str, edges: &[EdgeChange], top: usize, markdown: bool) {
    section(&format!("{name} ({})", edges.len()), markdown);
    for edge in edges.iter().take(top) {
        println!(
            "{} → {}  {}",
            edge.from,
            edge.to,
            bounded_names(&edge.items, top)
        );
    }
    close(markdown);
}

#[expect(clippy::print_stdout, reason = "atlas diff report helper")]
fn print_references(name: &str, edges: &[ReferenceChange], top: usize, markdown: bool) {
    section(&format!("{name} ({})", edges.len()), markdown);
    for edge in edges.iter().take(top) {
        println!(
            "{} → {}  assembly {} → {} max/fn  {}",
            edge.from,
            edge.to,
            edge.base_assembly,
            edge.current_assembly,
            bounded_names(&edge.items, top)
        );
    }
    close(markdown);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::super::references::FnRef;
    use super::*;

    #[test]
    fn diff_args_require_base_and_reject_repeated_flags() {
        assert!(parse_args(&[]).is_err());
        assert!(
            parse_args(&[
                "--base".into(),
                "HEAD".into(),
                "--base".into(),
                "main".into()
            ])
            .is_err()
        );
        assert!(
            parse_args(&[
                "--base".into(),
                "HEAD".into(),
                "--md".into(),
                "--json".into()
            ])
            .is_err()
        );
        assert_eq!(
            parse_args(&["--base".into(), "HEAD~1".into()])
                .unwrap()
                .unwrap(),
            Args {
                base: "HEAD~1".to_owned(),
                path: PathBuf::from("."),
                file: None,
                top: 20,
                markdown: false,
                json: false,
                no_index: false,
            }
        );
    }

    #[test]
    fn assembly_delta_uses_the_per_function_maximum() {
        let base_edges = [
            reference_edge("src/a.rs", "One", Some(10)),
            reference_edge("src/a.rs", "Two", Some(10)),
            reference_edge("src/a.rs", "Three", Some(30)),
            reference_edge("src/b.rs", "Four", Some(10)),
            reference_edge("src/a.rs", "Outside", None),
        ];
        let mut current_edges = vec![
            reference_edge("src/a.rs", "One", Some(10)),
            reference_edge("src/a.rs", "Two", Some(10)),
            reference_edge("src/a.rs", "Three", Some(10)),
            reference_edge("src/a.rs", "Four", Some(10)),
            reference_edge("src/a.rs", "Outside", None),
        ];
        let mut test_edge = reference_edge("src/a.rs", "TestOnly", Some(10));
        test_edge.test = true;
        current_edges.push(test_edge);
        let base = collect_reference_edges(base_edges.iter(), Path::new("."), "");
        let current = collect_reference_edges(current_edges.iter(), Path::new("."), "");
        let pair = ("caller".to_owned(), "target".to_owned());

        assert_eq!(base[&pair].items, current[&pair].items);
        assert_eq!(base[&pair].assembly(), 2);
        assert_eq!(current[&pair].assembly(), 4);
        let delta = assembly_deltas(&base, &current)["caller"];
        assert_eq!(delta, 2);
        assert!(
            !ModuleRow {
                module: "caller".to_owned(),
                code: 0,
                delta_code: 0,
                tests: 0,
                delta_tests: 0,
                escaping: 0,
                escaping_added: 0,
                escaping_removed: 0,
                upward_sites_delta: 0,
                references_added: Some(0),
                references_removed: Some(0),
                assembly_max_delta: Some(delta),
            }
            .unchanged()
        );
    }

    #[test]
    fn diff_reports_escaping_upward_strangler_and_file_movement_without_the_index() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "atlas@example.test"]);
        run_git(root, &["config", "user.name", "Atlas Test"]);
        fs::create_dir(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"atlas-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(root.join("src/lib.rs"), "pub mod cli;\npub mod store;\n").unwrap();
        fs::write(
            root.join("src/cli.rs"),
            "pub struct Helper;\npub struct Other;\n",
        )
        .unwrap();
        fs::write(
            root.join("src/store.rs"),
            "use crate::cli::Helper;\nuse crate::cli::Other;\npub fn a() {}\npub fn b() { let _ = (Helper, Other); }\n",
        )
        .unwrap();
        fs::write(
            root.join("refactor-target.toml"),
            "version = 5\nlayers = [[\"store\"], [\"cli\"]]\n[[strangler]]\nsymbol = \"LegacyToken\"\npath = \"src\"\nbaseline = 0\n",
        )
        .unwrap();
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "base"]);

        fs::write(
            root.join("src/store.rs"),
            "use crate::cli::Other;\npub fn a() {}\npub fn c() { let _ = Other; }\nfn consume() { let _ = \"LegacyToken\"; }\n",
        )
        .unwrap();
        fs::write(root.join("src/new.rs"), "fn new_detail() {}\n").unwrap();

        let report = build_report(
            root,
            &Args {
                base: "HEAD".to_owned(),
                path: PathBuf::from("src"),
                file: None,
                top: 20,
                markdown: false,
                json: false,
                no_index: true,
            },
        )
        .unwrap();

        assert_eq!(
            report
                .escaping
                .added
                .iter()
                .map(|item| item.id.name.as_str())
                .collect::<Vec<_>>(),
            ["c"]
        );
        assert_eq!(
            report
                .escaping
                .removed
                .iter()
                .map(|item| item.id.name.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
        assert_eq!(report.files.created, [PathBuf::from("src/new.rs")]);
        assert_eq!(report.upward.as_ref().unwrap().len(), 1);
        assert_eq!(report.upward.as_ref().unwrap()[0].base, 2);
        assert_eq!(report.upward.as_ref().unwrap()[0].current, 1);
        assert_eq!(report.upward.as_ref().unwrap()[0].delta, -1);
        assert_eq!(report.upward.as_ref().unwrap()[0].status, None);
        assert_eq!(report.stranglers.as_ref().unwrap()[0].base, 0);
        assert_eq!(report.stranglers.as_ref().unwrap()[0].current, 1);
        assert!(report.totals.code.delta > 0);

        let created_scope = build_report(
            root,
            &Args {
                path: PathBuf::from("src/new.rs"),
                ..args_for_test()
            },
        )
        .unwrap();
        assert_eq!(created_scope.files.created, [PathBuf::from("src/new.rs")]);

        let missing_target = build_report(
            root,
            &Args {
                file: Some(PathBuf::from("missing-target.toml")),
                ..args_for_test()
            },
        )
        .unwrap_err();
        assert!(
            missing_target
                .to_string()
                .contains("target file `missing-target.toml` does not exist")
        );

        fs::write(
            root.join("src/store.rs"),
            "pub fn a() {}\npub fn c() {}\nfn consume() { let _ = \"LegacyToken\"; }\n",
        )
        .unwrap();
        let closed = build_report(root, &args_for_test()).unwrap();
        assert_eq!(closed.upward.as_ref().unwrap()[0].status, Some("closed"));

        fs::write(root.join("src/store.rs"), "pub fn broken( {\n").unwrap();
        let unparseable = build_report(root, &args_for_test()).unwrap();
        assert_eq!(
            unparseable.parse_failures.current,
            [PathBuf::from("src/store.rs")]
        );
    }

    fn args_for_test() -> Args {
        Args {
            base: "HEAD".to_owned(),
            path: PathBuf::from("src"),
            file: None,
            top: 20,
            markdown: false,
            json: false,
            no_index: true,
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

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
