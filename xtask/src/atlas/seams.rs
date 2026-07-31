use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::history::{self, CochangeEdge};
use super::modules::{crate_module_for_path, module_for_path, workspace_crate_names};
use super::sources;
use super::syntax;
use super::{positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const DEFAULT_TOP: usize = 15;

const USAGE: &str = "cargo xtask atlas seams [--path <prefix>] [--top N] [--module <name>] [--window <pct>] [--since <ref>] [--max-commit-files N] [--min-cochange N] [--json]

Imports come from Rust `use` items; inline fully-qualified paths are not counted.
The provider table ranks outside modules by distinct imported item names across
the scoped modules that use them. Co-change counts Rust source files and omits
commits touching more than --max-commit-files of them under --path. The `(root)`
module's pairwise co-change fanout folds into one annotated hub row.

  --path <path>          root-relative subtree (default crates/rimz/src)
  --top N                rows per section (default 15)
  --module <name>        list imported item names on one scoped module's edges
  --window <pct>         recent co-change history window (default 25)
  --since <ref>          restrict co-change to <ref>..HEAD (excludes --window)
  --max-commit-files N   omit commits broader than N Rust sources (default 10)
  --min-cochange N       co-change and divergence threshold (default 3)
  --json                 versioned JSON agent contract (v1)";

#[derive(Debug)]
struct Args {
    path: PathBuf,
    top: usize,
    module: Option<String>,
    window: usize,
    since: Option<String>,
    max_commit_files: usize,
    min_cochange: usize,
    json: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ImportEdge {
    from: String,
    to: String,
    items: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ImportItems {
    from: String,
    to: String,
    items: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ExternalSurface {
    module: String,
    outside: Vec<String>,
    items: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ExternalProvider {
    provider: String,
    modules: usize,
    items: usize,
}

#[derive(Clone, Debug, Serialize)]
struct CochangeHub {
    module: String,
    modules: usize,
}

#[derive(Clone, Debug, Serialize)]
struct Divergence {
    kind: &'static str,
    left: String,
    right: String,
    imports: usize,
    cochanges: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_module: Option<String>,
    history_commits: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_window_pct: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_since: Option<String>,
    total_import_edges: usize,
    import_edges: Vec<ImportEdge>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    import_items: Vec<ImportItems>,
    total_external_surface: usize,
    external_surface: Vec<ExternalSurface>,
    total_external_providers: usize,
    external_providers: Vec<ExternalProvider>,
    total_cochange_edges: usize,
    cochange_edges: Vec<CochangeEdge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cochange_hub: Option<CochangeHub>,
    total_divergence: usize,
    cochange_without_import: usize,
    import_without_cochange: usize,
    divergence: Vec<Divergence>,
    parse_failures: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas seams output is a command stdout contract"
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
            serde_json::to_string_pretty(&report).context("rendering atlas seams JSON")?
        );
    } else {
        print_report(&report, args.top);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut path = None;
    let mut top = None;
    let mut module = None;
    let mut window = None;
    let mut since = None;
    let mut max_commit_files = None;
    let mut min_cochange = None;
    let mut json = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--path" => {
                let parsed = validate_scope(value(args, index, "seams", "--path")?, "--path")?;
                set_once(&mut path, parsed, "seams", "--path")?;
                index += 2;
            }
            "--top" => {
                let parsed =
                    positive_usize(value(args, index, "seams", "--top")?, "seams", "--top")?;
                set_once(&mut top, parsed, "seams", "--top")?;
                index += 2;
            }
            "--module" => {
                let parsed = value(args, index, "seams", "--module")?.to_owned();
                set_once(&mut module, parsed, "seams", "--module")?;
                index += 2;
            }
            "--window" => {
                let parsed = positive_usize(
                    value(args, index, "seams", "--window")?,
                    "seams",
                    "--window",
                )?;
                if parsed > 100 {
                    bail!("atlas seams --window must not exceed 100");
                }
                set_once(&mut window, parsed, "seams", "--window")?;
                index += 2;
            }
            "--since" => {
                let reference = value(args, index, "seams", "--since")?.to_owned();
                set_once(&mut since, reference, "seams", "--since")?;
                index += 2;
            }
            "--max-commit-files" => {
                let parsed = positive_usize(
                    value(args, index, "seams", "--max-commit-files")?,
                    "seams",
                    "--max-commit-files",
                )?;
                set_once(&mut max_commit_files, parsed, "seams", "--max-commit-files")?;
                index += 2;
            }
            "--min-cochange" => {
                let parsed = positive_usize(
                    value(args, index, "seams", "--min-cochange")?,
                    "seams",
                    "--min-cochange",
                )?;
                set_once(&mut min_cochange, parsed, "seams", "--min-cochange")?;
                index += 2;
            }
            "--json" if !json => {
                json = true;
                index += 1;
            }
            "--json" => bail!("atlas seams --json may only be passed once"),
            _ => bail!("unknown atlas seams argument `{arg}`"),
        }
    }
    if since.is_some() && window.is_some() {
        bail!("atlas seams --since and --window are mutually exclusive");
    }
    Ok(Some(Args {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        top: top.unwrap_or(DEFAULT_TOP),
        module,
        window: window.unwrap_or(25),
        since,
        max_commit_files: max_commit_files.unwrap_or(10),
        min_cochange: min_cochange.unwrap_or(3),
        json,
    }))
}

fn build_report(root: &Path, args: &Args) -> Result<Report> {
    let all_sources = sources::all_sources(root, None)?;
    let scoped_sources = sources::sources_in_scope(&all_sources, &args.path)?;
    let syntax = syntax::analyze_sources(&scoped_sources);
    let known_modules = all_sources
        .into_iter()
        .filter(|source| source.is_production())
        .map(|source| crate_module_for_path(&source.path))
        .collect::<BTreeSet<_>>();
    let workspace_crates = workspace_crate_names(root)?;
    let scope_module = crate_module_for_path(&args.path.join("mod.rs"));
    let mut scope_endpoints = scoped_sources
        .iter()
        .filter(|source| source.is_production())
        .map(|source| module_for_path(&source.path, &args.path))
        .collect::<BTreeSet<_>>();
    scope_endpoints.insert("(root)".to_owned());
    if let Some(requested) = &args.module
        && !scope_endpoints.contains(requested)
    {
        bail!(
            "atlas seams --module `{requested}` is not in the scoped module set; choose a module from the import or surface tables"
        );
    }
    let mut imports = BTreeMap::<(String, String), BTreeSet<String>>::new();
    for file in &syntax.files {
        let from = module_for_path(&file.path, &args.path);
        for imported in &file.imports {
            let Some(imported_module) =
                syntax::resolved_internal_import(imported, &known_modules, &workspace_crates)
            else {
                continue;
            };
            let to = endpoint(&imported_module, &scope_module);
            if from != to {
                imports
                    .entry((from.clone(), to))
                    .or_default()
                    .insert(imported.item.clone());
            }
        }
    }
    let mut import_edges = imports
        .iter()
        .map(|((from, to), items)| ImportEdge {
            from: from.clone(),
            to: to.clone(),
            items: items.len(),
        })
        .collect::<Vec<_>>();
    import_edges.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| left.from.cmp(&right.from))
            .then_with(|| left.to.cmp(&right.to))
    });
    let import_items = args.module.as_ref().map_or_else(Vec::new, |requested| {
        imports
            .iter()
            .filter(|((from, to), _)| from == requested || to == requested)
            .map(|((from, to), items)| ImportItems {
                from: from.clone(),
                to: to.clone(),
                items: items.iter().cloned().collect(),
            })
            .collect()
    });
    let mut surfaces = BTreeMap::<String, (BTreeSet<String>, BTreeSet<(String, String)>)>::new();
    for ((from, to), items) in &imports {
        if scope_endpoints.contains(to) {
            continue;
        }
        let surface = surfaces.entry(from.clone()).or_default();
        surface.0.insert(to.clone());
        surface
            .1
            .extend(items.iter().map(|item| (to.clone(), item.clone())));
    }
    let mut external_surface = surfaces
        .into_iter()
        .map(|(module, (outside, items))| ExternalSurface {
            module,
            outside: outside.into_iter().collect(),
            items: items.len(),
        })
        .collect::<Vec<_>>();
    external_surface.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| left.module.cmp(&right.module))
    });
    let mut external_providers = external_providers(&imports, &scope_endpoints);

    let cochange = history::cochange(
        root,
        &args.path,
        args.since.as_deref(),
        args.window,
        args.max_commit_files,
    )?;
    let cochange_edges = cochange.edges;
    let import_lookup = import_edges
        .iter()
        .map(|edge| ((edge.from.clone(), edge.to.clone()), edge.items))
        .collect::<BTreeMap<_, _>>();
    let cochange_lookup = cochange_edges
        .iter()
        .map(|edge| (ordered_pair(&edge.left, &edge.right), edge.commits))
        .collect::<BTreeMap<_, _>>();
    let (cochange_edges, cochange_hub) = fold_root_cochange_hub(cochange_edges);
    let (mut cochange_edges, import_free_cochange_edges) =
        partition_cochange_edges(cochange_edges, &import_lookup, args.min_cochange);
    let mut divergence = Vec::new();
    for edge in import_free_cochange_edges {
        divergence.push(Divergence {
            kind: "cochange-without-import",
            left: edge.left,
            right: edge.right,
            imports: 0,
            cochanges: edge.commits,
        });
    }
    for edge in &import_edges {
        let cochanges = cochange_lookup
            .get(&ordered_pair(&edge.from, &edge.to))
            .copied()
            .unwrap_or(0);
        if cochanges == 0 {
            divergence.push(Divergence {
                kind: "import-without-cochange",
                left: edge.from.clone(),
                right: edge.to.clone(),
                imports: edge.items,
                cochanges: 0,
            });
        }
    }
    divergence.sort_by(|left, right| {
        right
            .cochanges
            .cmp(&left.cochanges)
            .then_with(|| right.imports.cmp(&left.imports))
            .then_with(|| left.left.cmp(&right.left))
            .then_with(|| left.right.cmp(&right.right))
    });
    let cochange_without_import = divergence
        .iter()
        .filter(|row| row.kind == "cochange-without-import")
        .count();
    let import_without_cochange = divergence.len() - cochange_without_import;
    let total_import_edges = import_edges.len();
    let total_external_surface = external_surface.len();
    let total_external_providers = external_providers.len();
    let total_cochange_edges = cochange_edges.len();
    let total_divergence = divergence.len();
    import_edges.truncate(args.top);
    external_surface.truncate(args.top);
    external_providers.truncate(args.top);
    cochange_edges.truncate(args.top);
    divergence.truncate(args.top);
    Ok(Report {
        version: 1,
        verb: "seams",
        path: args.path.clone(),
        requested_module: args.module.clone(),
        history_commits: cochange.commits,
        history_window_pct: args.since.is_none().then_some(args.window),
        history_since: args.since.clone(),
        total_import_edges,
        import_edges,
        import_items,
        total_external_surface,
        external_surface,
        total_external_providers,
        external_providers,
        total_cochange_edges,
        cochange_edges,
        cochange_hub,
        total_divergence,
        cochange_without_import,
        import_without_cochange,
        divergence,
        parse_failures: syntax.parse_failures.len(),
    })
}

fn fold_root_cochange_hub(
    mut edges: Vec<CochangeEdge>,
) -> (Vec<CochangeEdge>, Option<CochangeHub>) {
    let partners = edges
        .iter()
        .filter_map(|edge| match (edge.left.as_str(), edge.right.as_str()) {
            ("(root)", module) | (module, "(root)") => Some(module.to_owned()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let hub = (partners.len() >= 3).then(|| CochangeHub {
        module: "(root)".to_owned(),
        modules: partners.len(),
    });
    if hub.is_some() {
        edges.retain(|edge| edge.left != "(root)" && edge.right != "(root)");
    }
    (edges, hub)
}

fn external_providers(
    imports: &BTreeMap<(String, String), BTreeSet<String>>,
    scope_endpoints: &BTreeSet<String>,
) -> Vec<ExternalProvider> {
    let mut providers = BTreeMap::<String, (BTreeSet<String>, BTreeSet<String>)>::new();
    for ((from, to), items) in imports {
        if scope_endpoints.contains(to) {
            continue;
        }
        let provider = providers.entry(to.clone()).or_default();
        provider.0.insert(from.clone());
        provider.1.extend(items.iter().cloned());
    }
    let mut providers = providers
        .into_iter()
        .map(|(provider, (modules, items))| ExternalProvider {
            provider,
            modules: modules.len(),
            items: items.len(),
        })
        .collect::<Vec<_>>();
    providers.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| right.modules.cmp(&left.modules))
            .then_with(|| left.provider.cmp(&right.provider))
    });
    providers
}

fn partition_cochange_edges(
    edges: Vec<CochangeEdge>,
    imports: &BTreeMap<(String, String), usize>,
    min_cochange: usize,
) -> (Vec<CochangeEdge>, Vec<CochangeEdge>) {
    edges
        .into_iter()
        .filter(|edge| edge.commits >= min_cochange)
        .partition(|edge| {
            imports.contains_key(&(edge.left.clone(), edge.right.clone()))
                || imports.contains_key(&(edge.right.clone(), edge.left.clone()))
        })
}

fn endpoint(module_path: &str, scope_module: &str) -> String {
    if scope_module.is_empty() {
        return if module_path == "(crate)" {
            "(root)".to_owned()
        } else {
            module_path
                .split("::")
                .next()
                .unwrap_or("(root)")
                .to_owned()
        };
    }
    if module_path == scope_module {
        return "(root)".to_owned();
    }
    if let Some(relative) = module_path.strip_prefix(&format!("{scope_module}::")) {
        return relative.split("::").next().unwrap_or("(root)").to_owned();
    }
    module_path
        .split("::")
        .next()
        .unwrap_or("(root)")
        .to_owned()
}

fn ordered_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas seams report is the command's stdout contract"
)]
fn print_report(report: &Report, top: usize) {
    println!("Atlas seams — {}", report.path.display());
    println!();
    println!("Import edges (distinct imported items)");
    for edge in report.import_edges.iter().take(top) {
        println!("{} -> {}: {}", edge.from, edge.to, edge.items);
    }
    bounded_tail(
        report.total_import_edges,
        report.import_edges.len(),
        "import edges",
    );
    if let Some(module) = &report.requested_module {
        println!();
        println!("Imported items on {module} edges");
        for edge in &report.import_items {
            println!("{} -> {}: {}", edge.from, edge.to, edge.items.join(", "));
        }
    }
    println!();
    println!("External surface");
    for surface in report.external_surface.iter().take(top) {
        println!(
            "{} -> {}: {} items",
            surface.module,
            surface.outside.join(", "),
            surface.items
        );
    }
    bounded_tail(
        report.total_external_surface,
        report.external_surface.len(),
        "surface rows",
    );
    println!();
    println!("External providers");
    for provider in report.external_providers.iter().take(top) {
        println!(
            "{} <- {} modules, {} items",
            provider.provider, provider.modules, provider.items
        );
    }
    bounded_tail(
        report.total_external_providers,
        report.external_providers.len(),
        "provider rows",
    );
    println!();
    println!("Co-change edges (pairs with import edges)");
    if let Some(hub) = &report.cochange_hub {
        println!(
            "{}: co-changes with {} modules (hub)",
            hub.module, hub.modules
        );
    }
    for edge in report.cochange_edges.iter().take(top) {
        println!("{} <> {}: {} commits", edge.left, edge.right, edge.commits);
    }
    bounded_tail(
        report.total_cochange_edges,
        report.cochange_edges.len(),
        "co-change edges",
    );
    println!();
    println!("Divergence");
    for row in report.divergence.iter().take(top) {
        println!(
            "{} {} <> {}: imports {}, cochanges {}",
            row.kind, row.left, row.right, row.imports, row.cochanges
        );
    }
    bounded_tail(
        report.total_divergence,
        report.divergence.len(),
        "divergence rows",
    );
    if let Some(since) = &report.history_since {
        println!(
            "history: {} commits (since {since})",
            report.history_commits
        );
    } else {
        // build_report records a window exactly when no explicit since bound exists.
        let window = report
            .history_window_pct
            .expect("a window-bounded report records its percentage");
        println!(
            "history: {} commits (window {}%)",
            report.history_commits, window
        );
    }
    println!(
        "total: {} import edges, {} provider rows, {} co-change hubs, {} co-change edges, {} divergence rows ({} cochange-without-import, {} import-without-cochange), {} parse failures",
        report.total_import_edges,
        report.total_external_providers,
        usize::from(report.cochange_hub.is_some()),
        report.total_cochange_edges,
        report.total_divergence,
        report.cochange_without_import,
        report.import_without_cochange,
        report.parse_failures
    );
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas bounded report is the command's stdout contract"
)]
fn bounded_tail(total: usize, shown: usize, label: &str) {
    if total > shown {
        println!("… and {} more {label}", total - shown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_follow_scope_granularity() {
        assert_eq!(endpoint("cli::agents_cmd::show", "cli"), "agents_cmd");
        assert_eq!(endpoint("store::event", "cli"), "store");
    }

    #[test]
    fn cochange_history_bounds_are_validated() {
        assert!(parse_args(&["--window".into(), "101".into()]).is_err());
        assert!(
            parse_args(&[
                "--window".into(),
                "25".into(),
                "--since".into(),
                "main".into(),
            ])
            .is_err()
        );
        assert_eq!(parse_args(&[]).unwrap().unwrap().window, 25);
        assert_eq!(
            parse_args(&["--module".into(), "agents_cmd".into()])
                .unwrap()
                .unwrap()
                .module
                .as_deref(),
            Some("agents_cmd")
        );
        assert_eq!(
            parse_args(&["--since".into(), "main".into()])
                .unwrap()
                .unwrap()
                .since
                .as_deref(),
            Some("main")
        );
    }

    #[test]
    fn cochange_sections_partition_pairs_by_import_presence() {
        let edges = vec![
            CochangeEdge {
                left: "agents".to_owned(),
                right: "hooks".to_owned(),
                commits: 40,
            },
            CochangeEdge {
                left: "agents".to_owned(),
                right: "store".to_owned(),
                commits: 20,
            },
            CochangeEdge {
                left: "hooks".to_owned(),
                right: "store".to_owned(),
                commits: 2,
            },
        ];
        let imports = BTreeMap::from([
            (("store".to_owned(), "agents".to_owned()), 2),
            (("store".to_owned(), "hooks".to_owned()), 1),
        ]);

        let (with_imports, without_imports) = partition_cochange_edges(edges, &imports, 3);

        assert_eq!(
            with_imports
                .iter()
                .map(|edge| ordered_pair(&edge.left, &edge.right))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([("agents".to_owned(), "store".to_owned())])
        );
        assert_eq!(
            without_imports
                .iter()
                .map(|edge| ordered_pair(&edge.left, &edge.right))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([("agents".to_owned(), "hooks".to_owned())])
        );
    }

    #[test]
    fn root_dispatcher_cochanges_fold_to_one_hub() {
        let edges = ["agents", "hooks", "room", "store"]
            .into_iter()
            .map(|module| CochangeEdge {
                left: "(root)".to_owned(),
                right: module.to_owned(),
                commits: 2,
            })
            .chain(std::iter::once(CochangeEdge {
                left: "agents".to_owned(),
                right: "store".to_owned(),
                commits: 1,
            }))
            .collect();
        let (edges, hub) = fold_root_cochange_hub(edges);

        let hub = hub.unwrap();
        assert_eq!(hub.module, "(root)");
        assert_eq!(hub.modules, 4);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (&edges[0].left, &edges[0].right),
            (&"agents".into(), &"store".into())
        );
    }

    #[test]
    fn domain_hubs_remain_visible_as_divergence_candidates() {
        let edges = ["hooks", "room", "store", "supervised"]
            .into_iter()
            .map(|module| CochangeEdge {
                left: "agents".to_owned(),
                right: module.to_owned(),
                commits: 2,
            })
            .collect();

        let (edges, hub) = fold_root_cochange_hub(edges);

        assert!(hub.is_none());
        assert_eq!(edges.len(), 4);
    }

    #[test]
    fn external_providers_union_modules_and_item_names() {
        let imports = BTreeMap::from([
            (
                ("agents".to_owned(), "harness".to_owned()),
                BTreeSet::from(["launch".to_owned(), "resume".to_owned()]),
            ),
            (
                ("hooks".to_owned(), "harness".to_owned()),
                BTreeSet::from(["launch".to_owned(), "stop".to_owned()]),
            ),
            (
                ("hooks".to_owned(), "store".to_owned()),
                BTreeSet::from(["open".to_owned()]),
            ),
            (
                ("agents".to_owned(), "hooks".to_owned()),
                BTreeSet::from(["install".to_owned()]),
            ),
        ]);
        let providers = external_providers(&imports, &BTreeSet::from(["hooks".to_owned()]));

        assert_eq!(providers[0].provider, "harness");
        assert_eq!(providers[0].modules, 2);
        assert_eq!(providers[0].items, 3);
        assert_eq!(providers[1].provider, "store");
    }
}
